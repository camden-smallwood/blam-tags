//! Extracted Halo 5 (`ucsh`) tags using the shared JSON layout and data tree.
//! Native metadata is retained by TagContainer, like classic tag headers.
//! Module/cache readers and presentation do not belong to this adapter.
use crate::data::{
    MAX_BLOCK_NESTING, TagBlockData, TagStructData, TagSubChunkContent, TagSubChunkEntry,
};
use crate::file::TagContainer;
use crate::stream::TagStream;
use crate::{Endian, TagFieldData, TagFieldType, TagFile, TagFileHeader, TagLayout, TagStruct};
use anyhow::{Context, Result, bail, ensure};
use std::collections::HashMap;
use std::ops::Range;

/// The `ucsh` versions read as Halo 5, by [`NativeTag::parse`] and
/// [`looks_like_halo5`] alike.
const SUPPORTED_VERSIONS: [u32; 2] = [23, 27];

#[derive(Clone, Debug)]
enum BindingKind {
    Raw,
    Block(usize),
    Data(usize),
    Reference(Option<(u32, String)>),
    StringId(String),
}
#[derive(Clone, Debug)]
struct Binding {
    offset: usize,
    size: usize,
    kind: BindingKind,
}

#[derive(Clone, Debug)]
struct DataBlock {
    size: usize,
    section: usize,
    offset: usize,
    address: usize,
}
#[derive(Clone, Debug)]
struct Structure {
    guid: [u8; 16],
    kind: u16,
    target: i32,
    host: i32,
    offset: usize,
}
#[derive(Clone, Debug)]
struct TagReference {
    group: u32,
    path: String,
    global_id: u32,
    asset_id: u64,
}
/// Original native bytes and relocation tables, retained for lossless saving.
#[derive(Clone, Debug)]
pub struct NativeTag {
    bytes: Vec<u8>,
    version: u32,
    blocks: Vec<DataBlock>,
    structures: Vec<Structure>,
    root_block: usize,
    strings: HashMap<u32, String>,
    references: HashMap<usize, TagReference>,
    data_targets: HashMap<usize, usize>,
    bindings: HashMap<String, Binding>,
}
impl NativeTag {
    fn parse(bytes: Vec<u8>, root_size: usize) -> Result<Self> {
        ensure!(
            bytes.get(..4) == Some(b"ucsh"),
            "not an extracted Halo 5 ucsh tag"
        );
        ensure!(bytes.len() >= 80, "truncated Halo 5 tag header");
        let version = u32_at(&bytes, 4)?;
        ensure!(
            SUPPORTED_VERSIONS.contains(&version),
            "unsupported Halo 5 tag version {version}"
        );
        let header_size = count(&bytes, 60)?;
        range(0, header_size, bytes.len())?;
        let data_size = count(&bytes, 64)?;
        let resource_size = count(&bytes, 68)?;
        range(
            header_size,
            data_size
                .checked_add(resource_size)
                .context("tag size overflow")?,
            bytes.len(),
        )?;
        let deps = count(&bytes, 28)?;
        let db_count = count(&bytes, 32)?;
        let st_count = count(&bytes, 36)?;
        let mut cursor = table_end(80, deps, 24, header_size)?;
        let db_end = table_end(cursor, db_count, 16, header_size)?;
        let mut blocks = Vec::with_capacity(db_count);
        for offset in (cursor..db_end).step_by(16) {
            let section = usize::from(u16::from_le_bytes(fixed_bytes::<2>(&bytes, offset + 6)?));
            ensure!(section <= 2, "unsupported tag data section {section}");
            blocks.push(DataBlock {
                size: count(&bytes, offset)?,
                section,
                offset: usize::try_from(u64_at(&bytes, offset + 8)?)?,
                address: 0,
            });
        }
        let section_one_size = blocks
            .iter()
            .filter(|b| b.section == 1)
            .try_fold(0usize, |n, b| {
                n.checked_add(b.size).context("section size overflow")
            })?;
        for block in &mut blocks {
            let base = match block.section {
                0 => 0,
                1 => header_size,
                _ => header_size
                    .checked_add(section_one_size)
                    .context("section address overflow")?,
            };
            block.address = base
                .checked_add(block.offset)
                .context("block address overflow")?;
            range(block.address, block.size, bytes.len())?;
        }
        cursor = db_end;
        let st_end = table_end(cursor, st_count, 32, header_size)?;
        let mut structures = Vec::with_capacity(st_count);
        for offset in (cursor..st_end).step_by(32) {
            let target = i32_at(&bytes, offset + 20)?;
            let host = i32_at(&bytes, offset + 24)?;
            ensure!(
                target >= -1 && (target == -1 || (target as usize) < blocks.len()),
                "invalid structure target {target}"
            );
            ensure!(
                host >= -1 && (host == -1 || (host as usize) < blocks.len()),
                "invalid structure host {host}"
            );
            structures.push(Structure {
                guid: fixed_bytes::<16>(&bytes, offset)?,
                kind: u16::from_le_bytes(fixed_bytes::<2>(&bytes, offset + 16)?),
                target,
                host,
                offset: count(&bytes, offset + 28)?,
            });
        }
        let reference_start = table_end(st_end, count(&bytes, 40)?, 20, header_size)?;
        let mut data_targets = HashMap::new();
        for at in (st_end..reference_start).step_by(20) {
            let target = i32_at(&bytes, at + 8)?;
            if target == -1 {
                continue;
            }
            ensure!(
                target >= 0 && (target as usize) < blocks.len(),
                "invalid data target"
            );
            let host = blocks
                .get(count(&bytes, at + 12)?)
                .context("invalid data host")?;
            let offset = count(&bytes, at + 16)?;
            range(offset, 0, host.size)?;
            let address = host
                .address
                .checked_add(offset)
                .context("data field address overflow")?;
            data_targets.insert(address, target as usize);
        }
        let reference_end = table_end(reference_start, count(&bytes, 44)?, 16, header_size)?;
        cursor = reference_end;
        cursor = table_end(cursor, count(&bytes, 48)?, 8, header_size)?;
        let str_end = table_end(cursor, count(&bytes, 52)?, 1, header_size)?;
        let mut strings = HashMap::new();
        strings.insert(0, String::new());
        let string_data = &bytes[cursor..str_end];
        let string_at = |offset: i32| -> Option<String> {
            let offset = usize::try_from(offset).ok()?;
            let tail = string_data.get(offset..)?;
            let end = tail.iter().position(|b| *b == 0)?;
            std::str::from_utf8(&tail[..end]).ok().map(str::to_owned)
        };
        let mut references = HashMap::new();
        for at in (reference_start..reference_end).step_by(16) {
            let dependency = i32_at(&bytes, at + 12)?;
            if dependency == -1 {
                continue;
            }
            ensure!(
                dependency >= 0 && (dependency as usize) < deps,
                "invalid reference dependency"
            );
            let host = count(&bytes, at)?;
            let host = blocks.get(host).context("invalid reference host")?;
            let offset = count(&bytes, at + 4)?;
            range(offset, 32, host.size)?;
            let address = host
                .address
                .checked_add(offset)
                .context("reference address overflow")?;
            let dependency = 80 + dependency as usize * 24;
            let dependency_name = i32_at(&bytes, dependency + 4)?;
            let path = string_at(i32_at(&bytes, at + 8)?)
                .filter(|path| !path.is_empty())
                .or_else(|| string_at(dependency_name));
            if let Some(path) = path.filter(|path| !path.is_empty()) {
                references.insert(
                    address,
                    TagReference {
                        group: u32_at(&bytes, dependency)?,
                        path,
                        global_id: u32_at(&bytes, dependency + 16)?,
                        asset_id: u64_at(&bytes, dependency + 8)?,
                    },
                );
            }
        }
        for text in bytes[cursor..str_end]
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
        {
            if let Ok(text) = std::str::from_utf8(text) {
                strings
                    .entry(murmur3(text.as_bytes()))
                    .or_insert_with(|| text.to_owned());
            }
        }
        let mains: Vec<_> = structures
            .iter()
            .filter(|s| s.kind == 0 && s.target >= 0)
            .collect();
        ensure!(
            mains.len() == 1,
            "expected one main tag structure, found {}",
            mains.len()
        );
        let root_block = mains[0].target as usize;
        ensure!(
            blocks[root_block].section == 1,
            "main tag structure is outside tag data"
        );
        ensure!(
            blocks[root_block].size == root_size,
            "layout size mismatch: JSON describes {} bytes, tag root has {}; editing refused",
            root_size,
            blocks[root_block].size
        );
        Ok(Self {
            bytes,
            version,
            blocks,
            structures,
            root_block,
            strings,
            references,
            data_targets,
            bindings: HashMap::new(),
        })
    }

