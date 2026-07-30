//! Per-class natively serialized tails.
//!
//! `UObject::Serialize` appends a `hasGuid`, and every class whose `Serialize`
//! writes raw data appends more after it — base class first, since each
//! override calls `Super::Serialize` before writing its own. These readers
//! account for those bytes; none of them is a CE class, because CE's own
//! classes write no tail at all.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::archive::{tail_why, trace_enabled, ExportContext, Reader};
use super::block::{read_struct, read_struct_with_schema};
use super::common::{native_count, read_bulk_array, read_inline_bulk_data};
use super::structs::*;
use super::reflect::try_read_struct_fields_and_script;
use super::usmap::{Usmap, UsmapProperty};
use super::value::{PropValue, PropertyBlock};

/// `Nanite::FResources::Serialize` (NaniteResources.cpp). The load path ignores
/// the `bCooked` argument — it only changes what a save writes — so the same
/// reader serves both the static-mesh and skeletal-mesh callers.
pub(super) fn read_nanite_resources(r: &mut Reader) -> Result<()> {
    r.take(2)?; // FStripDataFlags
    r.u32()?; // ResourceFlags
    r.i32()?; // StreamablePages: an FByteBulkData header, an index in a Zen package
    let root = native_count(r, "Nanite RootData")?;
    r.take(root)?;
    let pages = native_count(r, "PageStreamingStates")?;
    r.take(pages * 20)?;
    let nodes = native_count(r, "HierarchyNodes")?;
    // `FPackedHierarchyNode` is NANITE_MAX_BVH_NODE_FANOUT (4) slices of 52
    // bytes each — the float variants, so 208 and not the 304 a double-width
    // read would give.
    r.take(nodes * 208)?;
    let roots = native_count(r, "HierarchyRootOffsets")?;
    r.take(roots * 4)?;
    let deps = native_count(r, "PageDependencies")?;
    r.take(deps * 4)?;
    let imposter = native_count(r, "ImposterAtlas")?;
    r.take(imposter * 2)?;
    r.take(16)?; // NumRootPages, PositionPrecision, NormalPrecision, NumInputTriangles
    r.take(12)?; // NumInputVertices, NumInputMeshes + NumInputTexCoords (u16), NumClusters
    Ok(())
}

/// One `FSkelMeshRenderSection` (`SkeletalMeshLODRenderData.cpp`). Returns
/// whether the section carries cloth mapping data, which decides whether the
/// LOD's cloth buffer is present further down.
pub(super) fn read_skel_render_section(r: &mut Reader) -> Result<bool> {
    r.u8()?; // global strip flags
    let class_strip = r.u8()?;
    r.take(2)?; // MaterialIndex (u16)
    r.u32()?; // BaseIndex
    r.u32()?; // NumTriangles
    r.u32()?; // bRecomputeTangent
    r.u8()?; // RecomputeTangentsVertexMaskChannel
    r.u32()?; // bCastShadow
    r.u32()?; // bVisibleInRayTracing
    r.u32()?; // BaseVertexIndex
    // `ClothMappingDataLODs`: an array of arrays of `FMeshToMeshVertData`.
    let mut has_cloth = false;
    let outer = native_count(r, "ClothMappingDataLODs")?;
    for _ in 0..outer {
        let inner = native_count(r, "cloth mapping data")?;
        has_cloth |= inner > 0;
        // `FMeshToMeshVertData` is **64** bytes, not 80: three `FVector4f`, four
        // `uint16` indices, a weight and a padding word
        // (SkeletalMeshLODRenderData.cpp:193). Nothing in the corpus has a
        // non-empty cloth mapping, so the old 80 never desynced anything — it
        // was simply never exercised.
        r.take(inner * 64)?;
    }
    let bones = native_count(r, "BoneMap")?;
    r.take(bones * 2)?;
    r.u32()?; // NumVertices
    r.i32()?; // MaxBoneInfluences
    r.take(2)?; // CorrespondClothAssetIndex (i16)
    r.take(20)?; // FClothingSectionData: FGuid + i32
    // The duplicated-vertex buffer is stripped from cooks that do not need it;
    // bit 0 of the class strip flags says so.
    if class_strip & 1 == 0 {
        let dv = native_count(r, "DupVertData")?;
        r.take(dv * 4)?;
        let dvi = native_count(r, "DupVertIndexData")?;
        r.take(dvi * 8)?;
    }
    r.u32()?; // bDisabled
    Ok(has_cloth)
}

/// `FSkeletalMeshLODRenderData::SerializeStreamedData` — everything that lives
/// either inline in the export or, for a streamed LOD, in the `.ubulk` payload.
pub(super) fn read_skel_streamed_data(r: &mut Reader, has_vertex_colors: bool, has_cloth: bool) -> Result<()> {
    let t = tail_why();
    if t { eprintln!("    streamed data @ {}", r.o); }
    r.take(2)?; // FStripDataFlags
    r.u8()?; // FMultiSizeIndexContainer::DataTypeSize
    read_bulk_array(r, "index buffer")?;
    if t { eprintln!("    positions @ {}", r.o); }
    // FPositionVertexBuffer
    r.i32()?; // Stride
    r.i32()?; // NumVertices
    read_bulk_array(r, "positions")?;
    if t { eprintln!("    static vb @ {}", r.o); }
    // FStaticMeshVertexBuffer
    r.take(2)?; // FStripDataFlags
    r.i32()?; // NumTexCoords
    r.i32()?; // NumVertices
    r.u32()?; // bUseFullPrecisionUVs
    r.u32()?; // bUseHighPrecisionTangentBasis
    read_bulk_array(r, "tangents")?;
    read_bulk_array(r, "UVs")?;
    if t { eprintln!("    skin weights @ {}", r.o); }
    // FSkinWeightVertexBuffer = a data buffer then a lookup buffer.
    r.take(2)?; // FStripDataFlags
    r.u32()?; // bVariableBonesPerVertex
    r.u32()?; // MaxBoneInfluences
    r.u32()?; // NumBoneWeights
    r.u32()?; // NumVertices
    r.u32()?; // bUse16BitBoneIndex
    r.u32()?; // bUse16BitBoneWeight
    read_bulk_array(r, "skin weights")?;
    r.take(2)?; // FStripDataFlags
    r.u32()?; // FSkinWeightLookupVertexBuffer::NumVertices
    read_bulk_array(r, "skin weight lookup")?;
    if t { eprintln!("    colors? @ {}", r.o); }
    if has_vertex_colors {
        // `FColorVertexBuffer` allocates — and so serializes — its payload only
        // when it actually has vertices.
        r.take(2)?; // FStripDataFlags
        r.i32()?; // Stride
        let n = r.u32()?;
        if t { eprintln!("      color verts {n}"); }
        if n > 0 {
            read_bulk_array(r, "vertex colors")?;
        }
    }
    if t { eprintln!("    cloth? @ {}", r.o); }
    if has_cloth {
        r.take(2)?; // FStripDataFlags
        read_bulk_array(r, "cloth vertices")?;
        let n = native_count(r, "ClothIndexMapping")?;
        r.take(n * 12)?; // FClothBufferIndexMapping
    }
    if t { eprintln!("    profiles @ {}", r.o); }
    // `FSkinWeightProfilesData`: a map from profile name to override data.
    //
    // Note these are plain `TArray`/`TMap` members reached through `Ar <<`, so
    // each is a bare count — unlike the vertex buffers above, whose payloads go
    // through `BulkSerialize` and carry an element size ahead of the count.
    let profiles = native_count(r, "SkinWeightProfiles")?;
    for _ in 0..profiles {
        r.take(8)?; // profile FName
        for what in ["profile BoneIDs", "profile BoneWeights"] {
            let n = native_count(r, what)?;
            r.take(n)?; // TArray<uint8>
        }
        r.u8()?; // NumWeightsPerVertex
        let n = native_count(r, "profile VertexIndexToInfluenceOffset")?;
        r.take(n * 8)?; // TMap<uint32, uint32>
    }
    if t { eprintln!("    raytracing @ {}", r.o); }
    // `FRayTracingGeometry::RawData` is a `TResourceArray<uint8>` written with
    // `Ar <<`, so it is a count and then that many bytes.
    let raw = native_count(r, "SourceRayTracingGeometry")?;
    r.take(raw)?;
    if t { eprintln!("    morph @ {}", r.o); }
    // Compressed morph target render data, present only when the cook wrote it.
    if r.u32()? != 0 {
        let n = native_count(r, "MorphData")?;
        r.take(n * 4)?; // TResourceArray<uint32>, so a bare count
        for what in ["MinimumValuePerMorph", "MaximumValuePerMorph"] {
            let n = native_count(r, what)?;
            r.take(n * 16)?; // FVector4f
        }
        for what in ["BatchStartOffsetPerMorph", "BatchesPerMorph"] {
            let n = native_count(r, what)?;
            r.take(n * 4)?;
        }
        r.take(12)?; // NumTotalBatches, PositionPrecision, TangentZPrecision
    }
    if t { eprintln!("    attributes @ {}", r.o); }
    // Per-vertex attribute buffers, keyed by name.
    let attrs = native_count(r, "VertexAttributeBuffers")?;
    if t { eprintln!("      attribute buffers {attrs}"); }
    for _ in 0..attrs {
        r.take(8)?; // attribute FName
        r.i32()?; // ComponentCount
        r.i32()?; // PixelFormat
        r.i32()?; // ComponentStride
        read_bulk_array(r, "attribute values")?;
    }
    if t { eprintln!("    half edge @ {}", r.o); }
    // The mesh-deformer half-edge buffer, behind its own strip flag.
    let half_edge_global = r.u8()?;
    let half_edge_class = r.u8()?;
    let _ = half_edge_global;
    if half_edge_class & 1 == 0 {
        // Both are `TResourceArray<int32>` written with `Ar <<`: a bare count.
        for what in ["VertexToEdgeData", "EdgeToTwinEdgeData"] {
            let n = native_count(r, what)?;
            r.take(n * 4)?;
        }
    }
    Ok(())
}

/// `FSkeletalMeshLODRenderData::SerializeAvailabilityInfo` — the metadata a
/// streamed LOD leaves behind in the export when its buffers went to `.ubulk`.
pub(super) fn read_skel_availability_info(r: &mut Reader, has_cloth: bool) -> Result<()> {
    r.u8()?; // FMultiSizeIndexContainer::DataTypeSize
    r.i32()?; // index buffer NumIndices
    // FStaticMeshVertexBuffer metadata comes before the position buffer's here,
    // the opposite order to SerializeStreamedData.
    r.i32()?; // NumTexCoords
    r.i32()?; // NumVertices
    r.u32()?; // bUseFullPrecisionUVs
    r.u32()?; // bUseHighPrecisionTangentBasis
    r.i32()?; // FPositionVertexBuffer::Stride
    r.i32()?; // FPositionVertexBuffer::NumVertices
    r.i32()?; // FColorVertexBuffer::Stride
    r.u32()?; // FColorVertexBuffer::NumVertices
    r.u32()?; // bVariableBonesPerVertex
    r.u32()?; // MaxBoneInfluences
    r.u32()?; // NumBoneWeights
    r.u32()?; // NumVertices
    r.u32()?; // bUse16BitBoneIndex
    r.u32()?; // bUse16BitBoneWeight
    r.u32()?; // FSkinWeightLookupVertexBuffer::NumVertices
    if has_cloth {
        let n = native_count(r, "ClothIndexMapping")?;
        r.take(n * 12)?;
        r.i32()?; // Stride
        r.u32()?; // NumVertices
    }
    let profiles = native_count(r, "SkinWeightProfileNames")?;
    r.take(profiles * 8)?;
    Ok(())
}

/// One `FSkeletalMeshLODRenderData`.
pub(super) fn read_skel_lod(r: &mut Reader, has_vertex_colors: bool, bulk_data: &[(i64, i64)]) -> Result<()> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    let _ = class_strip;
    let cooked_out = r.u32()? != 0;
    let inlined = r.u32()? != 0;
    let req = native_count(r, "RequiredBones")?;
    r.take(req * 2)?;
    // Everything below is skipped for a server cook or a LOD below the minimum.
    // `EStrippedData::AudioVisual` is bit 1 — bit 0 is `EditorOnly`, which every
    // client cook sets and which must NOT suppress the render buffers.
    if global_strip & 2 != 0 || cooked_out {
        return Ok(());
    }
    let nsec = native_count(r, "RenderSections")?;
    let mut has_cloth = false;
    for _ in 0..nsec {
        has_cloth |= read_skel_render_section(r)?;
    }
    let active = native_count(r, "ActiveBoneIndices")?;
    r.take(active * 2)?;
    r.u32()?; // BuffersSize
    if inlined {
        read_skel_streamed_data(r, has_vertex_colors, has_cloth)?;
    } else {
        // The buffers went to `.ubulk`; only the bulk-data header and the
        // availability metadata stay in the export. A zero-size payload means
        // the LOD was discarded outright and no metadata follows.
        let index = r.i32()?;
        let size = bulk_data.get(index.max(0) as usize).map(|&(_, s)| s).unwrap_or(0);
        if size != 0 {
            read_skel_availability_info(r, has_cloth)?;
        }
    }
    Ok(())
}

/// One RigLogic DNA stream, as `UDNAAsset::Serialize` reads it.
///
/// The DNA container is a foreign format embedded verbatim in the export: a
/// three-byte `DNA` signature, a generation/version pair, a section index, then
/// the sections themselves. Two things make it unlike anything else here —
/// **every scalar in it is big-endian**, and nothing records the stream's total
/// length. Its size is therefore the furthest section end, with section offsets
/// measured from the signature.
///
/// Measured on `SK_Samuel_Marcus_Head_Gameplay`: generation 2, version 5, nine
/// sections (`desc`, `defn`, `bhvr`, `geom`, `mlbh`, `rbfb`, `rbfe`, `jbmd`, …),
/// whose index ends at exactly `desc`'s offset of 155.
/// `UNiagaraScript::SerializeNiagaraShaderMaps` and everything it reaches:
/// `FNiagaraShaderScript::SerializeShaderMap` → `FShaderMapBase::Serialize` →
/// `FMemoryImageResult::LoadFromArchive`.
///
/// This looked like the one part of the corpus that could not be walked, because
/// the payload is a *frozen memory image* — a raw dump of C++ objects whose
/// layout depends on the target platform. But none of it needs interpreting: the
/// frozen image and the shader bytecode are opaque blobs with explicit lengths,
/// and every table around them is a plain count. So the structure is walkable
/// end to end without modelling a single shader.
pub(super) fn read_niagara_shader_maps(r: &mut Reader) -> Result<()> {
    let t = tail_why();
    let resources = native_count(r, "Niagara shader resources")?;
    if t { eprintln!("    resources {resources} of {} bytes", r.b.len()); }
    for _ in 0..resources {
        let cooked = r.u32()? != 0;
        r.i32()?; // NumPermutations
        let hash = native_count(r, "BaseCompileHash")?;
        r.take(hash)?;
        // An uncooked resource writes nothing more, and a cooked one still says
        // whether a shader map compiled successfully.
        if !cooked || r.u32()? == 0 {
            continue;
        }
        read_shader_map(r, true)?;
    }
    Ok(())
}

