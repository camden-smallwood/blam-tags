//! Which managed-array attribute types do CE's geometry collections actually use?
//!
//! `UGeometryCollection` serializes an `FManagedArrayCollection` — a generic
//! container of named, typed arrays — so the cost of decoding it is set entirely
//! by how many of the 49 `EManagedArrayType` variants appear in practice. Ten of
//! them bulk-serialize (an element size *and* a count, so they are
//! self-describing and can be skipped without knowing the type); the rest are a
//! bare count and need per-type knowledge.
//!
//! This walks the collection header and every attribute it can, and reports the
//! first type it cannot size — so the remaining work is a known list rather than
//! an open-ended one.
//!
//! Run: ce_geocoll_types
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// `EManagedArrayType`, in the order `ManagedArrayTypeValues.inl` declares them
/// (`FNoneType` is 0, so the first entry below is 1).
const TYPES: &[&str] = &[
    "None", "Vector", "IntVector", "Vector2D", "LinearColor", "Int32", "Bool", "Transform",
    "String", "Float", "Quat", "BoneNode", "MeshSection", "Box", "IntArray", "Guid", "UInt8",
    "VectorArrayPointer", "VectorArrayUniquePointer", "ImplicitObject3Pointer",
    "ImplicitObject3UniquePointer", "ImplicitObject3SerializablePtr", "BVHParticlesFloat3Pointer",
    "BVHParticlesFloat3UniquePointer", "PBDRigidParticleHandle3fPtr",
    "PBDGeometryCollectionParticleHandle3fPtr", "GeometryParticle3fUniquePtr",
    "ImplicitObject3ThreadSafeSharedPointer", "ImplicitObject3SharedPointer",
    "PBDRigidClusteredParticleHandle3fPtr", "ConvexUniquePtr", "Vector2DArray", "Double",
    "IntVector4", "Vector3d", "IntVector2", "IntVector2Array", "Int32Array", "FloatArray",
    "Vector4f", "VectorArray", "PBDRigidParticle3fUniquePtr", "ImplicitObjectRefCountedPtr",
    "ConvexRefCountedPtr", "Transform3f", "IntVector3Array", "Vector4fArray", "PMatrix33d",
    "PMatrix33dArray", "Vector3fNestedArray",
];

/// Non-bulk types whose element is a fixed size on disk, so `Ar << TArray<T>`
/// is a bare count followed by `count * size` bytes.
fn fixed_size(t: usize) -> Option<usize> {
    Some(match TYPES.get(t).copied()? {
        // FQuat4f + FVector3f + FVector3f
        "Transform3f" => 40,
        // LWC doubles: FQuat4d + FVector3d + FVector3d
        "Transform" => 80,
        "LinearColor" | "Vector4f" => 16,
        "Vector3d" => 24,
        "Double" => 8,
        // FBox: two FVector3d and the IsValid byte
        "Box" => 49,
        // FGeometryCollectionSection: five int32
        "MeshSection" => 20,
        "PMatrix33d" => 72,
        _ => return None,
    })
}

/// Types whose element is itself an array of fixed-size items: an outer count,
/// then per entry an inner count and that many elements.
fn nested_elem(t: usize) -> Option<usize> {
    Some(match TYPES.get(t).copied()? {
        "Int32Array" | "FloatArray" | "IntArray" => 4,
        "Vector2DArray" | "IntVector2Array" => 8,
        "IntVector3Array" | "VectorArray" => 12,
        "Vector4fArray" => 16,
        "PMatrix33dArray" => 72,
        _ => return None,
    })
}

/// The types with a `TryBulkSerializeManagedArray` overload: their payload
/// carries its own element size, so it can be skipped blind.
fn is_bulk(t: usize) -> bool {
    matches!(TYPES.get(t).copied(), Some("Vector" | "IntVector" | "Vector2D" | "Int32" | "Bool" | "Float" | "Quat" | "Guid" | "UInt8" | "IntVector2"))
}

struct R<'a> {
    b: &'a [u8],
    o: usize,
}
impl<'a> R<'a> {
    fn i32(&mut self) -> Option<i32> {
        let s = self.b.get(self.o..self.o + 4)?;
        self.o += 4;
        Some(i32::from_le_bytes(s.try_into().unwrap()))
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        (self.o + n <= self.b.len()).then(|| self.o += n)
    }
}