    fn block(
        &mut self,
        layout: &TagLayout,
        index: u32,
        target: Option<usize>,
        count: usize,
        prefix: &str,
        depth: u32,
        claimed: &mut [bool],
    ) -> Result<TagBlockData> {
        ensure!(
            depth <= MAX_BLOCK_NESTING,
            "blocks nest more than {MAX_BLOCK_NESTING} deep"
        );
        let si = layout.block_layouts[index as usize].struct_index;
        let size = layout.struct_layouts[si as usize].size;
        let (address, native_index) = if count == 0 {
            (0, 0)
        } else {
            let target = target.context("nonempty block has no relocation target")?;
            let b = &self.blocks[target];
            ensure!(
                b.section == 1 && count.checked_mul(size) == Some(b.size),
                "block JSON layout does not match native data"
            );
            // Each data block holds one block's elements, as each block's
            // bytes are its own in an MCC tag; a second claim is a cycle or
            // a shared target that would decode the same elements again.
            ensure!(
                !std::mem::replace(&mut claimed[target], true),
                "native data block {target} is read by more than one tag block"
            );
            (b.address, target)
        };
        let mut raw = self.bytes[range(
            address,
            count.checked_mul(size).context("block size overflow")?,
            self.bytes.len(),
        )?]
        .to_vec();
        let mut elements = Vec::new();
        for i in 0..count {
            let path = if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}[{i}]")
            };
            elements.push(self.structure(
                layout,
                si,
                native_index,
                i * size,
                &mut raw[i * size..(i + 1) * size],
                &path,
                depth,
                claimed,
            )?);
        }
        Ok(TagBlockData {
            block_index: index,
            flags: 0,
            raw_data: raw,
            endian: Endian::Le,
            elements,
            classic_block_header: None,
            classic_structural_dirty: false,
            classic_trailing: None,
        })
    }
    fn structure(
        &mut self,
        layout: &TagLayout,
        si: u32,
        native_index: usize,
        base: usize,
        raw: &mut [u8],
        prefix: &str,
        depth: u32,
        claimed: &mut [bool],
    ) -> Result<TagStructData> {
        let first = layout.struct_layouts[si as usize].first_field_index as usize;
        let mut sub_chunks = Vec::new();
        for (ordinal, f) in layout.fields[first..]
            .iter()
            .take_while(|f| f.field_type != TagFieldType::Terminator)
            .enumerate()
        {
            let segment = format!("#{ordinal}");
            let path = if prefix.is_empty() {
                segment
            } else {
                format!("{prefix}/{segment}")
            };
            let off = f.offset as usize;
            let address = self.blocks[native_index].address + base + off;
            let size = if matches!(
                f.field_type,
                TagFieldType::Pad
                    | TagFieldType::Skip
                    | TagFieldType::UselessPad
                    | TagFieldType::Custom
            ) {
                f.definition as usize
            } else {
                layout.field_types[f.type_index as usize].size as usize
            };
            let mut binding = Binding {
                offset: address,
                size,
                kind: BindingKind::Raw,
            };
            let content = match f.field_type {
                TagFieldType::Struct => {
                    let len = layout.struct_layouts[f.definition as usize].size;
                    let region = range(off, len, raw.len())?;
                    Some(TagSubChunkContent::Struct(self.structure(
                        layout,
                        f.definition,
                        native_index,
                        base + off,
                        &mut raw[region],
                        &path,
                        depth,
                        claimed,
                    )?))
                }
                TagFieldType::Array => {
                    let array = &layout.array_layouts[f.definition as usize];
                    let size = layout.struct_layouts[array.struct_index as usize].size;
                    let mut elements = Vec::new();
                    for i in 0..array.count as usize {
                        let start = off + i * size;
                        let region = range(start, size, raw.len())?;
                        elements.push(self.structure(
                            layout,
                            array.struct_index,
                            native_index,
                            base + start,
                            &mut raw[region],
                            &format!("{path}[{i}]"),
                            depth,
                            claimed,
                        )?);
                    }
                    Some(TagSubChunkContent::Array(elements))
                }
                TagFieldType::Block => {
                    let n = count(&self.bytes, address + 16)?;
                    let target = self
                        .structures
                        .iter()
                        .find(|s| {
                            s.kind == 1 && s.host == native_index as i32 && s.offset == base + off
                        })
                        .and_then(|s| usize::try_from(s.target).ok());
                    binding.kind = BindingKind::Block(n);
                    Some(TagSubChunkContent::Block(self.block(
                        layout,
                        f.definition,
                        target,
                        n,
                        &path,
                        depth + 1,
                        claimed,
                    )?))
                }
                TagFieldType::Data => {
                    let len = count(&self.bytes, address + 24)?;
                    let start = if len == 0 {
                        0
                    } else {
                        let b = &self.blocks[*self
                            .data_targets
                            .get(&address)
                            .context("missing data relocation")?];
                        range(0, len, b.size)?;
                        b.address
                    };
                    binding.kind = BindingKind::Data(len);
                    binding.offset = start;
                    binding.size = len;
                    Some(TagSubChunkContent::Data(
                        self.bytes[range(start, len, self.bytes.len())?].to_vec(),
                    ))
                }
                TagFieldType::TagReference => {
                    let id = u32_at(&self.bytes, address + 12)?;
                    let reference = if id == u32::MAX {
                        None
                    } else {
                        let group = u32_at(&self.bytes, address + 24)?;
                        let asset = u64_at(&self.bytes, address + 16)?;
                        if let Some(r) = self.references.get(&address) {
                            ensure!(
                                r.global_id == id && r.asset_id == asset && r.group == group,
                                "reference metadata mismatch"
                            );
                            Some((group, r.path.clone()))
                        } else {
                            Some((group, format!("<unresolved tag 0x{id:08X}>")))
                        }
                    };
                    let mut data = Vec::new();
                    if let Some((group, name)) = &reference {
                        data.extend(group.to_le_bytes());
                        data.extend(name.as_bytes());
                        data.push(0);
                        raw[off..off + 4].copy_from_slice(&group.to_le_bytes());
                    }
                    binding.kind = BindingKind::Reference(reference);
                    Some(TagSubChunkContent::TagReference(data))
                }
                TagFieldType::StringId | TagFieldType::OldStringId => {
                    let id = u32_at(&self.bytes, address)?;
                    let text = self
                        .strings
                        .get(&id)
                        .cloned()
                        .unwrap_or_else(|| format!("0x{id:08X}"));
                    binding.kind = BindingKind::StringId(text.clone());
                    Some(if f.field_type == TagFieldType::StringId {
                        TagSubChunkContent::StringId(text.into_bytes())
                    } else {
                        TagSubChunkContent::OldStringId(text.into_bytes())
                    })
                }
                _ => None,
            };
            if !matches!(f.field_type, TagFieldType::Struct | TagFieldType::Array) {
                self.bindings.insert(path, binding);
            }
            if let Some(content) = content {
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some((first + ordinal) as u32),
                    content,
                });
            }
        }
        Ok(TagStructData {
            struct_index: si,
            classic_struct_header: None,
            sub_chunks,
        })
    }
    pub(crate) fn write(&self, tag: &TagFile) -> std::io::Result<Vec<u8>> {
        let mut output = self.bytes.clone();
        self.patch(tag.root(), "", &mut output)
            .map_err(std::io::Error::other)?;
        Ok(output)
    }
    fn patch(&self, value: TagStruct<'_>, prefix: &str, output: &mut [u8]) -> Result<()> {
        for f in value.fields_all() {
            let path = if prefix.is_empty() {
                format!("#{}", f.ordinal())
            } else {
                format!("{prefix}/#{}", f.ordinal())
            };
            if let Some(b) = self.bindings.get(&path) {
                match &b.kind {
                    BindingKind::Block(n) => ensure!(
                        f.as_block().context("block type changed")?.len() == *n,
                        "Halo 5 block resizing requires rebuilding native relocations"
                    ),
                    BindingKind::Data(len) => {
                        let data = f.as_data().context("data type changed")?;
                        ensure!(
                            data.len() == *len,
                            "Halo 5 data resizing requires rebuilding native relocations"
                        );
                        let region = range(b.offset, *len, output.len())?;
                        output[region].copy_from_slice(data);
                    }
                    BindingKind::Reference(original) => {
                        let Some(TagFieldData::TagReference(r)) = f.value() else {
                            bail!("reference type changed")
                        };
                        ensure!(
                            r.group_tag_and_name == *original,
                            "Halo 5 reference reassignment requires rebuilding dependency metadata"
                        );
                    }
                    BindingKind::StringId(original) => {
                        let text = match f.value() {
                            Some(TagFieldData::StringId(s) | TagFieldData::OldStringId(s)) => {
                                s.string
                            }
                            _ => bail!("string-id type changed"),
                        };
                        ensure!(
                            text == *original,
                            "Halo 5 string-id changes require rebuilding the string table"
                        );
                    }
                    BindingKind::Raw => {
                        let off = f.definition().offset() as usize;
                        let width = f.definition().wire_width() as usize;
                        if width > 0 {
                            ensure!(width == b.size, "field width changed");
                            let region = range(b.offset, width, output.len())?;
                            output[region].copy_from_slice(
                                &value.raw()[range(off, width, value.raw().len())?],
                            );
                        }
                    }
                }
            }
            if let Some(child) = f.as_struct() {
                self.patch(child, &path, output)?;
            } else if let Some(block) = f.as_block() {
                for (i, child) in block.iter().enumerate() {
                    self.patch(child, &format!("{path}[{i}]"), output)?;
                }
            } else if let Some(array) = f.as_array() {
                for (i, child) in array.iter().enumerate() {
                    self.patch(child, &format!("{path}[{i}]"), output)?;
                }
            }
        }
        Ok(())
    }
}

