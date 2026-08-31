//! Writing compiled bytecode + constant tables into `pixel_shader` /
//! `vertex_shader` / `compute_shader` tags, matching the serialization in
//! spec §7/§8 (the "splut", the entry-point reference bookkeeping).
//!
//! `pixel_shader` / `compute_shader` index their `entry points` block by entry
//! ordinal (each holding a `{start index, count}` range into `compiled
//! shaders`). `vertex_shader` nests one level deeper: `entry points[e]` holds a
//! `vertex types` block indexed by vertex-type ordinal, each a `{start,count}`
//! range. Both fill `compiled shaders` in visit order so ranges stay packed.
//!
//! Built on the blam-tags editing facade: clone a stock tag of the right group
//! (so the embedded `blay` layout is inherited verbatim), clear the blocks, and
//! repopulate.

use super::entry::Stage;
use super::reflect::ConstantTable;
use crate::{StringIdData, TagFieldData, TagFile};

/// One entry point's compiled output for one platform slot.
#[derive(Default, Clone)]
pub struct PlatformOutput {
    pub bytecode: Vec<u8>,
    pub table: ConstantTable,
}

/// A compiled shader "splut": per-platform bytecode + constant tables. Xenon is
/// never produced (spec §7d), so only dx9 (PC) and optionally durango are set.
#[derive(Default, Clone)]
pub struct Splut {
    pub dx9: Option<PlatformOutput>,
    pub durango: Option<PlatformOutput>,
    pub gprs: i32,
}

/// One compiled variant, keyed by (entry, vertex type). `pixel`/`compute`
/// ignore the vertex type for indexing (they flatten it into the per-entry
/// count); `vertex` nests by it.
#[derive(Clone)]
pub struct Variant {
    pub entry: usize,
    pub vertex_type: usize,
    pub splut: Splut,
}

/// Backwards-compatible single-entry helper: one entry, its passes in order.
pub struct EntryOutput {
    pub entry: usize,
    pub passes: Vec<Splut>,
}

type R<'a> = crate::TagStructMut<'a>;

fn set_i32(root: &mut R, path: &str, v: i32) {
    if let Some(mut f) = root.field_path_mut(path) {
        let _ = f.set(TagFieldData::LongInteger(v));
    }
}
fn set_flags(root: &mut R, path: &str, v: i32) {
    if let Some(mut f) = root.field_path_mut(path) {
        let _ = f.set(TagFieldData::LongFlags { value: v, names: Vec::new() });
    }
}
fn set_char(root: &mut R, path: &str, v: i8) {
    if let Some(mut f) = root.field_path_mut(path) {
        let _ = f.set(TagFieldData::CharInteger(v));
    }
}

fn set_data(root: &mut R, path: &str, bytes: &[u8]) {
    if let Some(mut f) = root.field_path_mut(path) {
        let _ = f.set(TagFieldData::Data(bytes.to_vec()));
    }
}

fn clear_block(root: &mut R, path: &str) {
    if let Some(mut f) = root.field_path_mut(path) {
        if let Some(mut b) = f.as_block_mut() {
            b.clear();
        }
    }
}

fn grow_block(root: &mut R, path: &str, n: usize) {
    if let Some(mut f) = root.field_path_mut(path) {
        if let Some(mut b) = f.as_block_mut() {
            while b.len() < n {
                b.add_element();
            }
        }
    }
}

fn add_element(root: &mut R, path: &str) -> usize {
    if let Some(mut f) = root.field_path_mut(path) {
        if let Some(mut b) = f.as_block_mut() {
            return b.add_element();
        }
    }
    0
}

/// Fill one `rasterizer_compiled_shader_struct` element (the splut) at `prefix`.
fn set_splut(root: &mut R, prefix: &str, splut: &Splut) {
    if let Some(out) = &splut.dx9 {
        set_data(root, &format!("{prefix}/dx9 compiled shader"), &out.bytecode);
        set_constant_table(root, &format!("{prefix}/dx9 rasterizer constant table"), &out.table);
    }
    if let Some(out) = &splut.durango {
        set_data(root, &format!("{prefix}/durango compiled shader"), &out.bytecode);
        set_constant_table(root, &format!("{prefix}/durango rasterizer constant table"), &out.table);
    }
    set_i32(root, &format!("{prefix}/gprs"), splut.gprs);
}