/// `FChaosArchive::SerializePtr` (ChaosArchive.h:176) — the object-graph form
/// every Chaos smart pointer goes through: a 4-byte `bExists`, and when set an
/// `int32 Tag`. A tag already seen in this archive is a back-reference and
/// carries **no payload**; only the first sighting is followed by the object.
/// Returns `Some(true)` when the caller must now read the object itself.
fn chaos_ptr(r: &mut R, seen: &mut std::collections::HashSet<i32>) -> Option<bool> {
    if r.i32()? == 0 {
        return Some(false);
    }
    let tag = r.i32()?;
    Some(seen.insert(tag))
}

/// `FBVHParticles::Serialize` (BVHParticles.cpp:62) = `FParticles::Serialize`
/// (a 4-byte `bSerialize` then the `MX` position array) followed by the
/// bounding-volume hierarchy (BoundingVolumeHierarchy.cpp:696). `T` is
/// `Chaos::FReal` = double throughout the BVH even though the particles
/// themselves are `FVector3f`.
fn read_bvh_particles(r: &mut R) -> Option<()> {
    let t = std::env::var("BLAM_GC_TRACE").is_ok();
    if t { eprintln!("   bvh @ {}", r.o); }
    if r.i32()? == 0 {
        return Some(()); // bSerialize false writes nothing more
    }
    let mx = r.i32()?;
    if t { eprintln!("   mx {mx} @ {}", r.o); }
    r.skip(mx.max(0) as usize * 12)?; // FVector3f positions
    let globals = r.i32()?;
    if t { eprintln!("   globals {globals} @ {}", r.o); }
    r.skip(globals.max(0) as usize * 4)?; // MGlobalObjects: TArray<int32>
    let aabbs = r.i32()?;
    if t {
        eprintln!("   aabbs {aabbs} @ {}", r.o);
        for (i, ch) in r.b[r.o..(r.o + 128).min(r.b.len())].chunks(16).enumerate() {
            eprint!("     {:06x}: ", r.o + i * 16);
            for x in ch { eprint!("{x:02x} "); }
            eprintln!();
        }
    }
    // `MWorldSpaceBoxes` is a **TMap<int32, TAABB<T,3>>**, not an array — the
    // second `SerializeAsAABBs` overload (Box.h:528). A bare `Ar << TMap` uses
    // TMap's own operator (a count then key/value pairs), not FMapProperty's
    // delta form, so each entry is the int32 key plus the box.
    // Measured: 28 bytes per entry, and the box bytes are byte-identical copies
    // of the particle position — so `T` is single-precision here and each box is
    // a degenerate point box. `TAABB::Serialize` is just `Ar << MMin << MMax`.
    r.skip(aabbs.max(0) as usize * (4 + 24))?; // key + TAABB<float,3>
    let lv = r.i32()?; // MMaxLevels
    let nodes = r.i32()?;
    if t { eprintln!("   maxlevels {lv} nodes {nodes} @ {}", r.o); }
    for _ in 0..nodes.max(0) {
        // `operator<<(TBVHNode)` writes LeafIndex, MAxis, MChildren, MMax, MMin
        // — that order, not declaration order.
        r.skip(8)?; // LeafIndex, MAxis
        let children = r.i32()?;
        r.skip(children.max(0) as usize * 4)?;
        r.skip(24)?; // MMax, MMin — TVector<float,3> each
    }
    let leafs = r.i32()?;
    for _ in 0..leafs.max(0) {
        let n = r.i32()?;
        r.skip(n.max(0) as usize * 4)?;
    }
    Some(())
}