/// Decode a native tag with an external layout, as `read_classic_tag_file` does.
pub fn read_halo5_tag_file(bytes: &[u8], layout: TagLayout, group_tag: u32) -> Result<TagFile> {
    let root = layout.header.tag_group_block_index;
    let root_size =
        layout.struct_layouts[layout.block_layouts[root as usize].struct_index as usize].size;
    let mut native = NativeTag::parse(bytes.to_vec(), root_size)?;
    let root_guid =
        layout.struct_layouts[layout.block_layouts[root as usize].struct_index as usize].guid;
    let native_guid = native
        .structures
        .iter()
        .find(|s| s.kind == 0 && s.target == native.root_block as i32)
        .context("missing root structure")?
        .guid;
    ensure!(
        native_guid == root_guid,
        "Halo 5 root GUID does not match the supplied definitions"
    );
    let data = native.block(
        &layout,
        root,
        Some(native.root_block),
        1,
        "",
        0,
        &mut vec![false; native.blocks.len()],
    )?;
    Ok(TagFile::from_parts(
        TagFileHeader {
            pad: [0; 36],
            build_version: 0,
            build_number: 0,
            version: native.version,
            group_tag,
            group_version: 0,
            checksum: 0,
            signature: u32::from_be_bytes(*b"BLAM"),
        },
        TagContainer::Halo5(Box::new(native)),
        Endian::Le,
        TagStream { layout, data },
    ))
}