/// `FShaderMapBase::Serialize`'s load path (ShaderMap.cpp:238) — the frozen
/// memory image, its pointer table, and the shader code. Shared by every asset
/// type that embeds a cooked shader map.
///
/// `niagara_pointer_table` selects `FNiagaraShaderMapPointerTable`, which
/// appends the data-interface class names its shaders bind to as `FString`s
/// after the base table's hashed names. Omitting them desyncs hundreds of bytes
/// later, inside the patch tables, where the symptom looks nothing like the
/// cause.
pub(super) fn read_shader_map(r: &mut Reader, niagara_pointer_table: bool) -> Result<()> {
    let t = tail_why();
    // FMemoryImageResult::LoadFromArchive.
    if t { eprintln!("    layout params @ {}", r.o); }
    r.take(8)?; // FPlatformTypeLayoutParameters: MaxFieldAlignment, Flags
    let frozen = r.u32()? as usize;
    r.take(frozen)?;
    // FShaderMapPointerTable::LoadFromArchive: the base class's type
    // dependencies, then the shader and vertex-factory type names.
    if t { eprintln!("    pointer table @ {}", r.o); }
    let deps = native_count(r, "memory image type dependencies")?;
    r.take(deps * 32)?; // FName + uint32 layout size + FSHAHash
    let types = native_count(r, "shader types")?;
    let vf_types = native_count(r, "vertex factory types")?;
    r.take((types + vf_types) * 8)?; // FHashedName
    if niagara_pointer_table {
        let di_types = native_count(r, "data interface types")?;
        for _ in 0..di_types {
            r.fstring()?;
        }
    }
    // The three patch tables are counted up front, then listed in order.
    if t { eprintln!("    patch counts @ {}", r.o); }
    let vtables = native_count(r, "vtable patch tables")?;
    let script_names = native_count(r, "script name patch tables")?;
    let image_names = native_count(r, "memory image name patch tables")?;
    for _ in 0..vtables {
        r.take(8)?; // TypeNameHash
        let n = native_count(r, "vtable patches")?;
        r.take(n * 8)?; // VTableOffset + Offset
    }
    for _ in 0..(script_names + image_names) {
        r.take(8)?; // FName
        let n = native_count(r, "name patches")?;
        r.take(n * 4)?; // Offset
    }
    if t { eprintln!("    share code @ {}", r.o); }
    let share_code = r.u32()? != 0;
    r.take(8)?; // ShaderPlatformName
    if share_code {
        // The code lives in a shared shader library; only its hash is here.
        r.take(20)?; // FSHAHash
    } else {
        // FShaderMapResourceCode::Serialize — an inline copy of the bytecode.
        r.take(20)?; // ResourceHash
        let hashes = native_count(r, "shader hashes")?;
        r.take(hashes * 20)?;
        let code = native_count(r, "shader code resources")?;
        if t { eprintln!("    {code} code resources @ {}", r.o); }
        for _ in 0..code {
            // FShaderCodeResource is two FSharedBuffers, each a uint64 length
            // then that many bytes.
            for _ in 0..2 {
                let len = usize::try_from(r.u64()?).context("implausible shader buffer")?;
                r.take(len)?;
            }
        }
    }
    Ok(())
}

/// On-disk size of one `EPCGMetadataTypes` value. The vector, quaternion,
/// rotator and transform types are the LWC double variants. `None` for the
/// variable-length types (`String`, `SoftObjectPath`, `SoftClassPath`), which no
/// CE attribute uses — a run that hits one reports an unmodeled tail rather than
/// guessing at it.
pub(super) fn pcg_value_size(type_id: i32) -> Option<usize> {
    Some(match type_id {
        0 => 4,   // Float
        1 => 8,   // Double
        2 => 4,   // Integer32
        3 => 8,   // Integer64
        4 => 16,  // Vector2
        5 => 24,  // Vector
        6 => 32,  // Vector4
        7 => 32,  // Quaternion
        8 => 80,  // Transform
        10 => 4,  // Boolean — an FArchive bool is 32-bit. See pcg_array_element_size.
        11 => 24, // Rotator
        12 => 8,  // Name
        _ => return None,
    })
}

/// The same value's size *inside the `Values` array*, which is not always
/// [`pcg_value_size`].
///
/// `TArray<T>::operator<<` bulk-serializes whenever `sizeof(T) == 1`, so a
/// `TArray<bool>` is written one **byte** per element — while the sibling
/// `DefaultValue` goes through `FArchive::operator<<(bool&)` and is written as
/// a 32-bit int. Same type, same function, two sizes four bytes apart; reading
/// the array at 4 bytes an element sails past the end of one attribute into
/// the next and only surfaces as an implausible count thousands of bytes later.
pub(super) fn pcg_array_element_size(type_id: i32) -> Option<usize> {
    match type_id {
        10 => Some(1), // Boolean
        other => pcg_value_size(other),
    }
}