/// `FImplicitObject::SerializationFactory` (ImplicitObject.cpp:406) dispatches on
/// an `int8` type byte, then the object serializes itself. `FImplicitObject::SerializeImp`
/// is the shared prefix: `bIsConvex` and `bDoCollide` (four bytes each, being
/// `FArchive` bools) then a one-byte `CollisionType`.
///
/// Returns the type byte so the caller can census what CE actually ships.
fn read_implicit_object(r: &mut R, types: &mut BTreeMap<i32, usize>) -> Option<()> {
    let ty = *r.b.get(r.o)? as i8 as i32;
    r.skip(1)?;
    *types.entry(ty).or_default() += 1;
    r.skip(9)?; // bIsConvex, bDoCollide, CollisionType
    match ty {
        // TSphere: Center then a single-precision radius (the radius lives in
        // the base class Margin but is written as FRealSingle).
        0 => r.skip(16)?,
        // FConvex::SerializeImp (Convex.h:890).
        8 => {
            let t = std::env::var("BLAM_GC_TRACE").is_ok();
            let planes = r.i32()?;
            if t { eprintln!("   convex planes {planes} @ {}", r.o); }
            r.skip(planes.max(0) as usize * 24)?; // TPlaneConcrete: MX + MNormal
            let verts = r.i32()?;
            if t { eprintln!("   convex verts {verts} @ {}", r.o); }
            r.skip(verts.max(0) as usize * 12)?;
            if t {
                for (i, ch) in r.b[r.o..(r.o + 80).min(r.b.len())].chunks(16).enumerate() {
                    eprint!("     {:06x}: ", r.o + i * 16);
                    for x in ch { eprint!("{x:02x} "); }
                    eprintln!();
                }
            }
            r.skip(24)?; // LocalBoundingBox: TAABB (MMin, MMax)
            r.skip(4)?; // Volume, as FRealSingle
            r.skip(12)?; // CenterOfMass
            r.skip(4)?; // Margin, as FRealSingle
            read_convex_structure_data(r)?;
            if t {
                eprintln!("   convex post-structure @ {}", r.o);
                for (i, ch) in r.b[r.o..(r.o + 64).min(r.b.len())].chunks(16).enumerate() {
                    eprint!("     {:06x}: ", r.o + i * 16);
                    for x in ch { eprint!("{x:02x} "); }
                    eprintln!();
                }
            }
            // Measured: the inertia is a float3 (values read as a plausible
            // (335, 118, 282)) but the rotation is a **double** quaternion —
            // its four doubles have norm 1.0000. Mixed precision in one struct.
            r.skip(12 + 32)?; // UnitMassInertiaTensor (float3), RotationOfMass (FQuat4d)
        }
        _ => return None,
    }
    Some(())
}

/// `FConvexStructureData::Serialize` (ConvexStructureData.h:253): an `int8`
/// index type, then the half-edge tables at that index width.
/// `TConvexHalfEdgeStructureData::Serialize` (ConvexHalfEdgeStructureData.h:556)
/// writes planes (2 indices each), half-edges (3), vertices (1) and the unique
/// edge list (1).
fn read_convex_structure_data(r: &mut R) -> Option<()> {
    let w = match *r.b.get(r.o)? as i8 {
        0 => {
            r.skip(1)?;
            return Some(()); // None: no container
        }
        1 => 1, // Small:  uint8
        2 => 2, // Medium: int16
        3 => 4, // Large:  int32
        _ => return None,
    };
    r.skip(1)?;
    for per in [2, 3, 1, 1] {
        let n = r.i32()?;
        r.skip(n.max(0) as usize * per * w)?;
    }
    Some(())
}