/// Header-only detection; Infinite's newer ucsh versions are not Halo 5.
pub fn looks_like_halo5(bytes: &[u8]) -> bool {
    bytes.get(..4) == Some(b"ucsh")
        && bytes.get(4..8).is_some_and(|b| {
            SUPPORTED_VERSIONS.contains(&u32::from_le_bytes(b.try_into().unwrap()))
        })
}
fn range(offset: usize, size: usize, length: usize) -> Result<Range<usize>> {
    let end = offset.checked_add(size).context("byte range overflow")?;
    ensure!(
        end <= length,
        "byte range {offset:#x}..{end:#x} exceeds {length:#x} bytes"
    );
    Ok(offset..end)
}
fn fixed_bytes<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    Ok(data[range(offset, N, data.len())?].try_into().unwrap())
}
fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(fixed_bytes(data, offset)?))
}
fn i32_at(data: &[u8], offset: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(fixed_bytes(data, offset)?))
}
fn count(data: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(i32_at(data, offset)?).context("negative table count")
}
fn u64_at(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(fixed_bytes(data, offset)?))
}
fn table_end(start: usize, count: usize, stride: usize, length: usize) -> Result<usize> {
    let size = count.checked_mul(stride).context("table size overflow")?;
    Ok(range(start, size, length)?.end)
}