fn set_constant_table(root: &mut R, prefix: &str, table: &ConstantTable) {
    set_i32(root, &format!("{prefix}/parameter buffer size"), table.parameter_buffer_size);
    set_i32(
        root,
        &format!("{prefix}/extern parameter buffer size"),
        table.extern_parameter_buffer_size,
    );
    if let Some(mut f) = root.field_path_mut(&format!("{prefix}/type")) {
        let _ = f.set(TagFieldData::CharEnum { value: table.table_type, name: None });
    }
    if let Some(mut cf) = root.field_path_mut(&format!("{prefix}/constants")) {
        if let Some(mut block) = cf.as_block_mut() {
            block.clear();
            for c in &table.constants {
                let i = block.add_element();
                if let Some(mut el) = block.element_mut(i) {
                    if let Some(mut f) = el.field_mut("constant name") {
                        let _ = f.set(TagFieldData::StringId(StringIdData { string: c.name.clone() }));
                    }
                    if let Some(mut f) = el.field_mut("register start") {
                        let _ = f.set(TagFieldData::ShortInteger(c.register_start));
                    }
                    if let Some(mut f) = el.field_mut("register count") {
                        let _ = f.set(TagFieldData::CharInteger(c.register_count));
                    }
                    if let Some(mut f) = el.field_mut("register set") {
                        let _ = f.set(TagFieldData::CharEnum { value: c.register_set, name: None });
                    }
                }
            }
        }
    }
}

/// Populate a pixel_shader or compute_shader tag (flat entry-point refs). The
/// variants for one entry are placed consecutively in `compiled shaders`,
/// ordered by vertex type; the entry's ref spans them.
pub fn emit_flat(
    tag: &mut TagFile,
    variants: &[Variant],
    version: i32,
    entry_points_flags: i32,
) -> Result<(), String> {
    let mut root = tag.root_mut();
    set_flags(&mut root, "entry_points", entry_points_flags);
    set_i32(&mut root, "version", version);

    let max_entry = variants.iter().map(|v| v.entry).max().map(|m| m + 1).unwrap_or(0);
    clear_block(&mut root, "entry points");
    clear_block(&mut root, "compiled shaders");
    grow_block(&mut root, "entry points", max_entry);

    let mut compiled_count: usize = 0;
    for e in 0..max_entry {
        let mut mine: Vec<&Variant> = variants.iter().filter(|v| v.entry == e).collect();
        mine.sort_by_key(|v| v.vertex_type);
        if mine.is_empty() {
            continue;
        }
        let start = compiled_count;
        set_char(&mut root, &format!("entry points[{e}]/start index"), start as i8);
        set_char(&mut root, &format!("entry points[{e}]/count"), mine.len() as i8);
        for v in mine {
            let idx = add_element(&mut root, "compiled shaders");
            let prefix = format!("compiled shaders[{idx}]/compiled shader splut");
            set_splut(&mut root, &prefix, &v.splut);
            compiled_count += 1;
        }
    }
    Ok(())
}

/// Populate a vertex_shader tag (entry → vertex-type nested refs).
pub fn emit_vertex(
    tag: &mut TagFile,
    variants: &[Variant],
    version: i32,
    entry_points_flags: i32,
) -> Result<(), String> {
    let mut root = tag.root_mut();
    set_flags(&mut root, "entry_points", entry_points_flags);
    set_i32(&mut root, "version", version);

    let max_entry = variants.iter().map(|v| v.entry).max().map(|m| m + 1).unwrap_or(0);
    clear_block(&mut root, "entry points");
    clear_block(&mut root, "compiled shaders");
    grow_block(&mut root, "entry points", max_entry);

    let mut compiled_count: usize = 0;
    for e in 0..max_entry {
        let mut mine: Vec<&Variant> = variants.iter().filter(|v| v.entry == e).collect();
        mine.sort_by_key(|v| v.vertex_type);
        // Nested "vertex types" block is dense up to the max vertex type present.
        let max_vt = mine.iter().map(|v| v.vertex_type).max().map(|m| m + 1).unwrap_or(0);
        grow_block(&mut root, &format!("entry points[{e}]/vertex types"), max_vt);
        for v in &mine {
            let start = compiled_count;
            set_char(
                &mut root,
                &format!("entry points[{e}]/vertex types[{}]/start index", v.vertex_type),
                start as i8,
            );
            set_char(
                &mut root,
                &format!("entry points[{e}]/vertex types[{}]/count", v.vertex_type),
                1,
            );
            let idx = add_element(&mut root, "compiled shaders");
            let prefix = format!("compiled shaders[{idx}]/compiled shader splut");
            set_splut(&mut root, &prefix, &v.splut);
            compiled_count += 1;
        }
    }
    Ok(())
}

/// Compatibility shim for the single-entry pixel/compute helper.
pub fn emit_flat_shader(
    tag: &mut TagFile,
    entries: &[EntryOutput],
    version: i32,
    entry_points_flags: i32,
) -> Result<(), String> {
    let variants: Vec<Variant> = entries
        .iter()
        .flat_map(|e| {
            e.passes.iter().enumerate().map(move |(i, s)| Variant {
                entry: e.entry,
                vertex_type: i,
                splut: s.clone(),
            })
        })
        .collect();
    emit_flat(tag, &variants, version, entry_points_flags)
}

/// The stage → tag group name for cloning a stock tag.
pub fn stage_group(stage: Stage) -> &'static str {
    match stage {
        Stage::Vertex => "vertex_shader",
        Stage::Pixel => "pixel_shader",
        Stage::Compute => "compute_shader",
    }
}