fn main() {
    let idx = FPackageObjectIndex::create_script_import(
        "/Script/GeometryCollectionEngine.GeometryCollection",
    );
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocked: BTreeMap<String, usize> = BTreeMap::new();
    let mut collections = 0;
    let mut implicit_types: BTreeMap<i32, usize> = BTreeMap::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = h.export_map.iter().find(|x| x.class_index == idx) else { continue };
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(b.len());
            if off >= end {
                continue;
            }
            let body = &b[off..end];
            collections += 1;

            // The collection begins after the property block and object trailer.
            // Anchor on the `bIsCookedOrCooking` flag followed by a plausible
            // collection Version and group count rather than re-deriving the
            // property block here.
            let Some(start) = (0..body.len().saturating_sub(12)).find(|&i| {
                let v = |k: usize| i32::from_le_bytes(body[i + k..i + k + 4].try_into().unwrap());
                v(0) == 1 && (1..=20).contains(&v(4)) && (1..=64).contains(&v(8))
            }) else {
                println!("{}: no collection anchor", h.package_name());
                continue;
            };
            let mut r = R { b: body, o: start + 4 };
            let mut chaos_tags = std::collections::HashSet::new();
            let _version = r.i32();
            let Some(groups) = r.i32() else { continue };
            // Group table: FName key, then FGroupInfo (its own version + size).
            for _ in 0..groups {
                if r.skip(16).is_none() {
                    break;
                }
            }
            let Some(attrs) = r.i32() else { continue };
            for _ in 0..attrs.max(0) {
                // Key: attribute FName + group FName.
                if r.skip(16).is_none() {
                    break;
                }
                let (Some(_ver), Some(ty)) = (r.i32(), r.i32()) else { break };
                // GroupIndexDependency FName + bPersistent.
                if r.skip(12).is_none() {
                    break;
                }
                let name = TYPES.get(ty as usize).copied().unwrap_or("??").to_string();
                *seen.entry(name.clone()).or_default() += 1;
                let Some(_array_version) = r.i32() else { break };
                if is_bulk(ty as usize) {
                    let (Some(elem), Some(n)) = (r.i32(), r.i32()) else { break };
                    if elem < 0 || n < 0 || r.skip(elem as usize * n as usize).is_none() {
                        *blocked.entry(format!("{name} (bad bulk {elem}x{n})")).or_default() += 1;
                        break;
                    }
                } else if TYPES.get(ty as usize) == Some(&"String") {
                    // TArray<FString>: a count, then each string's own length.
                    let Some(n) = r.i32() else { break };
                    let mut bad = false;
                    for _ in 0..n.max(0) {
                        let Some(len) = r.i32() else { bad = true; break };
                        let bytes =
                            if len >= 0 { len as usize } else { (-(len as i64) as usize) * 2 };
                        if r.skip(bytes).is_none() {
                            bad = true;
                            break;
                        }
                    }
                    if bad {
                        *blocked.entry(format!("{name} (bad strings)")).or_default() += 1;
                        break;
                    }
                } else if let Some(inner) = nested_elem(ty as usize) {
                    // TArray<TArray<T>>: an outer count, then each inner array's
                    // own count and fixed-size elements.
                    let Some(n) = r.i32() else { break };
                    let mut bad = false;
                    for _ in 0..n.max(0) {
                        let Some(m) = r.i32() else { bad = true; break };
                        if m < 0 || r.skip(inner * m as usize).is_none() {
                            bad = true;
                            break;
                        }
                    }
                    if bad {
                        *blocked.entry(format!("{name} (bad nested)")).or_default() += 1;
                        break;
                    }
                } else if matches!(TYPES.get(ty as usize), Some(&"ImplicitObjectRefCountedPtr" | &"ConvexRefCountedPtr")) {
                    // TArray<FImplicitObjectPtr>: a bare count, then each
                    // ref-counted pointer through the Chaos object-graph form.
                    let Some(n) = r.i32() else { break };
                    let mut bad = false;
                    for _ in 0..n.max(0) {
                        match chaos_ptr(&mut r, &mut chaos_tags) {
                            Some(true) => {
                                if read_implicit_object(&mut r, &mut implicit_types).is_none() {
                                    bad = true;
                                    break;
                                }
                            }
                            Some(false) => {}
                            None => {
                                bad = true;
                                break;
                            }
                        }
                    }
                    if bad {
                        *blocked.entry(format!("{name} (bad implicit)")).or_default() += 1;
                        break;
                    }
                } else if TYPES.get(ty as usize) == Some(&"BVHParticlesFloat3UniquePointer") {
                    // TArray<TUniquePtr<FBVHParticles>> has no bulk overload, so
                    // it is a bare count and then each pointer via the Chaos
                    // object-graph form.
                    let Some(n) = r.i32() else { break };
                    let mut bad = false;
                    for _ in 0..n.max(0) {
                        match chaos_ptr(&mut r, &mut chaos_tags) {
                            Some(true) => {
                                if read_bvh_particles(&mut r).is_none() {
                                    bad = true;
                                    break;
                                }
                            }
                            Some(false) => {}
                            None => {
                                bad = true;
                                break;
                            }
                        }
                    }
                    if bad {
                        *blocked.entry(format!("{name} (bad bvh)")).or_default() += 1;
                        break;
                    }
                } else if let Some(sz) = fixed_size(ty as usize) {
                    let Some(n) = r.i32() else { break };
                    if n < 0 || r.skip(sz * n as usize).is_none() {
                        *blocked.entry(format!("{name} (bad count {n})")).or_default() += 1;
                        break;
                    }
                } else {
                    // BLAM_GC_DUMP=<TypeName>: show the bytes an unmodeled
                    // array type starts with, so its serializer can be read off
                    // rather than guessed at.
                    if std::env::var("BLAM_GC_DUMP").is_ok_and(|v| v == name) {
                        println!("\n--- {} :: {name} @ {}", h.package_name(), r.o);
                        for (i, ch) in body[r.o..(r.o + 160).min(body.len())].chunks(16).enumerate() {
                            print!("  {:04x}: ", r.o + i * 16);
                            for x in ch {
                                print!("{x:02x} ");
                            }
                            println!();
                        }
                    }
                    *blocked.entry(name).or_default() += 1;
                    break;
                }
            }
        }
    }
    println!("{collections} geometry collections\n");
    println!("attribute types reached:");
    for (k, v) in &seen {
        println!("  {v:5}  {k}");
    }
    println!("\nimplicit-object type bytes seen:");
    for (k, v) in &implicit_types {
        println!("  {v:5}  type {k}");
    }
    println!("\nfirst type that blocked each walk:");
    for (k, v) in &blocked {
        println!("  {v:5}  {k}");
    }
}