// Halo's string-id hash (MurmurHash3 x86, seed 0).
fn murmur3(bytes: &[u8]) -> u32 {
    let mix = |mut k: u32| {
        k = k.wrapping_mul(0xcc9e2d51);
        k = k.rotate_left(15);
        k.wrapping_mul(0x1b873593)
    };
    let mut h = 0u32;
    let (chunks, remainder) = bytes.as_chunks::<4>();
    for c in chunks {
        h ^= mix(u32::from_le_bytes(*c));
        h = h.rotate_left(13).wrapping_mul(5).wrapping_add(0xe6546b64);
    }
    let mut tail = 0u32;
    for (i, &b) in remainder.iter().enumerate() {
        tail |= u32::from(b) << (8 * i)
    }
    if !remainder.is_empty() {
        h ^= mix(tail)
    }
    h ^= bytes.len() as u32;
    h ^= h >> 16;
    h = h.wrapping_mul(0x85ebca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2ae35);
    h ^ (h >> 16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    fn definitions() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions/halo5_guardians")
    }
    fn native_bytes(layout: &TagLayout) -> Vec<u8> {
        let root = &layout.struct_layouts[layout.block_layouts
            [layout.header.tag_group_block_index as usize]
            .struct_index as usize];
        let mut bytes = vec![0u8; 128 + root.size];
        bytes[..4].copy_from_slice(b"ucsh");
        for (offset, value) in [
            (4, 27u32),
            (32, 1),
            (36, 1),
            (60, 128),
            (64, root.size as u32),
            (80, root.size as u32),
            (120, u32::MAX),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[86] = 1;
        bytes[96..112].copy_from_slice(&root.guid);
        bytes
    }
    /// A tag whose root's first two block fields hold elements, in one
    /// native data block both point at, or in a data block each.
    fn two_root_blocks(shared: bool) -> (Vec<u8>, TagLayout, u32) {
        for entry in std::fs::read_dir(definitions()).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().unwrap() == "_meta.json" {
                continue;
            }
            let (layout, meta) = TagLayout::from_json_with_meta(&path).unwrap();
            let root = &layout.struct_layouts[layout.block_layouts
                [layout.header.tag_group_block_index as usize]
                .struct_index as usize];
            let blocks: Vec<(usize, usize)> = layout.fields[root.first_field_index as usize..]
                .iter()
                .take_while(|f| f.field_type != TagFieldType::Terminator)
                .filter(|f| f.field_type == TagFieldType::Block)
                .map(|f| {
                    let element = layout.block_layouts[f.definition as usize].struct_index;
                    (
                        f.offset as usize,
                        layout.struct_layouts[element as usize].size,
                    )
                })
                .filter(|&(_, size)| size > 0)
                .take(2)
                .collect();
            let [(first, a), (second, b)] = blocks[..] else {
                continue;
            };
            // Shared: a*b bytes read as b elements of a and a elements of b.
            let (sizes, fields) = if shared {
                (vec![root.size, a * b], [(first, 1, b), (second, 1, a)])
            } else {
                (vec![root.size, a, b], [(first, 1, 1), (second, 2, 1)])
            };
            let header = 80 + 16 * sizes.len() + 32 * (1 + fields.len());
            let mut bytes = vec![0u8; header + sizes.iter().sum::<usize>()];
            fn put(bytes: &mut [u8], at: usize, value: u32) {
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes())
            }
            put(&mut bytes, 0, u32::from_le_bytes(*b"ucsh"));
            put(&mut bytes, 4, 27);
            put(&mut bytes, 32, sizes.len() as u32);
            put(&mut bytes, 36, 1 + fields.len() as u32);
            put(&mut bytes, 60, header as u32);
            put(&mut bytes, 64, sizes.iter().sum::<usize>() as u32);
            let mut section_offset = 0;
            for (i, size) in sizes.iter().enumerate() {
                put(&mut bytes, 80 + 16 * i, *size as u32);
                bytes[80 + 16 * i + 6] = 1;
                bytes[80 + 16 * i + 8..80 + 16 * i + 16]
                    .copy_from_slice(&(section_offset as u64).to_le_bytes());
                section_offset += size;
            }
            let structures = 80 + 16 * sizes.len();
            bytes[structures..structures + 16].copy_from_slice(&root.guid);
            put(&mut bytes, structures + 24, u32::MAX);
            for (i, (offset, target, count)) in fields.into_iter().enumerate() {
                let at = structures + 32 * (1 + i);
                bytes[at + 16] = 1;
                put(&mut bytes, at + 20, target);
                put(&mut bytes, at + 28, offset as u32);
                put(&mut bytes, header + offset + 16, count as u32);
            }
            return (bytes, layout, meta.tag);
        }
        panic!("no group's root has two block fields with elements");
    }
    #[test]
    fn a_native_data_block_is_read_by_one_tag_block() {
        let (bytes, layout, group) = two_root_blocks(false);
        let tag = read_halo5_tag_file(&bytes, layout, group).unwrap();
        assert_eq!(tag.write_to_bytes().unwrap(), bytes);
        let (bytes, layout, group) = two_root_blocks(true);
        let error = read_halo5_tag_file(&bytes, layout, group).unwrap_err();
        assert!(
            error.to_string().contains("more than one tag block"),
            "{error}"
        );
    }
    #[test]
    fn fixed_arrays_edit_through_the_shared_api_and_wrong_root_layout_is_refused() {
        let (layout, meta) = TagLayout::from_json_with_meta(
            definitions().join("multilingual_unicode_string_list.json"),
        )
        .unwrap();
        let original = native_bytes(&layout);
        let mut tag = read_halo5_tag_file(&original, layout.clone(), meta.tag).unwrap();
        let array_field = tag
            .root()
            .fields()
            .find(|f| f.as_array().is_some())
            .unwrap();
        let element = array_field.as_array().unwrap().iter().nth(1).unwrap();
        let scalar = element
            .fields()
            .find(|f| f.type_name() == "short integer")
            .unwrap();
        let path = crate::TagFieldPath::default()
            .with_field(&array_field)
            .with_index(1)
            .with_field(&scalar)
            .to_string();
        tag.root_mut()
            .field_path_mut(&path)
            .unwrap()
            .set(TagFieldData::ShortInteger(7))
            .unwrap();
        let saved = tag.write_to_bytes().unwrap();
        assert_ne!(saved, original);
        let reopened = read_halo5_tag_file(&saved, layout.clone(), meta.tag).unwrap();
        assert!(matches!(
            reopened.root().field_path(&path).unwrap().value(),
            Some(TagFieldData::ShortInteger(7))
        ));
        let mut wrong_guid = original;
        wrong_guid[96] ^= 1;
        assert!(read_halo5_tag_file(&wrong_guid, layout, meta.tag).is_err());
    }
    #[test]
    fn all_json_definitions_use_the_shared_layout_importer() {
        let mut count = 0;
        for entry in std::fs::read_dir(definitions()).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|x| x == "json")
                && path.file_name().unwrap() != "_meta.json"
            {
                let (layout, meta) = TagLayout::from_json_with_meta(&path)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                let bytes = native_bytes(&layout);
                let tag = read_halo5_tag_file(&bytes, layout, meta.tag)
                    .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                assert_eq!(tag.write_to_bytes().unwrap(), bytes, "{}", path.display());
                count += 1;
            }
        }
        // Every group the dump lists, so a missing file fails here.
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(definitions().join("_meta.json")).unwrap())
                .unwrap();
        assert_eq!(count, meta["tag_index"].as_object().unwrap().len());
    }
    #[test]
    fn header_detection_rejects_other_ucsh_generations_and_truncation() {
        assert!(!looks_like_halo5(b"ucsh"));
        for version in [23u32, 27, 53] {
            let bytes = [b"ucsh".as_slice(), version.to_le_bytes().as_slice()].concat();
            assert_eq!(looks_like_halo5(&bytes), version != 53);
            let layout = TagLayout::from_json(definitions().join("biped.json")).unwrap();
            assert!(read_halo5_tag_file(&bytes, layout, u32::from_be_bytes(*b"bipd")).is_err());
        }
    }
    #[test]
    #[ignore = "requires an extracted Halo 5 tag dump; set HALO5_TEST_DUMP"]
    fn xbox_character_tag_groups_roundtrip_with_executable_definitions() {
        let Some(root) = std::env::var_os("HALO5_TEST_DUMP") else {
            eprintln!("skipping: set HALO5_TEST_DUMP to an extracted Halo 5 tag dump");
            return;
        };
        let root = PathBuf::from(root).join("tag_dump/objects/characters/grunt_mecha");
        let mut count = 0;
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_file() {
                continue;
            }
            let Some(extension) = path.extension().and_then(|s| s.to_str()) else {
                continue;
            };
            let schema = definitions().join(format!("{extension}.json"));
            if !schema.is_file() {
                continue;
            }
            let (layout, meta) = TagLayout::from_json_with_meta(schema).unwrap();
            let original = std::fs::read(&path).unwrap();
            let tag = read_halo5_tag_file(&original, layout, meta.tag)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                tag.write_to_bytes().unwrap(),
                original,
                "{}",
                path.display()
            );
            count += 1;
        }
        assert!(count >= 8, "too few real tag groups: {count}");
    }
    #[test]
    #[ignore = "requires an extracted Halo 5 tag dump; set HALO5_TEST_DUMP"]
    fn xbox_biped_shared_fields_native_roundtrip_and_atomic_edit() {
        let Some(root) = std::env::var_os("HALO5_TEST_DUMP") else {
            eprintln!("skipping: set HALO5_TEST_DUMP to an extracted Halo 5 tag dump");
            return;
        };
        let path =
            PathBuf::from(root).join("tag_dump/objects/characters/grunt_mecha/grunt_mecha.biped");
        let bytes = std::fs::read(path).unwrap();
        let (layout, meta) =
            TagLayout::from_json_with_meta(definitions().join("biped.json")).unwrap();
        let mut tag = read_halo5_tag_file(&bytes, layout, meta.tag).unwrap();
        assert_eq!(tag.write_to_bytes().unwrap(), bytes);
        let TagContainer::Halo5(native) = &tag.container else {
            panic!()
        };
        let mut functions = 0;
        let mut references = 0;
        let mut candidate = None;
        fn visit(
            value: TagStruct<'_>,
            prefix: &str,
            native_prefix: &str,
            native: &NativeTag,
            functions: &mut usize,
            references: &mut usize,
            candidate: &mut Option<(String, usize)>,
        ) {
            for f in value.fields_all() {
                let path = prefix
                    .parse::<crate::TagFieldPath>()
                    .unwrap()
                    .with_field(&f)
                    .to_string();
                let key = if native_prefix.is_empty() {
                    format!("#{}", f.ordinal())
                } else {
                    format!("{native_prefix}/#{}", f.ordinal())
                };
                if f.is_function_data() && !f.as_data().unwrap().is_empty() {
                    assert!(f.as_function().is_some(), "{path}");
                    *functions += 1;
                }
                if let Some(TagFieldData::TagReference(r)) = f.value() {
                    if r.group_tag_and_name.is_some() {
                        *references += 1;
                    }
                }
                if path.contains("[0]") && f.type_name() == "real" && !path.contains("up/down") {
                    *candidate = Some((path.clone(), native.bindings[&key].offset));
                }
                if let Some(child) = f.as_struct() {
                    visit(child, &path, &key, native, functions, references, candidate);
                } else if let Some(block) = f.as_block() {
                    for (i, child) in block.iter().enumerate() {
                        visit(
                            child,
                            &format!("{path}[{i}]"),
                            &format!("{key}[{i}]"),
                            native,
                            functions,
                            references,
                            candidate,
                        );
                    }
                }
            }
        }
        visit(
            tag.root(),
            "",
            "",
            native,
            &mut functions,
            &mut references,
            &mut candidate,
        );
        assert!(functions >= 14, "{functions} functions");
        assert!(references > 0);
        let (path, offset) = candidate.expect("nested scalar");
        tag.root_mut()
            .field_path_mut(&path)
            .unwrap()
            .set(TagFieldData::Real(2.5))
            .unwrap();
        let mut expected = bytes.clone();
        expected[offset..offset + 4].copy_from_slice(&2.5f32.to_le_bytes());
        assert_eq!(tag.write_to_bytes().unwrap(), expected);
        let output =
            std::env::temp_dir().join(format!("halo5-rewrite-{}.biped", std::process::id()));
        tag.write_atomic(&output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), expected);
        std::fs::remove_file(output).unwrap();
        let snapshot = tag.write_to_bytes().unwrap();
        assert_eq!(
            read_halo5_tag_file(&snapshot, tag.tag_stream.layout.clone(), meta.tag)
                .unwrap()
                .write_to_bytes()
                .unwrap(),
            snapshot
        );
        let mut rejected =
            read_halo5_tag_file(&snapshot, tag.tag_stream.layout.clone(), meta.tag).unwrap();
        let block_path = rejected
            .root()
            .fields()
            .find(|f| f.as_block().is_some())
            .map(|f| crate::TagFieldPath::default().with_field(&f).to_string())
            .unwrap();
        rejected
            .root_mut()
            .field_path_mut(&block_path)
            .unwrap()
            .as_block_mut()
            .unwrap()
            .add_element();
        let protected =
            std::env::temp_dir().join(format!("halo5-protected-{}.biped", std::process::id()));
        std::fs::write(&protected, &snapshot).unwrap();
        assert!(rejected.write_atomic(&protected).is_err());
        assert_eq!(std::fs::read(&protected).unwrap(), snapshot);
        std::fs::remove_file(protected).unwrap();
    }
}
