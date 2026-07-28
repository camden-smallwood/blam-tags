//! Forward-parse a cooked UE5 `USkeletalMesh` from the anchored
//! `FReferenceSkeleton` through the LOD render data. Tracing probe — prints
//! every field so the exact byte layout can be validated against real data
//! before porting into a library reader.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_mesh_parse -- \
//!     "<paks>" [uasset-suffix=sk_elite_common_body.uasset]

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

struct R<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> R<'a> {
    fn new(b: &'a [u8], p: usize) -> Self {
        Self { b, p }
    }
    fn u8(&mut self) -> u8 {
        let v = self.b[self.p];
        self.p += 1;
        v
    }
    fn i16(&mut self) -> i16 {
        let v = i16::from_le_bytes(self.b[self.p..self.p + 2].try_into().unwrap());
        self.p += 2;
        v
    }
    fn i32(&mut self) -> i32 {
        let v = i32::from_le_bytes(self.b[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        v
    }
    fn u32(&mut self) -> u32 {
        self.i32() as u32
    }
    fn boolean(&mut self) -> bool {
        // FArchive serializes bool as int32.
        self.i32() != 0
    }
    fn skip(&mut self, n: usize) {
        self.p += n;
    }
    fn peek_i32(&self, off: usize) -> i32 {
        i32::from_le_bytes(self.b[self.p + off..self.p + off + 4].try_into().unwrap())
    }
}

fn find_ref_skeleton(body: &[u8], names_len: usize) -> Option<usize> {
    let i32_at = |o: usize| -> Option<i32> {
        body.get(o..o + 4).map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let mut o = 0;
    while o + 8 < body.len() {
        if let Some(n) = i32_at(o) {
            if (8..=512).contains(&n) {
                let n = n as usize;
                let ok = (0..n).all(|i| {
                    let e = o + 4 + i * 12;
                    match (i32_at(e), i32_at(e + 8)) {
                        (Some(ni), Some(par)) => {
                            ni >= 0
                                && (ni as usize) < names_len
                                && if i == 0 { par == -1 } else { par >= 0 && (par as usize) < n }
                        }
                        _ => false,
                    }
                });
                if ok {
                    return Some(o);
                }
            }
        }
        o += 1;
    }
    None
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let suffix = args
        .next()
        .unwrap_or_else(|| "sk_elite_common_body.uasset".to_string())
        .to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut bytes = None;
    'o: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&suffix) {
                bytes = a.read(&e.path).ok();
                break 'o;
            }
        }
    }
    let bytes = bytes.expect("uasset not found");
    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        .expect("zen header");
    let names = hdr.name_map.copy_raw_names();
    let hstart = hdr.summary.header_size as usize;
    let body = &bytes[hstart..];
    println!("package {}  names={}  body={}B", hdr.package_name(), names.len(), body.len());

    let anchor = find_ref_skeleton(body, names.len()).expect("no ref skeleton anchor");
    let mut r = R::new(body, anchor);

    // --- FReferenceSkeleton ---
    let nbones = r.i32() as usize;
    println!("\nFMeshBoneInfo[{nbones}] @export+{anchor}");
    r.skip(nbones * 12); // FName(8) + i32 parent(4)

    let refpose_n = r.i32();
    // auto-detect FTransform size (LWC double=80 vs float=40) by which
    // makes the following map-count read back nbones.
    let tsize = [80usize, 40]
        .into_iter()
        .find(|&ts| r.peek_i32(nbones * ts) as usize == nbones)
        .expect("could not determine FTransform size");
    println!("FTransform refpose count={refpose_n} (transform size={tsize}B → {})",
        if tsize == 80 { "LWC double" } else { "float" });
    r.skip(nbones * tsize);

    let map_n = r.i32() as usize;
    println!("FinalNameToIndexMap count={map_n} (×12B)");
    r.skip(map_n * 12); // FName(8)+i32(4)

    // --- back in USkeletalMesh::Serialize ---
    let b_cooked = r.boolean();
    println!("bCooked={b_cooked}  (pos export+{})", r.p);
    let num_lods = r.i32();
    println!("NumLODs={num_lods}");
    if !(1..=12).contains(&num_lods) {
        println!("  !! NumLODs implausible — layout drift before here; bytes: {:02x?}",
            &body[r.p.min(body.len())..(r.p + 32).min(body.len())]);
        return;
    }

    // --- LOD 0 : FSkeletalMeshLODRenderData::SerializeRenderItem ---
    let g = r.u8();
    let c = r.u8();
    println!("\nLOD0 StripFlags global={g:#04x} class={c:#04x}");
    let lod_cooked_out = r.boolean();
    let inlined = r.boolean();
    println!("  bIsLODCookedOut={lod_cooked_out}  bInlined={inlined}");
    let req = r.i32() as usize;
    println!("  RequiredBones count={req}");
    r.skip(req * 2);

    let nsec = r.i32();
    println!("  NumSections={nsec}");
    if !(0..=64).contains(&nsec) {
        println!("  !! implausible section count; bytes: {:02x?}",
            &body[r.p.min(body.len())..(r.p + 48).min(body.len())]);
        return;
    }
    let mut sec_numverts = 0i32;
    let mut sum_tris = 0i64;
    for s in 0..nsec {
        let _sg = r.u8();
        let sclass = r.u8();
        let mat = r.i16();
        let base_index = r.i32();
        let num_tris = r.i32();
        let _brecompute = r.boolean();
        let _recompute_mask = r.u8(); // RecomputeTangentsVertexMaskChannel
        let _bcast = r.boolean();
        let _bvisrt = r.boolean();
        let base_vertex = r.u32();
        // ClothMappingDataLODs: FMeshToMeshVertData[][] (array of arrays)
        let cloth_outer = r.i32();
        for _ in 0..cloth_outer {
            let inner = r.i32();
            r.skip(inner as usize * FMESH_TO_MESH_VERTDATA);
        }
        // BoneMap ushort[]
        let bonemap_n = r.i32() as usize;
        r.skip(bonemap_n * 2);
        let num_verts = r.i32();
        let max_influences = r.i32();
        let _corr_cloth = r.i16();
        // FClothingSectionData: FGuid(16) + i32 AssetLodIndex
        r.skip(20);
        // DupVert arrays present unless class-stripped bit 1
        if sclass & 1 == 0 {
            let dv = r.i32() as usize;
            r.skip(dv * 4);
            let dvi = r.i32() as usize;
            r.skip(dvi * 8);
        }
        let _bdisabled = r.boolean();
        println!("  section[{s}] mat={mat} baseIdx={base_index} tris={num_tris} baseVtx={base_vertex} verts={num_verts} maxInfl={max_influences} boneMap={bonemap_n}");
        sec_numverts += num_verts;
        sum_tris += num_tris as i64;
    }

    // ActiveBoneIndices (short[]) + buffersSize (uint)
    let active_n = r.i32() as usize;
    r.skip(active_n * 2);
    let buffers_size = r.u32();
    println!("  ActiveBoneIndices={active_n}  buffersSize={buffers_size}  (inlined streamed data follows @export+{})", r.p);

    // --- SerializeStreamedData (bInlined) ---
    let _sg = r.u8();
    let _sc = r.u8();
    // FMultisizeIndexContainer: byte DataSize, then bulk array (i32 elemSize,i32 count,data)
    let data_size = r.u8();
    let idx_elem = r.i32();
    let idx_count = r.i32();
    println!("\n  IndexBuffer: DataSize={data_size} elemSize={idx_elem} count={idx_count}  (tris*3={})", sum_tris * 3);
    r.skip(idx_count as usize * idx_elem.max(0) as usize);

    // FPositionVertexBuffer: i32 Stride, i32 NumVertices, bulk array (i32 elemSize,i32 count,data)
    let pos_stride = r.i32();
    let pos_numverts = r.i32();
    let pos_elem = r.i32();
    let pos_count = r.i32();
    println!("  PositionBuffer: stride={pos_stride} numVerts={pos_numverts} bulk(elem={pos_elem} count={pos_count})  (section verts sum={sec_numverts})");
    r.skip(pos_count as usize * pos_elem.max(0) as usize);
    println!("  [after position data @export+{}] next 40B: {:02x?}", r.p, &body[r.p..(r.p+40).min(body.len())]);

    // FStaticMeshVertexBuffer (tangents + UVs)
    let _sg = r.u8();
    let _sc = r.u8();
    let num_uv = r.i32();
    let vb_numverts = r.i32();
    let full_uv = r.boolean();
    let high_tan = r.boolean();
    let tan_elem = r.i32();
    let tan_count = r.i32();
    r.skip(tan_count as usize * tan_elem.max(0) as usize);
    let uv_elem = r.i32();
    let uv_count = r.i32();
    r.skip(uv_count as usize * uv_elem.max(0) as usize);
    println!("\n  StaticMeshVB: numUV={num_uv} numVerts={vb_numverts} fullUV={full_uv} highTangent={high_tan}");
    println!("    tangents: elem={tan_elem} count={tan_count} (==verts? {})", tan_count == pos_count);
    println!("    UVs: elem={uv_elem} count={uv_count} (==verts*numUV? {})", uv_count == pos_count * num_uv);

    // FSkinWeightVertexBuffer
    let _sg = r.u8();
    let _sc = r.u8();
    let var_bones = r.boolean();
    let sw_maxinfl = r.u32();
    let sw_numbones = r.u32();
    let sw_numverts = r.u32();
    let use16_idx = r.boolean();
    let use16_wt = r.boolean();
    let sw_elem = r.i32();
    let sw_count = r.i32();
    println!("\n  SkinWeightVB: variableBones={var_bones} maxInfl={sw_maxinfl} numBones={sw_numbones} numVerts={sw_numverts} 16bitIdx={use16_idx} 16bitWt={use16_wt}");
    println!("    weightData: elem={sw_elem} count={sw_count}  (numVerts==pos? {})", sw_numverts as i32 == pos_count);

    println!("\n=== GEOMETRY: {pos_count} verts, {} tris, {num_uv} UV set(s), skin(maxInfl={sw_maxinfl}, {}bit idx / {}bit wt) ===",
        idx_count / 3, if use16_idx {16} else {8}, if use16_wt {16} else {8});
}

// FMeshToMeshVertData: 4×FVector4f(16) + 4×u16 + float + i32 = 64+8+4+4 = 80? Only
// used to skip empty cloth arrays here (cloth_outer=0), so exact size unused.
const FMESH_TO_MESH_VERTDATA: usize = 80;