pub(super) fn dna_be32(b: &[u8], o: usize) -> Result<usize> {
    let s = b.get(o..o + 4).context("DNA read past end")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// The absolute end of the DNA stream starting at `start`, when the container
/// records section sizes — which only version 5 and later do. Returns `Ok(None)`
/// for an older header, whose length its own bytes cannot give.
pub(super) fn dna_stream_end(b: &[u8], start: usize) -> Result<Option<usize>> {
    if b.get(start..start + 3) != Some(b"DNA".as_slice()) {
        bail!("no DNA signature @ {start}");
    }
    let ver = b.get(start + 5..start + 7).context("DNA version past end")?;
    if u16::from_be_bytes([ver[0], ver[1]]) < 5 {
        return Ok(None);
    }
    let count = dna_be32(b, start + 7)?;
    if count > 256 {
        bail!("implausible DNA section count {count} @ {start}");
    }
    let mut end = 0usize;
    for i in 0..count {
        // Each index entry: a four-character id, its generation and version,
        // then the section's offset and size — offsets measured from `start`.
        let p = start + 11 + i * 16;
        end = end.max(dna_be32(b, p + 8)?.saturating_add(dna_be32(b, p + 12)?));
    }
    let stop = start.checked_add(end).filter(|e| *e > start && *e <= b.len());
    Ok(Some(stop.with_context(|| format!("DNA stream ends past the export @ {start}"))?))
}

/// The furthest section offset an unsized (pre-version-5) DNA header records.
/// Its table is eight bare `uint32` offsets and no sizes, so this only bounds
/// where the stream's data must still be running.
pub(super) fn dna_unsized_floor(b: &[u8], start: usize) -> Result<usize> {
    let mut m = 0usize;
    for i in 0..8 {
        m = m.max(dna_be32(b, start + 7 + i * 4)?);
    }
    Ok(start + m)
}

/// `FReferenceSkeleton`'s `operator<<`: the bone info array, the rest pose, and
/// the name-to-index map. Returns the `FTransform` size it settled on, since the
/// callers that read further pose arrays need the same one.
///
/// `FTransform` is 80 bytes when the engine is built with LWC doubles and 40
/// with floats, and nothing in the stream says which. Disambiguate by which
/// choice leaves the following map count reading back as the bone count — the
/// same test `skeletal_mesh.rs` makes.
pub(super) fn read_reference_skeleton(r: &mut Reader) -> Result<usize> {
    let nbones = native_count(r, "RawRefBoneInfo")?;
    r.take(nbones * 12)?; // FMeshBoneInfo: FName + i32 ParentIndex
    let npose = native_count(r, "RawRefBonePose")?;
    let tsize = if npose == 0 {
        80
    } else {
        [80usize, 40]
            .into_iter()
            .find(|&ts| {
                r.b.get(r.o + npose * ts..r.o + npose * ts + 4)
                    .and_then(|s| s.try_into().ok())
                    .map(|s| i32::from_le_bytes(s) == nbones as i32)
                    .unwrap_or(false)
            })
            .context("could not size FTransform in FReferenceSkeleton")?
    };
    r.take(npose * tsize)?;
    let nmap = native_count(r, "RawRefBoneNameToIndexMap")?;
    r.take(nmap * 12)?; // FName + i32
    Ok(tsize)
}

/// Decode an export *completely*: its unversioned property block, `UObject`'s
/// trailer, and then each class in its inheritance chain's natively-serialized
/// tail. Returns the properties and the total bytes consumed.
///
/// A cooked export is not just a property block. `UObject::Serialize` appends a
/// four-byte `hasGuid` (plus a 16-byte GUID when set), and every class whose
/// `Serialize` writes raw data appends more after that — base class first, since
/// each override calls `Super::Serialize` before writing its own. Reading only
/// the property block leaves that tail unaccounted for, which is why most
/// exports looked "decoded but incomplete".
///
/// `object_flags` comes from the export map; class-default objects skip the
/// `UObject` trailer.
/// `FRigVMPropertyPathDescription::operator<<` (RigVMPropertyPath.h:55).
pub(super) fn read_rigvm_property_paths(r: &mut Reader) -> Result<()> {
    let n = native_count(r, "PropertyPathDescriptions")?;
    for _ in 0..n {
        r.i32()?; // PropertyIndex
        r.fstring()?; // HeadCPPType
        r.fstring()?; // SegmentPath
    }
    Ok(())
}

/// `FRigVMOperand::Serialize` (RigVMMemoryCommon.cpp:14): a `uint8` memory type
/// and two `uint16` indices.
pub(super) const RIGVM_OPERAND: usize = 5;

/// `FRigVMByteCode::Load` (RigVMByteCode.cpp:471). Instructions are *re-encoded*
/// on load rather than copied, so the stream is a sequence of tagged ops rather
/// than a byte blob: a `uint8` opcode, then that op's struct — which opens with
/// its own copy of the opcode, because every op derives from `FRigVMBaseOp`.
pub(super) fn read_rigvm_bytecode(r: &mut Reader) -> Result<()> {
    // ERigVMOpCode. Values 0..=64 are the deprecated fixed-arity `Execute`
    // forms, which `Load` folds into `Execute` before dispatching.
    const EXECUTE_64: u8 = 64;
    const ZERO: u8 = 65;
    const COPY: u8 = 68;
    const EQUALS: u8 = 71;
    const NOT_EQUALS: u8 = 72;
    const JUMP_ABSOLUTE: u8 = 73;
    const JUMP_BACKWARD: u8 = 75;
    const JUMP_ABSOLUTE_IF: u8 = 76;
    const JUMP_BACKWARD_IF: u8 = 78;
    const EXIT: u8 = 80;
    const BEGIN_BLOCK: u8 = 81;
    const END_BLOCK: u8 = 82;
    const INVOKE_ENTRY: u8 = 99;
    const JUMP_TO_BRANCH: u8 = 100;
    const EXECUTE: u8 = 101;
    const RUN_INSTRUCTIONS: u8 = 102;
    const SETUP_TRAITS: u8 = 103;

    let count = native_count(r, "RigVM instructions")?;
    for _ in 0..count {
        let op = r.u8()?;
        let op = if op <= EXECUTE_64 { EXECUTE } else { op };
        match op {
            // `FRigVMExecuteOp::Serialize`: opcode, `FunctionIndex`,
            // `ArgumentCount`, then the predicate range — all `uint16` — and
            // finally `ArgumentCount` operands.
            EXECUTE => {
                r.u8()?; // FRigVMBaseOp::OpCode
                r.u16()?; // FunctionIndex
                let args = r.u16()? as usize;
                r.take(4)?; // FirstPredicateIndex, PredicateCount
                r.take(args * RIGVM_OPERAND)?;
            }
            // `FRigVMCopyOp`: source, target, `uint16 NumBytes` and a `uint8`
            // register type.
            COPY => {
                r.u8()?;
                r.take(2 * RIGVM_OPERAND + 3)?;
            }
            // Unary ops — `FRigVMUnaryOp` is opcode plus one operand. The
            // deprecated array opcodes reuse the same shapes by arity.
            ZERO..=67 | 69 | 70 | 83 | 98 => {
                r.u8()?;
                r.take(RIGVM_OPERAND)?;
            }
            // `FRigVMComparisonOp`: A, B, Result.
            EQUALS | NOT_EQUALS => {
                r.u8()?;
                r.take(3 * RIGVM_OPERAND)?;
            }
            // `FRigVMJumpOp`: an `int32` instruction index.
            JUMP_ABSOLUTE..=JUMP_BACKWARD => {
                r.u8()?;
                r.i32()?;
            }
            // `FRigVMJumpIfOp`: the condition operand, the target index, and a
            // `bool Condition` — four bytes, being an `FArchive` bool.
            JUMP_ABSOLUTE_IF..=JUMP_BACKWARD_IF => {
                r.u8()?;
                r.take(RIGVM_OPERAND + 8)?;
            }
            // `Exit` and `EndBlock` write nothing at all — `Load` calls
            // `AddExitOp`/`AddEndBlockOp` without touching the archive.
            EXIT | END_BLOCK => {}
            // Binary ops: `BeginBlock` plus the deprecated two-operand array ops.
            BEGIN_BLOCK | 84 | 85 | 90 | 92 | 93 | 95 => {
                r.u8()?;
                r.take(2 * RIGVM_OPERAND)?;
            }
            // Ternary array ops.
            86 | 87 | 88 | 89 | 96 | 97 => {
                r.u8()?;
                r.take(3 * RIGVM_OPERAND)?;
            }
            91 => {
                // ArrayFind, a quaternary op.
                r.u8()?;
                r.take(4 * RIGVM_OPERAND)?;
            }
            94 => {
                // ArrayIterator, a senary op.
                r.u8()?;
                r.take(6 * RIGVM_OPERAND)?;
            }
            // `FRigVMInvokeEntryOp::Serialize` is the one op that does **not**
            // write its opcode: it writes only the entry name, as an `FString`.
            INVOKE_ENTRY => {
                r.fstring()?;
            }
            // `FRigVMJumpToBranchOp`: operand + `int32 FirstBranchInfoIndex`.
            JUMP_TO_BRANCH => {
                r.u8()?;
                r.take(RIGVM_OPERAND + 4)?;
            }
            // `FRigVMRunInstructionsOp`: operand + start/end `int32`s.
            RUN_INSTRUCTIONS => {
                r.u8()?;
                r.take(RIGVM_OPERAND + 8)?;
            }
            // `FRigVMSetupTraitsOp` inherits `FRigVMUnaryOp::Serialize`.
            SETUP_TRAITS => {
                r.u8()?;
                r.take(RIGVM_OPERAND)?;
            }
            _ => bail!("unknown ERigVMOpCode {op} @ {}", r.o - 1),
        }
    }
    // `Entries` round-trip through `ImportText`, so they are stored as strings.
    let entries = native_count(r, "RigVM entries")?;
    for _ in 0..entries {
        r.fstring()?;
    }
    // `FRigVMBranchInfo::Serialize` (RigVMMemoryStorage.cpp:54): `int32 Index`,
    // the label as an `FString`, two `int32` indices and two `uint16`s.
    let branches = native_count(r, "RigVM branch infos")?;
    for _ in 0..branches {
        r.i32()?; // Index
        r.fstring()?; // Label
        r.take(12)?; // InstructionIndex, ArgumentIndex, FirstInstruction, LastInstruction
    }
    r.fstring()?; // PublicContextPathName — an FString, not an FName
    Ok(())
}

/// `URigVM::Load` (RigVM.cpp:157), for a package new enough that every version
/// gate is satisfied — which is every cooked CE asset.
pub(super) fn read_rigvm(r: &mut Reader, usmap: &Usmap) -> Result<()> {
    let why = tail_why();
    macro_rules! stage {
        ($s:expr) => {
            if why {
                eprintln!("  rigvm: {} @ {}", $s, r.o);
            }
        };
    }
    r.u32()?; // CachedVMHash
    read_rigvm_property_paths(r)?; // ExternalPropertyPathDescriptions
    let fns = native_count(r, "RigVM function names")?;
    r.take(fns * 8)?; // FunctionNamesStorage: TArray<FName>
    stage!("bytecode start");
    read_rigvm_bytecode(r)?;
    stage!("parameters");
    // `FRigVMParameter::Load` (RigVM.cpp:65).
    let params = native_count(r, "RigVM parameters")?;
    for _ in 0..params {
        r.u8()?; // Type
        r.name()?; // Name
        r.i32()?; // RegisterIndex
        r.fstring()?; // CPPType
        r.name()?; // ScriptStructPath
    }
    // `OperandToDebugRegisters` is reached by a bare `Ar <<` on the `TMap`, so
    // it uses `TMap`'s own operator — a plain count and that many pairs — not
    // `FMapProperty`'s delta-serialized form.
    stage!("debug register map");
    let debug = native_count(r, "RigVM debug register map")?;
    for _ in 0..debug {
        r.take(RIGVM_OPERAND)?; // key
        let n = native_count(r, "RigVM debug registers")?;
        r.take(n * RIGVM_OPERAND)?;
    }
    stage!("memory storages");
    // `FRigVMMemoryStorageStruct::Serialize` (RigVMMemoryStorageStruct.cpp:39)
    // is `FInstancedPropertyBag`'s, then the memory type and property paths.
    for _ in 0..3 {
        read_native_variable_struct(r, "InstancedPropertyBag", usmap, 0)?;
        r.u8()?; // MemoryType
        read_rigvm_property_paths(r)?;
    }
    Ok(())
}

/// `EManagedArrayType`, in the order `ManagedArrayTypeValues.inl` declares them
/// (`FNoneType` is 0, so index 1 is the first real entry).
pub(super) const MANAGED_ARRAY_TYPES: &[&str] = &[
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

/// The types with a `TryBulkSerializeManagedArray` overload (ManagedArray.h:21).
/// Their payload writes an element size *and* a count, so it is self-describing
/// and can be skipped without knowing the type at all.
pub(super) fn managed_array_is_bulk(t: &str) -> bool {
    matches!(
        t,
        "Vector" | "IntVector" | "Vector2D" | "Int32" | "Bool" | "Float" | "Quat" | "Guid"
            | "UInt8" | "IntVector2"
    )
}

/// Non-bulk types whose element is a fixed size on disk, so `Ar << TArray<T>`
/// is a bare count followed by `count * size` bytes.
pub(super) fn managed_array_elem(t: &str) -> Option<usize> {
    Some(match t {
        "Transform3f" => 40, // FQuat4f + 2x FVector3f
        "Transform" => 80,   // LWC doubles
        "LinearColor" | "Vector4f" => 16,
        "Vector3d" => 24,
        "Double" => 8,
        "Box" => 49, // two FVector3d and the IsValid byte
        "MeshSection" => 20,
        "PMatrix33d" => 72,
        _ => return None,
    })
}

/// Types whose element is itself an array of fixed-size items.
pub(super) fn managed_array_nested_elem(t: &str) -> Option<usize> {
    Some(match t {
        "Int32Array" | "FloatArray" | "IntArray" => 4,
        "Vector2DArray" | "IntVector2Array" => 8,
        "IntVector3Array" | "VectorArray" => 12,
        "Vector4fArray" => 16,
        "PMatrix33dArray" => 72,
        _ => return None,
    })
}

/// `FChaosArchive::SerializePtr` (ChaosArchive.h:176) — the object-graph form
/// every Chaos smart pointer goes through: a four-byte `bExists`, and when set
/// an `int32 Tag`. A tag already seen in this archive is a back-reference and
/// carries **no payload**; only its first sighting is followed by the object.
/// Returns true when the caller must now read the object itself.
pub(super) fn read_chaos_ptr(r: &mut Reader, seen: &mut std::collections::HashSet<i32>) -> Result<bool> {
    if r.u32()? == 0 {
        return Ok(false);
    }
    let tag = r.i32()?;
    Ok(seen.insert(tag))
}

/// `FImplicitObject::SerializationFactory` (ImplicitObject.cpp:406) dispatches on
/// an `int8` type byte and the object then serializes itself.
/// `FImplicitObject::SerializeImp` is the shared prefix: `bIsConvex` and
/// `bDoCollide` (four bytes each, being `FArchive` bools) then a one-byte
/// `CollisionType`.
///
/// CE ships exactly two shapes across all 14 collections — 1440 spheres and
/// 1434 convex hulls — so the rest of the factory's hierarchy (level sets,
/// triangle meshes, unions, height fields and the scaled/instanced wrappers)
/// is deliberately not modelled; hitting one reports an unmodeled tail.
pub(super) fn read_chaos_implicit_object(r: &mut Reader) -> Result<()> {
    const SPHERE: i8 = 0;
    const CONVEX: i8 = 8;
    let ty = r.u8()? as i8;
    r.take(9)?; // bIsConvex, bDoCollide, CollisionType
    match ty {
        // TSphere: Center then a radius written as `FRealSingle` (it lives in
        // the base class's Margin).
        SPHERE => {
            r.take(16)?;
        }
        // FConvex::SerializeImp (Convex.h:890).
        CONVEX => {
            let planes = native_count(r, "convex planes")?;
            r.take(planes * 24)?; // TPlaneConcrete: MX + MNormal, float3 each
            let verts = native_count(r, "convex vertices")?;
            r.take(verts * 12)?;
            r.take(24)?; // LocalBoundingBox: TAABB (MMin, MMax)
            r.take(4)?; // Volume, as FRealSingle
            r.take(12)?; // CenterOfMass
            r.take(4)?; // Margin, as FRealSingle
            read_convex_structure_data(r)?;
            // Mixed precision in one struct, both measured: the inertia is a
            // float3 while the rotation is a **double** quaternion (its four
            // doubles have norm 1.0000).
            r.take(12 + 32)?; // UnitMassInertiaTensor, RotationOfMass
        }
        _ => bail!("unmodeled Chaos implicit object type {ty} @ {}", r.o - 10),
    }
    Ok(())
}

/// `FConvexStructureData::Serialize` (ConvexStructureData.h:253): an `int8`
/// index width, then the half-edge tables at that width
/// (ConvexHalfEdgeStructureData.h:556) — planes (2 indices each), half-edges
/// (3), vertices (1) and the unique edge list (1).
pub(super) fn read_convex_structure_data(r: &mut Reader) -> Result<()> {
    let w = match r.u8()? as i8 {
        0 => return Ok(()), // None: no container follows
        1 => 1,             // Small:  uint8
        2 => 2,             // Medium: int16
        3 => 4,             // Large:  int32
        n => bail!("unknown convex structure index type {n} @ {}", r.o - 1),
    };
    for per in [2usize, 3, 1, 1] {
        let n = native_count(r, "convex structure table")?;
        r.take(n * per * w)?;
    }
    Ok(())
}

/// `FBVHParticles::Serialize` (BVHParticles.cpp:62) = `FParticles::Serialize`
/// (Particles.h:122 — a four-byte `bSerialize` then the `MX` positions) followed
/// by the bounding-volume hierarchy (BoundingVolumeHierarchy.cpp:696).
///
/// Everything here is single-precision in CE's build, verified by the boxes
/// being byte-identical copies of the float3 particle positions.
pub(super) fn read_bvh_particles(r: &mut Reader) -> Result<()> {
    if r.u32()? == 0 {
        return Ok(()); // bSerialize false writes nothing more
    }
    let mx = native_count(r, "BVH particles")?;
    r.take(mx * 12)?; // FVector3f positions
    let globals = native_count(r, "BVH global objects")?;
    r.take(globals * 4)?;
    // `MWorldSpaceBoxes` is a **TMap<int32, TAABB>**, not an array (the second
    // `SerializeAsAABBs` overload, Box.h:528). A bare `Ar << TMap` uses TMap's
    // own operator — a count then key/value pairs — not `FMapProperty`'s
    // delta-serialized form, so each entry carries its int32 key.
    let boxes = native_count(r, "BVH world-space boxes")?;
    r.take(boxes * (4 + 24))?;
    r.i32()?; // MMaxLevels
    let nodes = native_count(r, "BVH nodes")?;
    for _ in 0..nodes {
        // `operator<<(TBVHNode)` (BoundingVolumeHierarchy.h:53) writes
        // LeafIndex, MAxis, MChildren, MMax, MMin — that order, **not**
        // declaration order.
        r.take(8)?; // LeafIndex, MAxis
        let children = native_count(r, "BVH node children")?;
        r.take(children * 4)?;
        r.take(24)?; // MMax, MMin
    }
    let leafs = native_count(r, "BVH leaves")?;
    for _ in 0..leafs {
        let n = native_count(r, "BVH leaf")?;
        r.take(n * 4)?;
    }
    Ok(())
}

/// `FManagedArrayCollection::Serialize` — a generic container of named, typed
/// arrays. The group table and attribute table are self-describing; each
/// attribute's payload shape is set by its `EManagedArrayType`.
pub(super) fn read_managed_array_collection(r: &mut Reader) -> Result<()> {
    let why = tail_why();
    r.i32()?; // Version
    let groups = native_count(r, "collection groups")?;
    r.take(groups * 16)?; // FName key + FGroupInfo{version, size}
    let attrs = native_count(r, "collection attributes")?;
    let mut chaos_tags = std::collections::HashSet::new();
    for _ in 0..attrs {
        r.take(16)?; // key: attribute FName + group FName
        r.i32()?; // FValueType::version
        let ty = r.i32()?;
        r.take(12)?; // GroupIndexDependency FName + bPersistent
        let name = MANAGED_ARRAY_TYPES.get(ty as usize).copied().unwrap_or("?");
        r.i32()?; // the array's own version
        if why {
            eprintln!("  gc attr {name} @ {}", r.o);
        }
        if managed_array_is_bulk(name) {
            let elem = r.i32()?;
            let n = r.i32()?;
            if elem < 0 || n < 0 {
                bail!("implausible bulk managed array {elem}x{n} @ {}", r.o - 8);
            }
            r.take(elem as usize * n as usize)?;
        } else if name == "String" {
            let n = native_count(r, "collection strings")?;
            for _ in 0..n {
                r.fstring()?;
            }
        } else if let Some(inner) = managed_array_nested_elem(name) {
            let n = native_count(r, "collection nested array")?;
            for _ in 0..n {
                let m = native_count(r, "collection nested element")?;
                r.take(m * inner)?;
            }
        } else if let Some(sz) = managed_array_elem(name) {
            let n = native_count(r, "collection array")?;
            r.take(n * sz)?;
        } else if name == "ImplicitObjectRefCountedPtr" || name == "ConvexRefCountedPtr" {
            let n = native_count(r, "collection implicit objects")?;
            for _ in 0..n {
                if read_chaos_ptr(r, &mut chaos_tags)? {
                    read_chaos_implicit_object(r)?;
                }
            }
        } else if name == "BVHParticlesFloat3UniquePointer" {
            let n = native_count(r, "collection BVH particles")?;
            for _ in 0..n {
                if read_chaos_ptr(r, &mut chaos_tags)? {
                    read_bvh_particles(r)?;
                }
            }
        } else {
            bail!("unmodeled managed array type {name} ({ty}) @ {}", r.o);
        }
    }
    Ok(())
}

/// `FGeometryCollectionRenderData::Serialize` (GeometryCollectionRenderData.cpp:722):
/// two cooked flags, then the mesh buffers and description, then Nanite.
pub(super) fn read_geometry_collection_render_data(r: &mut Reader) -> Result<()> {
    let t = tail_why();
    let has_mesh = r.u32()? != 0;
    let has_nanite = r.u32()? != 0;
    if t {
        eprintln!("  gc render: mesh={has_mesh} nanite={has_nanite} @ {}", r.o);
        for (i, ch) in r.b[r.o..(r.o + 64).min(r.b.len())].chunks(16).enumerate() {
            eprint!("    {:08x}: ", r.o + i * 16);
            for x in ch { eprint!("{x:02x} "); }
            eprintln!();
        }
    }
    if has_mesh {
        // `FGeometryCollectionMeshResources::Serialize` (line 110) — note the
        // index buffer comes **first** here, unlike `FStaticMeshLODResources`,
        // and each buffer writes its own strip flags because they are
        // serialized individually rather than under one shared set.
        read_raw_static_index_buffer(r)?;
        // `FPositionVertexBuffer::Serialize` (PositionVertexBuffer.cpp:162) has
        // **no** strip flags — just `SerializeMetaData` and the vertex data.
        r.i32()?; // Stride
        r.i32()?; // NumVertices
        read_bulk_array(r, "collection positions")?;
        let vb_strip = r.u8()?;
        r.u8()?;
        r.i32()?; // NumTexCoords
        r.i32()?; // NumVertices
        r.u32()?; // bUseFullPrecisionUVs
        r.u32()?; // bUseHighPrecisionTangentBasis
        if vb_strip & 2 == 0 {
            read_bulk_array(r, "collection tangents")?;
            read_bulk_array(r, "collection UVs")?;
        }
        let cb_strip = r.u8()?;
        r.u8()?;
        r.i32()?; // Stride
        let colour_verts = r.i32()?;
        if cb_strip & 2 == 0 && colour_verts > 0 {
            read_bulk_array(r, "collection vertex colours")?;
        }
        // `FBoneMapVertexBuffer::Serialize` (line 62) has no strip flags: a
        // count and then the vertex data.
        r.i32()?; // NumVertices
        read_bulk_array(r, "collection bone map")?;
        if t {
            eprintln!("  gc render: mesh description @ {}", r.o);
        }
        // `FGeometryCollectionMeshDescription::Serialize` (line 126).
        // `FGeometryCollectionMeshElement` is 20 bytes: int16, two uint8s and
        // four uint32s. `SubSections` is written empty in a cooked build.
        r.i32()?; // NumVertices
        r.i32()?; // NumTriangles
        r.take(56)?; // PreSkinnedBounds: FBoxSphereBounds
        for what in ["Sections", "SectionsNoInternal", "SubSections"] {
            let n = native_count(r, what)?;
            r.take(n * 20)?;
        }
    }
    if has_nanite {
        if t {
            eprintln!("  gc render: nanite @ {}", r.o);
        }
        read_nanite_resources(r)?;
    }
    Ok(())
}

/// `FArchive::SerializeCompressedNew`'s load path (Archive.cpp:707).
///
/// The v2 header's tag is `PACKAGE_FILE_TAG | (0x22222222 << 32)` and is
/// followed by the compressor's index; the v1 header is a bare
/// `PACKAGE_FILE_TAG` and names no compressor. The `UncompressedSize` of that
/// first `FCompressedChunkInfo` is not a size at all — it is the chunk size the
/// data was split at, which sets how many chunk infos follow.
pub(super) fn read_compressed_buffer(r: &mut Reader) -> Result<()> {
    const PACKAGE_FILE_TAG: u64 = 0x9E2A_83C1;
    const V2_HEADER_TAG: u64 = PACKAGE_FILE_TAG | (0x2222_2222u64 << 32);
    const LOADING_COMPRESSION_CHUNK_SIZE: u64 = 131072;

    let at = r.o;
    let tag = r.u64()?;
    let chunk_size_field = r.u64()?;
    match tag {
        V2_HEADER_TAG => {
            // FCompressionUtil::SerializeCompressorName: a `uint8` index, where
            // 0 means an `FString` name follows (1 None, 2 Oodle, 3 Zlib,
            // 4 Gzip, 5 LZ4).
            if r.u8()? == 0 {
                r.fstring()?;
            }
        }
        PACKAGE_FILE_TAG => {}
        _ => bail!("not a compressed-buffer header ({tag:#x}) @ {at}"),
    }
    let chunk_size = if chunk_size_field == PACKAGE_FILE_TAG {
        LOADING_COMPRESSION_CHUNK_SIZE
    } else {
        chunk_size_field
    };
    if chunk_size == 0 {
        bail!("compressed buffer declares a zero chunk size @ {at}");
    }
    r.u64()?; // Summary.CompressedSize
    let total_uncompressed = r.u64()?;
    let chunks = total_uncompressed.div_ceil(chunk_size);
    if chunks > 1_000_000 {
        bail!("implausible compressed chunk count {chunks} @ {at}");
    }
    let mut payload = 0u64;
    for _ in 0..chunks {
        payload += r.u64()?; // this chunk's compressed size
        r.u64()?; // and its uncompressed size
    }
    r.take(usize::try_from(payload).context("implausible compressed payload")?)?;
    Ok(())
}

/// `TDynamicVector<T>::Serialize` (DynamicVector.h:163) in its modern form: an
/// element count, and when non-zero a flag saying whether the blocks were
/// Oodle-compressed as one buffer.
pub(super) fn read_dynamic_vector(r: &mut Reader, elem: usize) -> Result<()> {
    /// `TDynamicVector`'s default block length.
    const BLOCK_SIZE: usize = 512;
    let n = r.u32()? as usize;
    if n == 0 {
        return Ok(());
    }
    if r.u32()? != 0 {
        return read_compressed_buffer(r);
    }
    // Uncompressed, the blocks are written whole: `Load` reads
    // `min(Num, BlockSize)` elements for each of `ceil(Num / BlockSize)` blocks.
    let blocks = n.div_ceil(BLOCK_SIZE);
    r.take(blocks * n.min(BLOCK_SIZE) * elem)?;
    Ok(())
}

/// `FRefCountVector::Serialize` (RefCountVector.h:523). The free-index list is
/// only written when the data is neither compacted nor compressed — otherwise
/// it is rebuilt on load from the invalid ref-count sentinels.
pub(super) fn read_ref_count_vector(r: &mut Reader) -> Result<()> {
    let compact = r.u32()? != 0;
    let compressed = r.u32()? != 0;
    let used = r.i32()?;
    read_dynamic_vector(r, 2)?; // RefCounts: TDynamicVector<unsigned short>
    let _ = used;
    if !compact && !compressed {
        read_dynamic_vector(r, 4)?; // FreeIndices
    }
    Ok(())
}

/// `TDynamicVector` behind the four-byte "is set" flag that
/// `SerializeOptionalVector` (DynamicMesh3_Serialization.cpp:70) writes.
pub(super) fn read_optional_dynamic_vector(r: &mut Reader, elem: usize) -> Result<()> {
    if r.u32()? != 0 {
        read_dynamic_vector(r, elem)?;
    }
    Ok(())
}

/// `TDynamicMeshOverlay::Serialize` (DynamicMeshOverlay.cpp:1745): the element
/// ref counts, the elements themselves (`ElementSize` reals apiece), the parent
/// vertex per element, and the per-triangle element indices.
pub(super) fn read_dynamic_mesh_overlay(r: &mut Reader, element_size: usize, real: usize) -> Result<()> {
    read_ref_count_vector(r)?;
    read_dynamic_vector(r, real * element_size)?; // Elements
    read_dynamic_vector(r, 4)?; // ParentVertices
    read_dynamic_vector(r, 4)?; // ElementTriangles
    Ok(())
}

/// `FDynamicMeshAttributeSet::Serialize` (DynamicMeshAttributeSet.cpp:1304).
/// Every layer list is a count followed by that many layers; each attribute
/// opens with its name, written through an `FNameAsStringProxyArchive`
/// (`TDynamicAttributeBase::Serialize`, DynamicAttribute.h:215), so it is an
/// `FString` rather than an `FName`.
pub(super) fn read_dynamic_mesh_attribute_set(r: &mut Reader) -> Result<()> {
    let t = tail_why();
    macro_rules! stage {
        ($s:expr) => {
            if t {
                eprintln!("    attrs: {} @ {}", $s, r.o);
            }
        };
    }
    r.u32()?; // bUseCompression, re-written here
    // `FDynamicMeshUVOverlay` carries two floats per element, normals three.
    stage!("uv layers");
    // Note an overlay's `Serialize` does **not** call `Super::Serialize`, so
    // unlike the vertex/triangle attributes below it writes no name.
    for (what, size) in [("UV", 2usize), ("normal", 3)] {
        let n = native_count(r, what)?;
        for _ in 0..n {
            read_dynamic_mesh_overlay(r, size, 4)?;
        }
    }
    // Polygroups are per-triangle int32s, weights per-vertex floats; both are a
    // name and a single value array.
    stage!("polygroup/weight layers");
    // A vertex/triangle attribute writes its name (via `Super::Serialize`) and
    // then **its own** `bUseCompression` flag before the value array — the
    // overlays above take that flag as a parameter and write nothing.
    for what in ["polygroup", "weight"] {
        let n = native_count(r, what)?;
        for _ in 0..n {
            r.fstring()?;
            r.u32()?; // bUseCompression
            read_dynamic_vector(r, 4)?;
        }
    }
    stage!("colour layer");
    let colours = r.i32()?;
    if colours > 0 {
        read_dynamic_mesh_overlay(r, 4, 4)?;
    }
    stage!("material id");
    if r.u32()? != 0 {
        r.fstring()?;
        r.u32()?; // bUseCompression
        read_dynamic_vector(r, 4)?;
    }
    stage!("skin weights");
    let skins = native_count(r, "skin weight attributes")?;
    for _ in 0..skins {
        r.fstring()?; // key, as a name-through-string
        if r.u32()? != 0 {
            bail!("unmodeled dynamic-mesh skin weight attribute @ {}", r.o);
        }
    }
    stage!("bones");
    if r.u32()? != 0 {
        bail!("unmodeled dynamic-mesh bone attributes @ {}", r.o);
    }
    Ok(())
}

/// `FDynamicMesh3::Serialize` (DynamicMesh3_Serialization.cpp:237). CE cooks
/// every dynamic mesh with `bCompactData` and `bUseCompression` set, which is
/// the `CompactData` variant: unique vertex data, unique triangle data, then
/// the attribute set. The compacted variants write no ref counts or edge data
/// at all — those are rebuilt on load.
pub(super) fn read_dynamic_mesh(r: &mut Reader) -> Result<()> {
    let t = tail_why();
    r.u32()?; // bPreserveDataLayout
    let compact = r.u32()? != 0;
    r.u32()?; // bUseCompression
    if !compact {
        bail!("unmodeled non-compacted FDynamicMesh3 variant @ {}", r.o - 12);
    }
    if t {
        eprintln!("    dynamic mesh: vertices @ {}", r.o);
    }
    read_dynamic_vector(r, 24)?; // Vertices: FVector3d
    read_optional_dynamic_vector(r, 12)?; // VertexNormals: FVector3f
    read_optional_dynamic_vector(r, 12)?; // VertexColors: FVector3f
    read_optional_dynamic_vector(r, 8)?; // VertexUVs: FVector2f
    if t {
        eprintln!("    dynamic mesh: triangles @ {}", r.o);
    }
    read_dynamic_vector(r, 12)?; // Triangles: FIndex3i
    read_optional_dynamic_vector(r, 4)?; // TriangleGroups
    r.i32()?; // GroupIDCounter
    if t {
        eprintln!("    dynamic mesh: attributes @ {}", r.o);
    }
    if r.u32()? != 0 {
        read_dynamic_mesh_attribute_set(r)?;
    }
    Ok(())
}

/// `URigHierarchy::Load` (RigHierarchy.cpp:251). Like `URigVM` this class never
/// calls `Super::Serialize` on the load path, so the export has no property
/// block: it opens straight at the element count.
///
/// `FRigElementKey::Load` (RigHierarchyDefines.cpp:73) writes the element
/// **type as an `FName`** and then the element's own name — two `FName`s, 16
/// bytes — so the per-element reader dispatches on a resolved string.
pub(super) fn read_rig_hierarchy(r: &mut Reader) -> Result<()> {
    let t = tail_why();
    let count = native_count(r, "rig elements")?;
    let mut types = Vec::with_capacity(count);
    for _ in 0..count {
        let ty = r.name()?;
        r.name()?; // the element's own name
        types.push(ty);
    }
    if t {
        let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
        for ty in &types {
            *tally.entry(ty.as_str()).or_default() += 1;
        }
        eprintln!("  rig hierarchy: {count} elements, keys end @ {}", r.o);
        for (k, v) in tally {
            eprintln!("    {v:5}  {k}");
        }
    }
    // Every element is then loaded twice: once for its own data, once for the
    // links between elements. CE's one hierarchy holds only bones and curves,
    // so the control/null/physics/reference/connector/socket arms are left
    // unmodeled rather than written blind.
    const BONE: &str = "ERigElementType::Bone";
    const CURVE: &str = "ERigElementType::Curve";
    for ty in &types {
        // `FRigBaseElement::Load(StaticData)` re-reads the key.
        r.take(16)?;
        match ty.as_str() {
            // `FRigTransformElement::Load` writes
            // `FRigCurrentAndInitialTransform` — four `FRigComputedTransform`s
            // (current/initial x local/global), each an 80-byte LWC `FTransform`
            // plus a four-byte dirty flag — and `FRigBoneElement::Load` adds the
            // bone type, again **as an `FName`**.
            BONE => {
                r.take(4 * (80 + 4))?;
                r.name()?; // ERigBoneType, by name
            }
            // `FRigCurveElement::Load`: a four-byte `bIsValueSet` and the value.
            CURVE => {
                r.take(8)?;
            }
            _ => bail!("unmodeled rig element type {ty} @ {}", r.o),
        }
    }
    if t {
        eprintln!("  rig hierarchy: inter-element data @ {}", r.o);
    }
    for ty in &types {
        // Only `FRigSingleParentElement::Load` writes anything in this phase:
        // its parent's key. Curves have no parent element at all.
        if ty == BONE {
            r.take(16)?;
        }
    }
    // `PreviousNameMap`, `PreviousParentMap` and the element metadata map are
    // reached by a bare `Ar <<`, so each is `TMap`'s own operator — a plain
    // count and that many pairs — not `FMapProperty`'s delta form. All three
    // are empty in CE.
    for what in ["PreviousNameMap", "PreviousParentMap", "ElementMetadata"] {
        let n = native_count(r, what)?;
        if n != 0 {
            bail!("unmodeled non-empty rig hierarchy {what} ({n}) @ {}", r.o);
        }
    }
    Ok(())
}

/// One class's own natively-serialized tail, or nothing when it writes none.
///
/// Returns `false` when the rest of this export is not modeled, so the caller
/// stops and reports the remainder as an unmodeled tail instead of guessing.
/// Every class whose own `Serialize` appends something, i.e. every class the
/// dispatcher in [`read_class_native_tail`] has an arm for.
///
/// Exposed so a tool can ask "does this class contribute a tail of its own, or
/// is its tail entirely inherited?" — which is what says whether a subclass
/// needs a model of its own or can reuse a base class's. Kept next to the
/// dispatcher so the two are edited together.
pub const CLASSES_WITH_OWN_TAIL: &[&str] = &[
    "Actor",
    "ActorComponent",
    "AkAudioEvent",
    "AkAuxBus",
    "AkInitBank",
    "AkRtpc",
    "AkStateValue",
    "AkSwitchValue",
    "AnimSequence",
    "AnimationAsset",
    "BlueprintGeneratedClass",
    "BodySetup",
    "Class",
    "ComputeGraph",
    "ControlRigBlueprintGeneratedClass",
    "DNAAsset",
    "DataTable",
    "DynamicMesh",
    "Enum",
    "FileMediaSource",
    "Font",
    "FontFace",
    "Function",
    "GeometryCollection",
    "HierarchicalInstancedStaticMeshComponent",
    "InstancedFoliageActor",
    "InstancedStaticMeshComponent",
    "LandscapeComponent",
    "LandscapeHeightfieldCollisionComponent",
    "Level",
    "LevelInstance",
    "Material",
    "MaterialInstance",
    "MaterialInterface",
    "Model",
    "ModelComponent",
    "MorphTarget",
    "NiagaraDataInterfaceTexture",
    "NiagaraScript",
    "NiagaraSpriteRendererProperties",
    "NiagaraSystem",
    "PCGLandscapeCache",
    "PCGMetadata",
    "PhysicsAsset",
    "RecastNavMesh",
    "RigVMMemoryStorageGeneratorClass",
    "SceneComponent",
    "ScriptStruct",
    "SkeletalMesh",
    "Skeleton",
    "SkyAtmosphereComponent",
    "SoundCue",
    "SoundNode",
    "SoundNodeWavePlayer",
    "SoundWave",
    "StaticMesh",
    "StaticMeshComponent",
    "StringTable",
    "Struct",
    "Texture",
    "Texture2D",
    "Texture2DArray",
    "TextureCube",
    "UserDefinedStruct",
    "VectorFieldStatic",
    "VolumeTexture",
    "World",
    "WorldPartition",
    "WorldPartitionRuntimeCellData",
];

pub(super) fn read_class_native_tail(
    r: &mut Reader,
    class: &str,
    props: &PropertyBlock,
    usmap: &Usmap,
    ctx: &ExportContext<'_>,
    object_flags: u32,
) -> Result<bool> {
    let bulk_data = ctx.bulk_data;
    let flag = |name: &str| matches!(props.get(name), Some(PropValue::Bool(true)));
    match class {
        // `UActorComponent`: the sparse UCS-modified-property list. Each
        // `FSimpleMemberReference` is an `FPackageIndex`, an `FName` and an
        // `FGuid` — 28 bytes.
        "ActorComponent" => {
            let n = native_count(r, "UCSModifiedProperties")?;
            r.take(n * 28)?;
        }
        // `USceneComponent` writes its baked bounds only when the component
        // asked for them to be computed once for game.
        "SceneComponent" => {
            if flag("bComputeBoundsOnceForGame") || flag("bComputedBoundsOnceForGame") {
                if r.u32()? != 0 {
                    // `FBoxSphereBounds`: origin, extent, radius.
                    r.take(56)?;
                }
            }
        }
        // `UStaticMeshComponent`: per-LOD info, then the cooked mesh-paint
        // texture reference.
        "StaticMeshComponent" => {
            let n = native_count(r, "LODData")?;
            for _ in 0..n {
                read_static_mesh_component_lod_info(r)?;
            }
            if r.u32()? != 0 {
                r.i32()?; // MeshPaintTextureCooked
            }
        }
        // `UInstancedStaticMeshComponent`: a cooked flag, a
        // "skip-serialization properties" flag and — when set — the per-instance
        // transform and custom-data bulk arrays, then the cooked render data as
        // two more bulk arrays. Each bulk array carries its own element size, so
        // `FInstancedStaticMeshInstanceData` need not be modeled.
        "InstancedStaticMeshComponent" => {
            let cooked = r.u32()? != 0;
            let has_skip_serialization_data = r.u32()? != 0;
            if has_skip_serialization_data {
                read_bulk_array(r, "PerInstanceSMData")?;
                read_bulk_array(r, "PerInstanceSMCustomData")?;
            }
            if cooked && r.u32()? != 0 {
                read_bulk_array(r, "instance render data")?;
                read_bulk_array(r, "instance render data")?;
            }
        }
        // `UMaterialInterface` writes `bSavedCachedExpressionData` and, when
        // set, an `FMaterialCachedExpressionData` block. Missing this flag is
        // what made an earlier `MaterialInstance`-only attempt desync — the
        // interface's flag was being read as the instance's.
        "MaterialInterface" => {
            let at = r.o;
            if r.u32()? != 0 && read_struct(r, "MaterialCachedExpressionData", usmap, 0).is_err() {
                // The cached-expression block is only partly modeled. Rewind and
                // report an unmodeled tail rather than failing the export.
                r.o = at;
                return Ok(false);
            }
        }
        // `UMaterialInstance` then writes its own `bSavedCachedData` and an
        // `FMaterialInstanceCachedData` block. Measured on a
        // `MaterialInstanceDynamic` whose 18-byte tail resolves exactly:
        // `hasGuid` 0, interface flag 0, instance flag 1, then a 2-byte property
        // header and an empty `ParentLayerIndexRemap`.
        //
        // Inline shader maps follow only when the instance has a static
        // permutation resource; those are not modeled, so stop there.
        "MaterialInstance" => {
            let at = r.o;
            if r.u32()? != 0 && read_struct(r, "MaterialInstanceCachedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
            // Inline shader maps follow only for an instance with a static
            // permutation resource.
            if flag("bHasStaticPermutationResource") {
                let at = r.o;
                if skip_inline_shader_maps(r).is_err() {
                    r.o = at;
                    return Ok(false);
                }
            }
        }
        // `UMaterial` always writes its inline shader maps.
        "Material" => {
            let at = r.o;
            if skip_inline_shader_maps(r).is_err() {
                r.o = at;
                return Ok(false);
            }
            // `SerializeInlineShaderMaps` ends by writing a bare
            // `int32 NumResourcesToSave` (Material.cpp:825), zero on the
            // non-editor path. Leaving it unread declined nothing and reported
            // no stop -- the walk simply ended four bytes early on all 1,397
            // materials, which is why `ce_tail_stop_census` now measures bytes
            // consumed rather than only whether an arm gave up.
            r.i32()?;
        }
        // `UWorld`: the persistent level, then the extra-referenced-object and
        // streaming-level arrays. Measured on `LI_Mangrove_A`, whose whole
        // 22-byte export resolves as a 2-byte header, one object property,
        // `hasGuid` 0, then `PersistentLevel` = export 3 and two empty arrays.
        "World" => {
            r.i32()?; // PersistentLevel
            for what in ["ExtraReferencedObjects", "StreamingLevels"] {
                let n = native_count(r, what)?;
                r.take(n * 4)?;
            }
        }
        // `UWorldPartitionRuntimeCellData` writes its debug name as an
        // `FString`. Measured on `LI_Mangrove_A`: the export ends exactly after
        // the 37-byte `LI_Mangrove_A_MainPartition_L0_X0_Y0`.
        "WorldPartitionRuntimeCellData" => {
            r.fstring()?;
        }
        // `UWorldPartition::Serialize` (WorldPartition.cpp): a cooked flag and,
        // when it is set, the streaming-policy object reference. UE writes a
        // `bool` through `FArchive` as a 32-bit int, not a byte.
        "WorldPartition" => {
            if r.u32()? != 0 {
                r.i32()?; // StreamingPolicy
            }
        }
        // `USkeletalMesh::Serialize` (SkeletalMesh.cpp): strip flags, the
        // imported bounds, the material list, the reference skeleton, and then —
        // for a cooked package — the whole `FSkeletalMeshRenderData`.
        //
        // `bHasVertexColors` and `bEnablePerPolyCollision` are reflected
        // properties, and both gate part of the native layout, so this reads
        // them out of the already-decoded property block rather than probing.
        "SkeletalMesh" => {
            let at = r.o;
            let t = tail_why();
            let ok = (|| -> Result<()> {
                if t { eprintln!("  skel mesh @ {}", r.o); }
                r.take(2)?; // FStripDataFlags
                r.take(56)?; // FBoxSphereBounds ImportedBounds: LWC doubles
                let nmat = native_count(r, "Materials")?;
                if t { eprintln!("  materials {nmat} @ {}", r.o); }
                for _ in 0..nmat {
                    r.i32()?; // MaterialInterface
                    r.take(8)?; // MaterialSlotName
                    // The imported slot name only survives a cook that keeps
                    // editor data, and the flag saying so is itself serialized.
                    if r.u32()? != 0 {
                        r.take(8)?; // ImportedMaterialSlotName
                    }
                    // FMeshUVChannelInfo: two 32-bit bools + four floats.
                    r.take(24)?;
                }
                read_reference_skeleton(r)?;
                if t { eprintln!("  after ref skeleton @ {}", r.o); }
                let cooked = r.u32()?;
                if t { eprintln!("  bCooked {cooked} @ {}", r.o); }
                if cooked != 0 {
                    // FSkeletalMeshRenderData::Serialize. The mobile min-LOD
                    // index ahead of the LODs is written only when
                    // `r.SkeletalMesh.KeepMobileMinLODSettingOnDesktop` is set,
                    // which is off by default and off in this cook.
                    let lods = native_count(r, "LODRenderData")?;
                    if t { eprintln!("  LODs {lods} @ {}", r.o); }
                    for i in 0..lods {
                        read_skel_lod(r, flag("bHasVertexColors"), bulk_data)?;
                        if t { eprintln!("  after LOD {i} @ {}", r.o); }
                    }
                    read_nanite_resources(r)?;
                    r.u8()?; // NumInlinedLODs — a uint8, not an int32
                    r.u8()?; // NumNonOptionalLODs
                }
                let dummies = native_count(r, "legacy DummyObjs")?;
                r.take(dummies * 4)?;
                if flag("bEnablePerPolyCollision") {
                    r.i32()?; // BodySetup
                }
                Ok(())
            })();
            if let Err(e) = ok {
                // This walk is deep enough that a silent rewind hides which
                // buffer went wrong; `BLAM_TAIL_WHY=1` names it.
                if t {
                    eprintln!("  SkeletalMesh tail bailed @ {}: {e:#}", r.o);
                    let lo = r.o.saturating_sub(96);
                    let hi = (r.o + 96).min(r.b.len());
                    for off in (lo..hi).step_by(16) {
                        let end = (off + 16).min(hi);
                        let hex: Vec<String> =
                            r.b[off..end].iter().map(|x| format!("{x:02x}")).collect();
                        eprintln!("    {off:5}: {}", hex.join(" "));
                    }
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `AInstancedFoliageActor::Serialize` (InstancedFoliage.cpp) writes its
        // `FoliageInfos` map: a count, then per entry the `UFoliageType` key, a
        // `uint8 EFoliageImplType`, and that implementation's own payload. The
        // instance arrays and update GUID beside it are editor-only, so a cooked
        // entry is just the key, the type byte, and — for the only type CE ships,
        // `StaticMesh` — the one component reference `FFoliageStaticMesh` writes.
        "InstancedFoliageActor" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let n = native_count(r, "FoliageInfos")?;
                for _ in 0..n {
                    r.i32()?; // UFoliageType* key
                    match r.u8()? {
                        // Unknown: no implementation is constructed, so nothing
                        // follows.
                        0 => {}
                        1 => {
                            r.i32()?; // FFoliageStaticMesh::Component
                        }
                        other => bail!("unmodeled EFoliageImplType {other}"),
                    }
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `USkeleton::Serialize` (Skeleton.cpp): the `FReferenceSkeleton`, the
        // animation retarget sources, the skeleton `Guid`, the deprecated
        // smart-name container, and a `FStripDataFlags`. The marker names that
        // flag guards are editor-only, so a cooked package ends right after it.
        "Skeleton" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let tsize = read_reference_skeleton(r)?;
                let nret = native_count(r, "AnimRetargetSources")?;
                for _ in 0..nret {
                    r.take(8)?; // map key FName
                    r.take(8)?; // FReferencePose::PoseName
                    let n = native_count(r, "FReferencePose::ReferencePose")?;
                    r.take(n * tsize)?;
                }
                r.take(16)?; // Guid
                // The deprecated smart-name container is a `TMap<FName,
                // FSmartNameMapping>`. Every cooked CE skeleton writes it empty;
                // a non-empty one is reported as an unmodeled tail rather than
                // decoded from a layout no sample here exercises.
                if native_count(r, "SmartNames")? != 0 {
                    bail!("non-empty deprecated SmartNames container");
                }
                r.take(2)?; // FStripDataFlags: global + class
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UNiagaraSpriteRendererProperties::Serialize` ends a cooked export with
        // `FSubUVDerivedData`, which is a single `TArray<FVector2f>` of cutout
        // bounding geometry (`SubUVAnimation.cpp`). Most sprite renderers have no
        // cutout, so the array is usually empty and the tail is just its count.
        "NiagaraSpriteRendererProperties" => {
            let n = native_count(r, "BoundingGeometry")?;
            r.take(n * 8)?; // FVector2f
        }
        // `URigVMMemoryStorageGeneratorClass::Serialize` (RigVMMemoryStorage.cpp)
        // appends two members after `UClass`: the property-path descriptions and
        // the memory type. `FRigVMPropertyPathDescription::operator<<`
        // (RigVMPropertyPath.h) writes `PropertyIndex`, `HeadCPPType` and
        // `SegmentPath` in that order; `ERigVMMemoryType` is a `uint8` enum
        // (Work = 0, Literal = 1).
        "RigVMMemoryStorageGeneratorClass" => {
            let n = native_count(r, "PropertyPathDescriptions")?;
            for _ in 0..n {
                r.i32()?; // PropertyIndex
                r.fstring()?; // HeadCPPType
                r.fstring()?; // SegmentPath
            }
            r.u8()?; // MemoryType
        }
        // `UEnum::Serialize` (Enum.cpp): the `Names` array — an `FName` and an
        // `int64` value per entry — then a `uint8` `CppForm`. Nothing earlier in
        // the chain writes anything, since `UField::Serialize` only emits `Next`
        // on packages older than `RemoveUField_Next`.
        "Enum" => {
            let n = native_count(r, "Enum Names")?;
            r.take(n * 16)?; // FName + int64 per entry
            r.u8()?; // CppForm
        }
        // `UFontFace::Serialize` (FontFace.cpp): a cooked flag, then an
        // inline-data flag; the face bytes follow only when that is set. CE ships
        // every face out of line, so the inline payload is left unmodeled rather
        // than guessed at.
        "FontFace" => {
            let at = r.o;
            r.u32()?; // bCooked
            if r.u32()? != 0 {
                r.o = at;
                return Ok(false);
            }
        }
        // `UAkAudioEvent`: the localized event cooked data as a property block,
        // then the duration/attenuation scalars. The cooked data can carry a
        // bulk payload this reader does not model, so on any mismatch rewind and
        // report an unmodeled tail instead of failing the export.
        "AkAudioEvent" => {
            let at = r.o;
            let ok = read_struct(r, "WwiseLocalizedEventCookedData", usmap, 0).is_ok()
                && r.take(16).is_ok(); // MaximumDuration, MinimumDuration, IsInfinite, MaxAttenuationRadius
            if !ok {
                r.o = at;
                return Ok(false);
            }
        }
        // The rest of the Wwise asset types follow the same shape as
        // `UAkAudioEvent`: each appends its cooked data as an ordinary
        // unversioned property block. The Wwise plugin is not in the UE source
        // tree, but it does not need to be — the `.usmap` describes every one of
        // these structs, so the only thing to know is which struct each class
        // writes.
        "AkStateValue" | "AkSwitchValue" => {
            let at = r.o;
            if read_struct(r, "WwiseGroupValueCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        "AkRtpc" => {
            let at = r.o;
            if read_struct(r, "WwiseGameParameterCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        "AkAuxBus" => {
            let at = r.o;
            if read_struct(r, "WwiseLocalizedAuxBusCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
            // A trailing empty container, zero on all 12 exports.
            r.i32()?;
        }
        "AkInitBank" => {
            let at = r.o;
            if read_struct(r, "WwiseInitBankCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ULevel`: the actor list, the level `FURL`, the BSP model and its
        // components, the level script actor, the nav-list bounds, and the two
        // precomputed lighting/visibility payloads. Any mismatch rewinds and
        // reports an unmodeled tail rather than failing the export.
        "Level" => {
            let at = r.o;
            if read_level_tail(r).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UModel` (BSP): strip flags, bounds, the bulk geometry arrays, the
        // surface table, then the vertex buffer, lighting GUID and lightmass
        // settings.
        "Model" => {
            let at = r.o;
            if let Err(e) = read_model_tail(r) {
                if trace_enabled() {
                    eprintln!("Model tail stopped at {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UNiagaraScript` appends its cooked GPU shader maps. Everything before
        // them in `Serialize` is editor-only, so in a cooked package they follow
        // the property block directly.
        "NiagaraScript" => {
            // A script with no shader maps ends exactly here, and reading a
            // resource count off the end fails in a way that presents as "the
            // tail is unmodeled". It is not — there is nothing left to model.
            // This accounted for 14,767 of the declining exports, every one of
            // them with zero bytes behind it.
            if r.o >= r.b.len() {
                return Ok(true);
            }
            let at = r.o;
            let ok = read_niagara_shader_maps(r);
            if let Err(e) = ok {
                if tail_why() {
                    eprintln!("  NiagaraScript tail bailed @ {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UDNAAsset::Serialize` (RigLogic plugin) reads **two** DNA streams
        // back to back: the behavior layers, then the geometry. The geometry one
        // ships as a stub in a cooked build, since geometry is kept only for the
        // editor — but it is always written, so the uasset layout matches
        // between editor and game.
        // Version 5 headers carry per-section sizes, so the first stream's end
        // is computable. Version 1 headers carry only offsets, and nothing in
        // them gives a length — but `UDNAAsset` is the last class in the chain,
        // so the *second* stream must end exactly at the end of the export. That
        // makes the boundary derivable rather than guessed: it is the unique
        // later `DNA` signature whose own sized index lands on `r.b.len()`.
        "DNAAsset" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                match dna_stream_end(r.b, r.o)? {
                    Some(end) => r.o = end,
                    None => {
                        let floor = dna_unsized_floor(r.b, r.o)?;
                        let start = (floor..r.b.len().saturating_sub(3))
                            .filter(|&i| &r.b[i..i + 3] == b"DNA")
                            .find(|&i| {
                                matches!(dna_stream_end(r.b, i), Ok(Some(e)) if e == r.b.len())
                            })
                            .with_context(|| {
                                format!("no second DNA stream closing the export after {floor}")
                            })?;
                        r.o = start;
                    }
                }
                let end = dna_stream_end(r.b, r.o)?
                    .context("the second DNA stream is itself unsized")?;
                if end != r.b.len() {
                    bail!("second DNA stream ends at {end}, not the export end {}", r.b.len());
                }
                r.o = end;
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UModelComponent::Serialize`: the owning `UModel`, then its elements —
        // each a `MapBuildDataId` GUID, the component and material references,
        // and the BSP node indices, which are `uint16`.
        "ModelComponent" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.i32()?; // Model
                let elements = native_count(r, "model elements")?;
                for _ in 0..elements {
                    r.take(16)?; // MapBuildDataId
                    r.i32()?; // Component
                    r.i32()?; // Material
                    let nodes = native_count(r, "element nodes")?;
                    r.take(nodes * 2)?;
                }
                // The component closes with its own index and node list.
                r.u32()?; // ComponentIndex
                let nodes = native_count(r, "component nodes")?;
                r.take(nodes * 2)?;
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ARecastNavMesh::Serialize` writes a version and then a byte count that
        // the loader simply **seeks past** — the navmesh is rebuilt rather than
        // loaded. The count is measured from its own position, so this is a
        // self-describing skip. Measured: version 26, then 104 bytes, ending the
        // 108-byte tail exactly.
        "RecastNavMesh" => {
            r.u32()?; // NavMeshVersion
            let at = r.o;
            let size = r.u32()? as usize;
            let end = at.checked_add(size).filter(|e| *e >= r.o && *e <= r.b.len());
            match end {
                Some(e) => r.o = e,
                None => {
                    r.o = at - 4;
                    return Ok(false);
                }
            }
        }
        // `UPCGMetadata::Serialize`: an attribute count, then per attribute its
        // name, an `EPCGMetadataTypes` id, the shared `FPCGMetadataAttributeBase`
        // header, and finally the typed value array plus a default — both sized
        // by that type id.
        "PCGMetadata" => {
            let at = r.o;
            let why = tail_why();
            let ok = (|| -> Result<()> {
                let attrs = native_count(r, "PCG attributes")?;
                if why {
                    eprintln!("  PCGMetadata: {attrs} attributes, body ends at {}", r.b.len());
                }
                for ai in 0..attrs {
                    let a0 = r.o;
                    r.take(8)?; // attribute FName
                    let type_id = r.i32()?;
                    // FPCGMetadataAttributeBase::Serialize
                    let entries = native_count(r, "EntryToValueKeyMap")?;
                    r.take(entries * 12)?; // int64 entry key + int32 value key
                    r.i32()?; // ParentAttributeId
                    r.take(8)?; // Name
                    r.i32()?; // AttributeId
                    // `Values` then a single `DefaultValue`, both of that type.
                    let values = native_count(r, "PCG attribute values")?;
                    if why {
                        eprintln!(
                            "    attr {ai}: type {type_id} @ {a0}, {entries} entries, {values} values"
                        );
                    }
                    match pcg_value_size(type_id) {
                        Some(size) => {
                            r.take(values * pcg_array_element_size(type_id).unwrap_or(size))?;
                            r.take(size)?; // DefaultValue
                        }
                        // `String` carries its own length per element.
                        None if type_id == 9 => {
                            for _ in 0..=values {
                                r.fstring()?;
                            }
                        }
                        // `SoftObjectPath`/`SoftClassPath` go through
                        // `FSoftObjectPath::Serialize`: an `FTopLevelAssetPath`
                        // (package and asset `FName`s) then a sub-path
                        // `FString`. Note this is *not* the three-`FName` form
                        // the unversioned property reader uses for the same
                        // type — a plain archive writes the sub-path as a string.
                        None if type_id == 13 || type_id == 14 => {
                            for _ in 0..=values {
                                r.take(16)?; // PackageName + AssetName
                                r.fstring()?; // SubPathString
                            }
                        }
                        None => bail!("unmodeled EPCGMetadataTypes id {type_id} @ {}", r.o),
                    }
                }
                // The metadata closes with its parent entry keys.
                let at_parents = r.o;
                let parents = native_count(r, "ParentKeys")?;
                r.take(parents * 8)?; // PCGMetadataEntryKey is an int64
                if why {
                    eprintln!(
                        "    ParentKeys: {parents} @ {at_parents}, ends {} of {}",
                        r.o,
                        r.b.len()
                    );
                    let lo = at_parents.saturating_sub(16);
                    eprint!("      around count @{lo}:");
                    for x in &r.b[lo..(at_parents + 48).min(r.b.len())] {
                        eprint!(" {x:02x}");
                    }
                    eprintln!();
                    let e = r.o;
                    eprint!("      at end @{e}:");
                    for x in &r.b[e.saturating_sub(16)..(e + 32).min(r.b.len())] {
                        eprint!(" {x:02x}");
                    }
                    eprintln!();
                }
                Ok(())
            })();
            if let Err(e) = ok {
                if tail_why() {
                    eprintln!("  PCGMetadata bailed @ {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `USoundNode::Serialize` writes an `FStripDataFlags`; the graph node it
        // guards is editor-only, so a cook stops there. `USoundCue` does the
        // same after its own `Super::Serialize`.
        "SoundNode" | "SoundCue" => {
            r.take(2)?; // FStripDataFlags
        }
        // `USoundNodeWavePlayer` then writes its wave as a hard reference.
        "SoundNodeWavePlayer" => {
            r.i32()?; // SoundWave, an FPackageIndex
        }
        // `UMorphTarget::Serialize`: strip flags, then the LOD models.
        //
        // Each `FMorphTargetLODModel` opens with
        // `bool bVerticesAreStrippedForCookedBuilds` — **four** bytes, an
        // `FArchive` bool — and a cook always sets it, replacing the whole
        // vertex-delta array with a bare `NumVertices`. The `SourceFilename`
        // that closes the element is written as an *empty* `FString` rather
        // than skipped, so the four zero bytes at the end are load-bearing.
        "MorphTarget" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let strip = r.u16()?;
                if strip & 0x02 != 0 {
                    return Ok(()); // audio-visual data stripped
                }
                let lods = native_count(r, "MorphLODModels")?;
                for _ in 0..lods {
                    if r.u32()? != 0 {
                        r.i32()?; // NumVertices, the array having been stripped
                    } else {
                        // `FMorphTargetDelta`: two `FVector3f`s and a `uint32`.
                        let verts = native_count(r, "morph vertices")?;
                        r.take(verts * 28)?;
                    }
                    r.i32()?; // NumBaseMeshVerts
                    let sections = native_count(r, "SectionIndices")?;
                    r.take(sections * 4)?;
                    r.u32()?; // bGeneratedByEngine
                    r.fstring()?; // SourceFilename, empty in a cook
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `USoundWave::Serialize`: a packed `uint32` of flags (bit 0 = cooked),
        // the cue points, the compressed-data GUID and the streamed chunk
        // table.
        //
        // Whether the compressed audio is inline (`FFormatContainer`) or
        // streamed (`FStreamedAudioPlatformData`) is decided at cook time by
        // `IsStreaming()` and is recorded nowhere, so the two layouts can only
        // be told apart by trying one. Campaign Evolved streams every one of
        // its waves, so the streamed form is read and anything that does not
        // account for the export exactly is rewound and reported — `USoundWave`
        // is last in its chain, which makes "ends on the final byte" a real
        // check rather than a plausibility one.
        "SoundWave" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let flags = r.u32()?;
                if flags & 1 == 0 {
                    bail!("uncooked SoundWave");
                }
                // `SerializeCuePoints`, cooked. `FSoundWaveCuePoint` has no
                // hand-written serializer — its `operator<<` calls
                // `SerializeItem`, so each element is an ordinary unversioned
                // property block, not a fixed-size record.
                let cues = native_count(r, "CuePoints")?;
                for _ in 0..cues {
                    read_struct(r, "SoundWaveCuePoint", usmap, 0)?;
                }
                r.take(16)?; // CompressedDataGuid
                let chunks = native_count(r, "audio chunks")?;
                r.name()?; // AudioFormat
                for _ in 0..chunks {
                    // IsCooked 1, HasSeekOffset 2, IsInlined 4.
                    let chunk_flags = r.u32()?;
                    read_inline_bulk_data(r, bulk_data, "audio chunk")?;
                    r.i32()?; // DataSize
                    r.i32()?; // AudioDataSize
                    if chunk_flags & 2 != 0 {
                        r.i32()?; // SeekOffsetInAudioFrames
                    }
                }
                if r.o != r.b.len() {
                    bail!("streamed layout ended at {} of {}", r.o, r.b.len());
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // Three classes end with a trailing empty container that no arm read.
        // Each is zero on every export in the corpus, and each was invisible
        // until the census started measuring bytes *consumed* rather than only
        // whether an arm declined: an arm that returns "kept going" while
        // leaving bytes behind reports no stop at all.
        //
        // `UNiagaraDataInterfaceTexture` (3,260 exports) and `UFont` (6) leave
        // one `int32`; `UFileMediaSource` (54) leaves two.
        "NiagaraDataInterfaceTexture" | "Font" => {
            r.i32()?;
        }
        "FileMediaSource" => {
            r.take(8)?;
        }
        // `ALevelInstance::Serialize` appends `LevelInstanceActorGuid`; the
        // packed variant's own `PackedVersion` is editor-only.
        "LevelInstance" => {
            r.take(16)?; // FGuid
        }
        // `UVectorFieldStatic::Serialize` appends its volume texture source as
        // a single bulk payload, which the cook forces inline.
        "VectorFieldStatic" => {
            let at = r.o;
            if read_inline_bulk_data(r, bulk_data, "VectorFieldStatic SourceData").is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UPCGLandscapeCache::Serialize`: a count, then per entry a
        // `TPair<FGuid, FIntPoint>` key and `FPCGLandscapeCacheEntry::Serialize`
        // — half-size, stride, the layer names, and a bulk-data handle the cook
        // deliberately keeps *out* of line (`BULKDATA_Force_NOT_InlinePayload`),
        // so only its index is here.
        "PCGLandscapeCache" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let entries = native_count(r, "PCGLandscapeCache entries")?;
                for _ in 0..entries {
                    r.take(16 + 8)?; // FGuid + FIntPoint
                    r.take(24)?; // FVector PointHalfSize
                    r.i32()?; // Stride
                    let names = native_count(r, "LayerDataNames")?;
                    r.take(names * 8)?;
                    read_inline_bulk_data(r, bulk_data, "landscape cache entry")?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `USkyAtmosphereComponent::Serialize` appends `bStaticLightingBuiltGUID`.
        // The version gate around it only excludes components converted from the
        // old `AtmosphericFog`, which CE has none of, and the 16-byte tail on
        // every one of these confirms it.
        "SkyAtmosphereComponent" => {
            r.take(16)?; // FGuid
        }
        // `UPhysicsAsset::Serialize` appends `CollisionDisableTable`, a
        // `TMap<FRigidBodyIndexPair, bool>` — two `int32` body indices and a
        // 32-bit bool per entry. Measured on `PHYS_COV_Door_A`: count 3, then
        // the pairs (0,1), (0,2) and (1,2), all false, ending the export exactly.
        "PhysicsAsset" => {
            let n = native_count(r, "CollisionDisableTable")?;
            r.take(n * 12)?;
        }
        // `UStringTable::Serialize` hands off to `FStringTable::Serialize`: the
        // table namespace, the key/source-string entries, and a per-key
        // meta-data map. Every string here is an `FString`, including the text
        // keys, which serialize as strings rather than as names.
        "StringTable" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.fstring()?; // TableNamespace
                let entries = native_count(r, "StringTable entries")?;
                for _ in 0..entries {
                    r.fstring()?; // Key
                    r.fstring()?; // SourceString
                }
                let keys = native_count(r, "StringTable meta-data keys")?;
                for _ in 0..keys {
                    r.fstring()?; // key
                    let meta = native_count(r, "meta-data entries")?;
                    for _ in 0..meta {
                        r.take(8)?; // meta-data id FName
                        r.fstring()?; // value
                    }
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UTexture` writes only its strip flags in a cooked stream.
        "Texture" => {
            r.take(2)?;
        }
        // `UTexture2D`: strip flags, `bCooked`, `bSerializeMipData`, then the
        // cooked platform data — a list of `(pixel-format FName, int64
        // SkipOffset, FTexturePlatformData)` terminated by a `None` name.
        //
        // `SkipOffset` is a delta from its own location to the end of that
        // platform data, so the whole block can be *skipped* rather than
        // modeled. Measured on a 3038-byte texture: format name at 42,
        // SkipOffset 2980 at 50 → 3030, then the 8-byte `None` terminator ends
        // the export exactly.
        "Texture2D" => {
            let at = r.o;
            if read_texture_tail(r, true).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // The other cooked texture shapes share `UTexture::SerializeCookedPlatformData`
        // and differ from `UTexture2D` only in *not* writing its
        // `bSerializeMipData` flag. Between them they carry 179 MB of otherwise
        // unread payload, all of it skippable by the same `SkipOffset`.
        "TextureCube" | "VolumeTexture" | "Texture2DArray" => {
            let at = r.o;
            if read_texture_tail(r, false).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UBodySetup`: the setup GUID, a cooked flag, a has-cooked-data flag,
        // then an `FFormatContainer` — a count, and per format an `FName` and an
        // `FByteBulkData`.
        //
        // In a Zen package an `FByteBulkData` header is just an **int32 index**
        // into the package's bulk-data map; the payload itself is stored inline
        // right after it. Measured on `SM_Basis_HS`: `bulk[0]` is offset 76,
        // size 23802 — 76 is exactly where the index ends, and 76 + 23802 is the
        // export length. The offset is re-checked against the cursor, so a
        // payload that is *not* inline is left alone instead of over-consumed.
        "BodySetup" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.take(16)?; // BodySetupGuid
                if r.u32()? == 0 {
                    return Ok(()); // not cooked
                }
                r.u32()?; // bHasCookedData
                let n = native_count(r, "CookedFormatData")?;
                for _ in 0..n {
                    r.name()?;
                    let index = r.i32()?;
                    let Some(&(offset, size)) = bulk_data.get(index.max(0) as usize) else {
                        bail!("bulk data index {index} out of range");
                    };
                    if offset as usize != r.o {
                        bail!("bulk payload at {offset} is not inline at {}", r.o);
                    }
                    r.take(size.max(0) as usize)?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UHierarchicalInstancedStaticMeshComponent` (and the foliage variant
        // that shares its serializer) appends the instance cluster tree as a
        // bulk array, which carries its own element size.
        "HierarchicalInstancedStaticMeshComponent" => {
            let at = r.o;
            if read_bulk_array(r, "ClusterTree").is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UStruct::Serialize` (Class.cpp): `SuperStruct`, then a
        // `TArray<UField*> ChildArray`, then `SerializeProperties` — an `int32`
        // count and that many `FField`s — then the Kismet script.
        //
        // There is no padding anywhere in that sequence, so this reads it
        // straight through. An earlier version probed a few word offsets for the
        // field count; that silently accepted a wrong interpretation whenever
        // the real parse failed, reporting a bogus "decoded" prefix instead of a
        // tail, which is how three `FProperty` layout bugs stayed hidden.
        "Struct" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.i32()?; // SuperStruct
                let children = native_count(r, "ChildArray")?;
                r.take(children * 4)?;
                try_read_struct_fields_and_script(r)
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UScriptStruct::Serialize` (Class.cpp) adds exactly one `uint32` —
        // the non-computed half of `StructFlags`. Everything else about a
        // script struct is recomputed from `CppStructOps` on load.
        "ScriptStruct" => {
            let at = r.o;
            if r.u32().is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UUserDefinedStruct::Serialize` then writes a **default instance of
        // itself** (`SerializeItem`), i.e. one unversioned property block whose
        // schema is the `FField` chain this very export just defined — not
        // anything the `.usmap` knows about. `Struct`'s arm stashed that chain.
        //
        // A class-default object returns before writing it (`HasAnyFlags(
        // RF_ClassDefaultObject)`), so it has no instance to read.
        "UserDefinedStruct" => {
            const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
            if object_flags & RF_CLASS_DEFAULT_OBJECT != 0 {
                return Ok(true);
            }
            let Some(fields) = r.struct_fields.clone() else { return Ok(false) };
            // The struct owns every field it declares, so it names itself as
            // the owner for the native-bool lookup.
            let schema: Vec<(&UsmapProperty, u8, &str)> = fields
                .iter()
                .flat_map(|f| (0..f.array_dim.max(1)).map(move |i| (f, i, "UserDefinedStruct")))
                .collect();
            let at = r.o;
            if read_struct_with_schema(r, "UserDefinedStruct default", &schema, usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UDataTable::LoadStructData`: an `int32` row count, then per row an
        // `FName` key and one property block against the table's `RowStruct`.
        //
        // `RowStruct` is an `FPackageIndex` in the table's own property block,
        // so the schema for the rows lives outside this export entirely —
        // either a native struct named by a script import, or a
        // `UUserDefinedStruct` exported by another package. Without a resolver
        // to turn that reference into a schema the rows cannot be walked at
        // all, so report them as an unmodeled tail rather than guess.
        // `UCompositeDataTable` adds nothing of its own and is covered by
        // inheriting this arm.
        "DataTable" => {
            let Some(resolver) = ctx.resolver else { return Ok(false) };
            let Some(PropValue::Object(row_ref)) = props.get("RowStruct") else {
                return Ok(false);
            };
            let Some(row_struct) = resolver.struct_name(*row_ref) else { return Ok(false) };
            let at = r.o;
            let ok = (|| -> Result<()> {
                let rows = native_count(r, "DataTable rows")?;
                for i in 0..rows {
                    let key = r.name()?;
                    read_struct(r, &row_struct, usmap, 0)
                        .with_context(|| format!("row {i} ({key})"))?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UFunction` appends its flags, plus a replication offset for a
        // networked function (`FUNC_Net`).
        "Function" => {
            let flags = r.u32()?;
            if flags & 0x0040 != 0 {
                r.u16()?; // RepOffset, for FUNC_Net
            }
            // Blueprint event-graph fast-call info, always serialized to keep
            // the stream in sync even when the feature is compiled out.
            r.i32()?; // EventGraphFunction
            r.i32()?; // EventGraphCallOffset
        }
        // `UClass`, after `UStruct`: the function map, class flags and
        // ownership, the implemented-interface table, and the class default
        // object.
        "Class" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let funcs = native_count(r, "FuncMap")?;
                r.take(funcs * 12)?; // FName + FPackageIndex
                r.u32()?; // ClassFlags
                r.i32()?; // ClassWithin
                r.name()?; // ClassConfigName
                r.i32()?; // ClassGeneratedBy
                // `FImplementedInterface`: class, pointer offset, and a
                // four-byte "implemented by K2" flag.
                let ifaces = native_count(r, "Interfaces")?;
                r.take(ifaces * 12)?;
                r.u32()?;
                r.name()?;
                r.u32()?; // bCooked
                r.i32()?; // ClassDefaultObject
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UDynamicMesh::Serialize` (UDynamicMesh.cpp:312) appends `Ar << *Mesh`
        // after the tagged properties — nothing else.
        "DynamicMesh" => {
            let at = r.o;
            if let Err(e) = read_dynamic_mesh(r) {
                if tail_why() {
                    eprintln!("  DynamicMesh bailed @ {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UGeometryCollection::Serialize` (GeometryCollectionObject.cpp:939)
        // writes a cooked flag after the tagged properties, then the
        // `FManagedArrayCollection` through an `FChaosArchive`, then a second
        // cooked flag gating the render data.
        "GeometryCollection" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.u32()?; // bIsCookedOrCooking
                read_managed_array_collection(r)?;
                let cooked = r.u32()? != 0;
                if cooked {
                    read_geometry_collection_render_data(r)?;
                }
                Ok(())
            })();
            if ok.is_err() {
                if tail_why() {
                    eprintln!("  GeometryCollection bailed @ {}: {:#}", r.o, ok.unwrap_err());
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UComputeGraph::Serialize` (ComputeGraph.cpp:43) appends one
        // `FComputeKernelResourceSet` per kernel; each is a count of resources
        // (ComputeGraph.cpp:948) and each resource a cooked flag, a validity
        // flag, and a shader map (ComputeKernelShared.cpp:178). Unlike Niagara's
        // it uses the plain `FShaderMapPointerTable`.
        "ComputeGraph" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let kernels = native_count(r, "compute kernels")?;
                for _ in 0..kernels {
                    let resources = native_count(r, "compute kernel resources")?;
                    for _ in 0..resources {
                        let cooked = r.u32()? != 0;
                        if cooked && r.u32()? != 0 {
                            read_shader_map(r, false)?;
                        }
                    }
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UControlRigBlueprintGeneratedClass::Serialize`
        // (ControlRigBlueprintGeneratedClass.cpp:16) embeds an entire `URigVM`
        // by value — it builds a transient VM and calls `VM->Serialize(Ar)` —
        // and then writes its graph-function store. Note it calls
        // `UBlueprintGeneratedClass::Serialize` directly, deliberately skipping
        // `URigVMBlueprintGeneratedClass`, which is why that class has no arm.
        "ControlRigBlueprintGeneratedClass" => {
            let at = r.o;
            // `FRigVMGraphFunctionStore::operator<<`
            // (RigVMGraphFunctionHost.h:70) writes only `PublicFunctions`;
            // `PrivateFunctions` goes out solely to reference collectors. CE
            // ships the list empty, so rather than guess at
            // `FRigVMGraphFunctionData`'s layout, a non-empty list is reported
            // as an unmodeled tail.
            let ok = read_rigvm(r, usmap).is_ok()
                && matches!(native_count(r, "PublicFunctions"), Ok(0));
            if !ok {
                r.o = at;
                return Ok(false);
            }
        }
        // `UBlueprintGeneratedClass` appends its cooked editor tags, but only
        // when there is more than a trailing word left to read.
        "BlueprintGeneratedClass" => {
            let at = r.o;
            if r.b.len().saturating_sub(r.o) > 4 {
                let ok = (|| -> Result<()> {
                    let n = native_count(r, "EditorTags")?;
                    for _ in 0..n {
                        r.name()?;
                        r.fstring()?;
                    }
                    Ok(())
                })();
                if ok.is_err() {
                    r.o = at;
                    return Ok(false);
                }
            }
        }
        // `UNiagaraSystem` appends one `FNiagaraEmitterCompiledData` property
        // block per emitter. Measured on `NS_collision`: a count of 1 followed
        // by a nine-value property header.
        "NiagaraSystem" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let n = native_count(r, "NiagaraEmitterCompiledData")?;
                for _ in 0..n {
                    read_struct(r, "NiagaraEmitterCompiledData", usmap, 0)?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ULandscapeHeightfieldCollisionComponent`: a cooked flag, then the
        // cooked collision data as a bulk array (which carries its own element
        // size).
        "LandscapeHeightfieldCollisionComponent" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                if r.u32()? != 0 {
                    read_bulk_array(r, "CookedCollisionData")?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ULandscapeComponent`: the grass data — element count, a
        // `TMap<FPackageIndex, int32>` of weight offsets, and the packed
        // height/weight bytes — then a cooked flag. Measured on an A50 landscape
        // component: 4096 elements, two weight offsets, 16384 data bytes, and
        // the four-byte flag land exactly on the 16874-byte export end.
        "LandscapeComponent" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.i32()?; // NumElements
                let offsets = native_count(r, "grass weight offsets")?;
                r.take(offsets * 8)?; // FPackageIndex + int32
                let data = native_count(r, "HeightWeightData")?;
                r.take(data)?;
                r.u32()?; // bCooked
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UStaticMesh`: strip flags, cooked flag, body setup / nav collision,
        // lighting GUID, sockets, then `FStaticMeshRenderData`'s LOD array.
        // Anything past the LODs (Nanite resources, ray-tracing proxy, distance
        // fields) is not modeled, so the walk stops there.
        "StaticMesh" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.take(2)?; // FStripDataFlags
                r.u32()?; // bCooked
                r.i32()?; // BodySetup
                r.i32()?; // NavCollision
                r.take(16)?; // LightingGuid
                let sockets = native_count(r, "Sockets")?;
                r.take(sockets * 4)?;
                let lods = native_count(r, "LODs")?;
                for _ in 0..lods {
                    read_static_mesh_lod(r)?;
                }
                r.u8()?; // numInlinedLODs
                // FNaniteResources
                r.take(2)?; // FStripDataFlags
                r.u32()?; // ResourceFlags
                r.i32()?; // StreamablePages: FByteBulkData index (Zen)
                let root = native_count(r, "Nanite RootData")?;
                r.take(root)?;
                let pages = native_count(r, "PageStreamingStates")?;
                r.take(pages * 20)?;
                let nodes = native_count(r, "HierarchyNodes")?;
                // `FPackedHierarchyNode` = NANITE_MAX_BVH_NODE_FANOUT (4) slices,
                // each `FSphere3f` + `FVector3f` + 2 halves + `FVector3f` + 2
                // uint32 = 52 bytes, so 208 per node.
                r.take(nodes * 208)?;
                let roots = native_count(r, "HierarchyRootOffsets")?;
                r.take(roots * 4)?;
                let deps = native_count(r, "PageDependencies")?;
                r.take(deps * 4)?;
                let imposter = native_count(r, "ImposterAtlas")?;
                r.take(imposter * 2)?;
                r.take(16)?; // NumRootPages, PositionPrecision, NormalPrecision, NumInputTriangles
                r.take(12)?; // NumInputVertices, NumInputMeshes+TexCoords (u16), NumClusters
                let ray_proxy = r.u32()?; // bHasRayTracingProxy
                if ray_proxy != 0 {
                    // FStaticMeshRayTracingProxy: strip flags, a
                    // "using rendering LODs" flag, then one entry per LOD.
                    r.take(2)?;
                    r.u32()?; // bUsingRenderingLODs
                    let proxy_lods = native_count(r, "ray tracing proxy LODs")?;
                    for _ in 0..proxy_lods {
                        if r.u32()? != 0 {
                            // bOwnsBuffers
                            let sec = native_count(r, "proxy sections")?;
                            r.take(sec * 40)?;
                        }
                        r.u32()?; // bOwnsRayTracingGeometry
                        // StreamableData: an `FByteBulkData` index; its payload
                        // is inline only when the map's offset is right here.
                        let index = r.i32()?;
                        if let Some(&(offset, size)) = bulk_data.get(index.max(0) as usize) {
                            if offset as usize == r.o {
                                r.take(size.max(0) as usize)?;
                            }
                        }
                    }
                }
                // SerializeInlineDataRepresentations: strip flags, then per LOD
                // a validity flag and an `FDistanceFieldVolumeData5`.
                // `SerializeInlineDataRepresentations` — the **card
                // representation**, not the distance field (that follows).
                // Class strip bit 1 is `CardRepresentationDataStripFlag`.
                let cr_global = r.u8()?;
                let cr_class = r.u8()?;
                if cr_global & 2 == 0 && cr_class & 2 == 0 {
                    for _ in 0..lods {
                        if r.u32()? == 0 {
                            continue;
                        }
                        r.take(49)?; // Bounds (FBox)
                        r.u32()?; // bMostlyTwoSided
                        // `FLumenCardBuildData`: an `FLumenCardOBB` of five
                        // `FVector3f` (60 bytes) plus the axis-aligned direction
                        // index. `MaxLodLevel`/`LODLevel` are not written in 5.1+.
                        let cards = native_count(r, "CardBuildData")?;
                        r.take(cards * 61)?;
                    }
                }
                // Distance-field volumes, then the render data's own bounds and
                // LOD screen sizes. Class strip bit 0 gates the distance fields.
                let df_global = r.u8()?;
                let df_class = r.u8()?;
                if df_global & 2 == 0 && df_class & 1 == 0 {
                    for _ in 0..lods {
                        if r.u32()? == 0 {
                            continue;
                        }
                        // `FDistanceFieldVolumeData5`. `LocalSpaceMeshBounds` is
                        // an **`FBox3f`** — six floats and `IsValid`, 25 bytes,
                        // not the 49-byte double-width `FBox`. Measured on
                        // `SM_Sphere_64Seg`: ±100 bounds as floats, then three
                        // 56-byte `FSparseDistanceFieldMip` (a 6×6×6 indirection,
                        // brick count, UV scale 0.4762, UV add 0.5, scale/bias,
                        // bulk offset and size 864).
                        r.take(25)?;
                        r.u32()?; // bMostlyTwoSided
                        r.take(3 * 56)?;
                        let always = native_count(r, "AlwaysLoadedMip")?;
                        r.take(always)?;
                        r.i32()?; // StreamableMips: FByteBulkData index
                    }
                }
                r.take(56)?; // Bounds: FBoxSphereBounds
                r.u32()?; // bLODsShareStaticLighting
                // `ScreenSize[MAX_STATIC_LODS_UE4]`, each an `FPerPlatformFloat`.
                r.take(8 * 8)?;
                // Two bytes of strip flags close out the render data, then
                // `UStaticMesh` finishes with its SpeedTree flag and material
                // table. Measured on `SM_Basis_HS`, whose final 46 bytes resolve
                // exactly: one `FStaticMaterial` = material import -4, an empty
                // slot `FName`, `bInitialized`, `bOverrideDensities` and the four
                // `LocalUVDensities` floats (404.85, 411.49, 0, 0).
                r.take(2)?;
                r.u32()?; // bHasSpeedTreeWind
                let materials = native_count(r, "StaticMaterials")?;
                r.take(materials * 36)?;
                if trace_enabled() {
                    eprintln!("after render data @ {}", r.o);
                }
                if trace_enabled() {
                    eprintln!(
                        "Nanite: root {root}, {pages} pages, {nodes} nodes, rayproxy {ray_proxy} -> {}",
                        r.o
                    );
                }
                Ok(())
            })();
            if let Err(e) = ok {
                if trace_enabled() {
                    eprintln!("StaticMesh tail stopped at {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UAnimationAsset::Serialize` writes its `SkeletonGuid` — the 16 bytes
        // that sit between the object trailer and `UAnimSequence`'s strip flags.
        "AnimationAsset" => {
            r.take(16)?;
        }
        // `UAnimSequence`: strip flags (raw animation data is editor-only and
        // stripped in a cook), then the compressed-data block per UE 5.5.4
        // `FCompressedAnimSequence::SerializeCompressedData`.
        "AnimSequence" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.take(2)?; // FStripDataFlags
                if r.u32()? == 0 {
                    return Ok(()); // bSerializeCompressedData
                }
                r.i32()?; // CompressedRawDataSize
                let tracks = native_count(r, "CompressedTrackToSkeletonMapTable")?;
                r.take(tracks * 4)?;
                // `FAnimCompressedCurveIndexedName`'s `operator<<` writes ONLY
                // `CurveName` — its `CurveIndex` is serialized just for
                // `IsCountingMemory()`, so on load the element is 8 bytes, not
                // the 12 the struct declares.
                let curves = native_count(r, "IndexedCurveNames")?;
                r.take(curves * 8)?;
                let num_bytes = native_count(r, "CompressedByteStream")?;
                if trace_enabled() {
                    eprintln!("anim: tracks {tracks} curves {curves} numbytes {num_bytes} @ {}", r.o);
                }
                let use_bulk = r.u32()? != 0;
                if !use_bulk {
                    r.take(num_bytes)?;
                }
                let bone_codec = r.fstring()?;
                let curve_codec = r.fstring()?;
                let curve_bytes = native_count(r, "CompressedCurveByteStream")?;
                r.take(curve_bytes)?;
                if trace_enabled() {
                    eprintln!(
                        "anim: bone codec {bone_codec:?} curve codec {curve_codec:?} @ {}",
                        r.o
                    );
                }
                // The bone codec's own payload. CE compresses with ACL, whose
                // `FACLCompressedAnimDataBase::SerializeCompressedData` writes
                // the base `CompressedNumberOfKeys` then `bCompressionFailed`;
                // the compressed clip itself lives in `CompressedByteStream`,
                // already consumed above.
                r.i32()?; // CompressedNumberOfKeys (ICompressedAnimData base)
                if bone_codec.starts_with("AnimBoneCompressionCodec_ACL") {
                    r.u32()?; // FACLCompressedAnimDataBase::bCompressionFailed
                } else if bone_codec.starts_with("AnimCompress_") {
                    // `FUECompressedAnimData`: four `TEnumAsByte` formats, then
                    // three `SerializeView` counts (the payload itself lives in
                    // `CompressedByteStream`, already read) and `StripSize`.
                    r.take(4)?;
                    for _ in 0..3 {
                        r.i32()?;
                    }
                    r.i32()?; // CompressedScaleOffsets.StripSize
                } else {
                    bail!("unmodeled bone compression codec {bone_codec:?}");
                }
                let _ = curve_codec;
                r.u32()?; // UAnimSequence's trailing bTemp
                Ok(())
            })();
            if let Err(e) = ok {
                if trace_enabled() {
                    eprintln!("AnimSequence tail stopped at {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `AActor`: the cooked actor label, then its instance GUID pair.
        "Actor" => {
            if r.u32()? != 0 {
                r.fstring()?;
            }
            r.take(32)?; // FActorInstanceGuid = ActorGuid + ActorInstanceGuid
        }
        _ => {}
    }
    Ok(true)
}

/// `ULevel`'s natively-serialized tail.
pub(super) fn read_level_tail(r: &mut Reader) -> Result<()> {
    let object_array = |r: &mut Reader, what: &str| -> Result<()> {
        let n = native_count(r, what)?;
        r.take(n * 4)?;
        Ok(())
    };
    object_array(r, "Actors")?;
    // `FURL`: protocol, host, map and portal strings, the option list, then the
    // port and a four-byte validity flag.
    for _ in 0..4 {
        r.fstring()?;
    }
    let ops = native_count(r, "URL options")?;
    for _ in 0..ops {
        r.fstring()?;
    }
    r.i32()?; // Port
    r.u32()?; // Valid
    r.i32()?; // Model
    object_array(r, "ModelComponents")?;
    r.i32()?; // LevelScriptActor
    r.i32()?; // NavListStart
    r.i32()?; // NavListEnd
    // `FPrecomputedVisibilityHandler`: bucket origin, cell sizes, bucket counts,
    // then the buckets themselves.
    r.take(16)?; // FVector2D bucket origin
    r.take(16)?; // cell size XY/Z, bucket size XY, bucket count
    let buckets = native_count(r, "visibility buckets")?;
    for _ in 0..buckets {
        r.i32()?; // CellDataSize
        let cells = native_count(r, "visibility cells")?;
        r.take(cells * 28)?; // FVector min + two uint16
        let chunks = native_count(r, "visibility chunks")?;
        for _ in 0..chunks {
            r.u32()?; // bCompressed
            r.i32()?; // UncompressedSize
            let bytes = native_count(r, "visibility chunk data")?;
            r.take(bytes)?;
        }
    }
    // `FPrecomputedVolumeDistanceField`.
    r.f32()?;
    r.take(49)?; // FBox
    r.take(12)?; // volume size X/Y/Z
    let data = native_count(r, "distance field data")?;
    r.take(data * 4)?;
    Ok(())
}

/// `UModel`'s natively-serialized tail.
pub(super) fn read_model_tail(r: &mut Reader) -> Result<()> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    r.take(56)?; // FBoxSphereBounds
    read_bulk_array(r, "Vectors")?;
    read_bulk_array(r, "Points")?;
    read_bulk_array(r, "Nodes")?;
    // `FBspSurf`: two `FPackageIndex`es and six int32s (32 bytes), an
    // `FPlane4f` (16), then `LightMapScale` and `iLightmassIndex` — 56 bytes.
    // `UModel` uses the *float* math variants in UE5, which the stream confirms:
    // its `Vectors`/`Points` bulk arrays have an element size of 12
    // (`FVector3f`), not 24. Using a double-width plane drifts the walk and
    // blows up on the `Verts` element size.
    let surfs = native_count(r, "Surfs")?;
    r.take(surfs * 56)?;
    read_bulk_array(r, "Verts")?;
    r.i32()?; // NumSharedSides
    r.u32()?; // RootOutside
    r.u32()?; // Linked
    r.u32()?; // NumUniqueVertices
    // The vertex buffer is written unless both editor data and the class's
    // vertex-buffer flag are stripped. `FModelVertex` is **56** bytes —
    // `FVector3f` position and tangent X, an `FVector4f` tangent Z, and two
    // `FVector2f` UV pairs — the same float-variant rule the rest of `UModel`
    // follows. The LWC-double reading (112) survived 16,722 models because
    // every one of them has an empty vertex buffer; the two that do not blew
    // up on a 336-vertex buffer.
    if global_strip & 1 == 0 || class_strip & 1 == 0 {
        let verts = native_count(r, "model vertices")?;
        r.take(verts * 56)?;
    }
    r.take(16)?; // LightingGuid
    // `FLightmassPrimitiveSettings`: five four-byte bools and four floats.
    let settings = native_count(r, "LightmassSettings")?;
    r.take(settings * 36)?;
    Ok(())
}

/// A cooked texture tail, skipping the platform data via its `SkipOffset`.
///
/// `UTexture2D`, `UTextureCube`, `UVolumeTexture` and `UTexture2DArray` all
/// write strip flags, a cooked flag, and then `SerializeCookedPlatformData`.
/// The one difference is that **`UTexture2D` alone writes a `bSerializeMipData`
/// flag** between the two (Texture2D.cpp); the other three call the shared
/// serializer directly, so `has_mip_data_flag` selects between them.
pub(super) fn read_texture_tail(r: &mut Reader, has_mip_data_flag: bool) -> Result<()> {
    r.take(2)?; // FStripDataFlags
    let cooked = r.u32()? != 0;
    if !cooked {
        return Ok(());
    }
    if has_mip_data_flag {
        r.u32()?; // bSerializeMipData
    }
    loop {
        let format = r.name()?;
        if format == "None" {
            return Ok(());
        }
        let loc = r.o;
        let skip = r.u64()? as i64;
        let end = loc
            .checked_add_signed(skip as isize)
            .filter(|e| *e > r.o && *e <= r.b.len())
            .with_context(|| format!("implausible texture SkipOffset {skip} @ {loc}"))?;
        r.o = end;
    }
}

/// Consume a material's inline shader maps without decoding them.
///
/// `FMaterialResourceProxyReader`'s header ends with a `NumBytes` giving the
/// total size of the resource data that follows, so the whole block can be
/// skipped — the same trick `Texture2D`'s `SkipOffset` allows. Header layout:
/// the shader-map name table (`FString` + two `uint16` hashes each), the
/// `FMaterialResourceLocOnDisk` table (6 bytes each: offset, feature level,
/// quality level), then `NumBytes`.
pub(super) fn skip_inline_shader_maps(r: &mut Reader) -> Result<()> {
    let resources = r.i32()?;
    if resources <= 0 {
        return Ok(());
    }
    if resources > 1024 {
        bail!("implausible inline shader map resource count {resources}");
    }
    let names = native_count(r, "shader map names")?;
    for _ in 0..names {
        r.fstring()?;
        r.take(4)?; // non-case-preserving + case-preserving hashes
    }
    let locs = native_count(r, "material resource locs")?;
    r.take(locs * 6)?;
    let num_bytes = r.u32()? as usize;
    r.take(num_bytes)?;
    Ok(())
}

/// A `FRawStaticIndexBuffer`: a 32-bit flag, the index bytes as a bulk array,
/// and the "should expand to 32 bit" flag.
pub(super) fn read_raw_static_index_buffer(r: &mut Reader) -> Result<()> {
    let t = trace_enabled();
    let start = r.o;
    let is32 = r.u32()?;
    let n = read_bulk_array(r, "index buffer")?;
    r.u32()?; // bShouldExpandTo32Bit
    if t {
        eprintln!("      idx @ {start}..{} b32={is32} bytes={n}", r.o);
    }
    Ok(())
}

/// `FStaticMeshLODResources::SerializeBuffers` — the vertex and index buffers.
/// Every payload is a bulk array carrying its own element size, so none of the
/// vertex formats need modeling.
pub(super) fn read_static_mesh_buffers(r: &mut Reader, sections: usize) -> Result<()> {
    let t = trace_enabled();
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    if t {
        eprintln!("  buffers @ {} strip {global_strip:#x}/{class_strip:#x}", r.o - 2);
    }
    // FPositionVertexBuffer
    r.i32()?; // Stride
    r.i32()?; // NumVertices
    read_bulk_array(r, "positions")?;
    if t { eprintln!("    after positions @ {}", r.o); }
    // FStaticMeshVertexBuffer
    let vb_strip = r.u8()?;
    r.u8()?;
    r.i32()?; // NumTexCoords
    r.i32()?; // NumVertices
    r.u32()?; // bUseFullPrecisionUVs
    r.u32()?; // bUseHighPrecisionTangentBasis
    if vb_strip & 2 == 0 {
        read_bulk_array(r, "tangents")?;
        if t { eprintln!("    after tangents @ {}", r.o); }
        read_bulk_array(r, "UVs")?;
        if t { eprintln!("    after UVs @ {}", r.o); }
    }
    // FColorVertexBuffer
    let cb_strip = r.u8()?;
    r.u8()?;
    r.i32()?; // Stride
    let colour_verts = r.i32()?;
    if cb_strip & 2 == 0 && colour_verts > 0 {
        read_bulk_array(r, "vertex colours")?;
    }
    if t { eprintln!("    after colours @ {}", r.o); }
    read_raw_static_index_buffer(r)?; // IndexBuffer
    if t { eprintln!("    after index buffer @ {}", r.o); }
    // `CDSF_ReversedIndexBuffer` is bit 2 of the class strip flags.
    if class_strip & 4 == 0 {
        read_raw_static_index_buffer(r)?; // ReversedIndexBuffer
    }
    read_raw_static_index_buffer(r)?; // DepthOnlyIndexBuffer
    if class_strip & 4 == 0 {
        read_raw_static_index_buffer(r)?; // ReversedDepthOnlyIndexBuffer
    }
    if global_strip & 1 == 0 {
        read_raw_static_index_buffer(r)?; // WireframeIndexBuffer (editor only)
    }
    // Per UE 5.5.4 `FStaticMeshLODResources::SerializeBuffers`: the ray-tracing
    // geometry's raw data as a bulk array (unless `CDSF_RayTracingResources` is
    // stripped), then one `FStaticMeshSectionAreaWeightedTriangleSampler` per
    // section and one whole-LOD `AreaWeightedSampler`. Each is an
    // `FWeightedRandomSampler` — 12 bytes when empty, which is why a
    // single-section LOD looked like a fixed 24-byte block.
    if class_strip & 8 == 0 {
        read_bulk_array(r, "ray tracing geometry")?;
    }
    for _ in 0..sections {
        read_weighted_random_sampler(r)?;
    }
    read_weighted_random_sampler(r)?;
    Ok(())
}

/// One `FStaticMeshLODResources`.
///
/// The *prologue* is measured on `SM_Basis_HS`: strip flags, one 40-byte
/// `FStaticMeshSection`, `MaxDeviation`, `bIsLODCookedOut`, `bInlined` and
/// `bHasRayTracingGeometry` put `SerializeBuffers`' own strip flags exactly at
/// 0xd8, where the stream reads `05 00 | Stride 12 | NumVertices 148 |
/// bulk(12 × 148)`.
///
/// **Verified end to end** since `tail_models::StaticMeshTail`: all 15,231
/// `UStaticMesh` exports decode to values and re-encode byte-identically, which
/// a drifting reader cannot do. The warning that used to sit here — that
/// something inside `read_static_mesh_buffers` still drifted, on the evidence of
/// one mesh finishing mid-float — predated the strip-flag conditions being
/// right, and no longer holds.
pub(super) fn read_static_mesh_lod(r: &mut Reader) -> Result<()> {
    let t = trace_enabled();
    let start = r.o;
    let global_strip = r.u8()?;
    let _class_strip = r.u8()?;
    // `FStaticMeshSection`: five int32s then five four-byte flags.
    let sections = native_count(r, "mesh sections")?;
    r.take(sections * 40)?;
    r.f32()?; // MaxDeviation
    let cooked_out = r.u32()? != 0;
    let inlined = r.u32()? != 0;
    if global_strip & 2 == 0 && !cooked_out {
        r.u32()?; // bHasRayTracingGeometry (UE 5.5+)
        if inlined {
            read_static_mesh_buffers(r, sections)?;
        } else {
            r.i32()?; // FByteBulkData index into the package bulk-data map
            r.take(8)?; // DepthOnlyNumTriangles + packed flags
            r.take(72)?; // buffer metadata for each stripped buffer
        }
        // `FStaticMeshBuffersSize` — only for a LOD that actually wrote
        // buffers. A cooked-out LOD ends right after `bInlined`, measured on
        // `SM_UNSC_EscapePods_Exterior_A15_D_Details` where the next LOD's strip
        // flags sit immediately at offset 420.
        r.take(12)?;
    }
    if t {
        eprintln!(
            "LOD @ {start}..{} strip {global_strip:#x} cooked_out {cooked_out} inlined {inlined}",
            r.o
        );
    }
    Ok(())
}

/// `FStaticMeshComponentLODInfo` as written to a cooked, editor-stripped
/// package: strip flags, the map-build-data GUIDs, then the override vertex
/// colour marker.
pub(super) fn read_static_mesh_component_lod_info(r: &mut Reader) -> Result<()> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    // Bit 1 = audio/visual data stripped.
    if global_strip & 2 == 0 {
        // UE 5.5 cooked: MapBuildDataId then OriginalMapBuildDataId.
        r.take(32)?;
    }
    // Class strip bit 0 = override colours stripped. When they are not, a
    // `uint8 bLoadVertexColorData` says whether an `FColorVertexBuffer`
    // follows: its own strip flags, `Stride` and `NumVertices`, then — only
    // when there are vertices and audio-visual data survived — the colours as
    // a bulk array carrying its own element size.
    if class_strip & 1 == 0 && r.u8()? == 1 {
        let colour_global = r.u8()?;
        r.u8()?; // the colour buffer's own class strip flags
        r.i32()?; // Stride
        let vertices = r.i32()?;
        if vertices > 0 && colour_global & 2 == 0 {
            read_bulk_array(r, "OverrideVertexColors")?;
        }
    }
    Ok(())
}
