use super::{
    binary::*,
    schema::{Schema, SchemaField, ValueKind},
};
use anyhow::{Context, Result, bail, ensure};
use std::{collections::HashMap, path::Path, sync::Arc};

#[derive(Clone, Debug)]
struct DataBlock {
    size: usize,
    section: usize,
    offset: usize,
    address: usize,
}
#[derive(Clone, Debug)]
struct Structure {
    kind: u16,
    target: i32,
    host: i32,
    offset: usize,
}
#[derive(Clone, Debug)]
pub enum FieldKind {
    Value(ValueKind),
    Inline(Vec<Field>),
    Block {
        target: Option<usize>,
        count: usize,
        element_size: usize,
        children: Vec<SchemaField>,
    },
    Data {
        target: Option<usize>,
        size: usize,
    },
    Opaque,
}
#[derive(Clone, Debug)]
pub struct Field {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub kind: FieldKind,
    pub editable: bool,
}
#[derive(Clone, Debug)]
pub struct TagReference {
    pub group: u32,
    pub path: String,
    pub global_id: u32,
    pub asset_id: u64,
}
/// Keeps every unedited byte, including unknown header and resource data.
pub struct Document {
    bytes: Vec<u8>,
    pub schema: Arc<Schema>,
    pub version: u32,
    header_size: usize,
    blocks: Vec<DataBlock>,
    structures: Vec<Structure>,
    pub root_block: usize,
    strings: HashMap<u32, String>,
    references: HashMap<usize, TagReference>,
    data_targets: HashMap<usize, usize>,
}
impl Document {
    pub fn open(path: &Path, schema: Arc<Schema>) -> Result<Self> {
        ensure!(
            std::fs::metadata(path)?.len() <= 256 * 1024 * 1024,
            "tag exceeds the 256 MiB editor limit"
        );
        Self::parse(std::fs::read(path)?, schema)
    }
    pub fn parse(bytes: Vec<u8>, schema: Arc<Schema>) -> Result<Self> {
        ensure!(
            bytes.get(..4) == Some(b"ucsh"),
            "not an extracted Halo 5 ucsh tag"
        );
        ensure!(bytes.len() >= 80, "truncated Halo 5 tag header");
        let version = u32_at(&bytes, 4)?;
        ensure!(
            matches!(version, 23 | 27),
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
            let section = usize::from(u16::from_le_bytes(super::binary::bytes::<2>(
                &bytes,
                offset + 6,
            )?));
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
                kind: u16::from_le_bytes(super::binary::bytes::<2>(&bytes, offset + 16)?),
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
            blocks[root_block].size == schema.size,
            "layout size mismatch: XML describes {} bytes, tag root has {}; editing refused",
            schema.size,
            blocks[root_block].size
        );
        Ok(Self {
            bytes,
            schema,
            version,
            header_size,
            blocks,
            structures,
            root_block,
            strings,
            references,
            data_targets,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn data(&self, field: &Field) -> Result<&[u8]> {
        let FieldKind::Data { target, size } = field.kind else {
            bail!("not a data field")
        };
        if size == 0 {
            return Ok(&[]);
        }
        let target = self
            .blocks
            .get(target.context("data field has no metadata target")?)
            .context("invalid data target")?;
        range(0, size, target.size)?;
        Ok(&self.bytes[range(target.address, size, self.bytes.len())?])
    }
    /// Native metadata carries reference names independently of the runtime
    /// IDs in the field. Null references have no target.
    pub fn tag_reference(&self, field: &Field) -> Result<Option<TagReference>> {
        ensure!(
            matches!(field.kind, FieldKind::Value(ValueKind::Reference)),
            "not a tag reference"
        );
        let data = &self.bytes[range(field.offset, field.size, self.bytes.len())?];
        let global_id = u32_at(data, 12)?;
        let asset_id = u64_at(data, 16)?;
        let group = u32_at(data, 24)?;
        if global_id == u32::MAX {
            return Ok(None);
        }
        if let Some(reference) = self.references.get(&field.offset) {
            ensure!(
                reference.global_id == global_id
                    && reference.asset_id == asset_id
                    && reference.group == group,
                "reference metadata does not match its field"
            );
            return Ok(Some(reference.clone()));
        }
        // Keep an unresolved non-null ID visible rather than presenting it as
        // an empty reference. Opening this descriptor will report a missing tag.
        Ok(Some(TagReference {
            group,
            global_id,
            asset_id,
            path: format!("<unresolved tag 0x{global_id:08X}>"),
        }))
    }
    pub fn string_id(&self, field: &Field) -> Result<String> {
        ensure!(
            matches!(field.kind, FieldKind::Value(ValueKind::StringId)),
            "not a string ID"
        );
        let hash = u32_at(&self.bytes, field.offset)?;
        Ok(self
            .strings
            .get(&hash)
            .cloned()
            .unwrap_or_else(|| format!("0x{hash:08X}")))
    }
    /// Apply fixed-width field bytes from a format-independent editor model.
    /// Metadata and every byte outside the listed fields remain unchanged.
    pub fn with_field_bytes(&self, edits: &[(Field, Vec<u8>)]) -> Result<Self> {
        let mut bytes = self.bytes.clone();
        for (field, value) in edits {
            ensure!(
                field.editable && value.len() == field.size,
                "invalid field write"
            );
            ensure!(
                field.offset >= self.header_size,
                "metadata cannot be edited"
            );
            let r = range(field.offset, field.size, bytes.len())?;
            bytes[r].copy_from_slice(value);
        }
        Self::parse(bytes, self.schema.clone())
    }
    pub fn root_fields(&self) -> Result<Vec<Field>> {
        self.fields_at(self.root_block, 0, &self.schema.fields)
    }
    pub fn fields_at(
        &self,
        block_index: usize,
        base: usize,
        schema: &[SchemaField],
    ) -> Result<Vec<Field>> {
        let block = self
            .blocks
            .get(block_index)
            .context("invalid block index")?;
        ensure!(
            block.section == 1,
            "resource structures cannot be edited as tag fields"
        );
        let mut result = Vec::with_capacity(schema.len());
        for s in schema {
            let relative = base
                .checked_add(s.offset)
                .context("field offset overflow")?;
            range(relative, s.size, block.size)?;
            let offset = block
                .address
                .checked_add(relative)
                .context("field address overflow")?;
            let kind = if s.is_block {
                let count = count(&self.bytes, offset + 16)?;
                let target = self
                    .structures
                    .iter()
                    .find(|v| v.kind == 1 && v.host == block_index as i32 && v.offset == relative)
                    .and_then(|v| usize::try_from(v.target).ok());
                if count > 0 {
                    let target_block = self
                        .blocks
                        .get(target.context("nonempty tag block has no metadata target")?)
                        .context("invalid tag block target")?;
                    ensure!(
                        s.element_size > 0
                            && count.checked_mul(s.element_size) == Some(target_block.size),
                        "{} block layout mismatch ({} elements, {} bytes each, {} stored bytes)",
                        s.name,
                        count,
                        s.element_size,
                        target_block.size
                    );
                    ensure!(
                        target_block.section == 1,
                        "{} points outside tag data",
                        s.name
                    );
                }
                FieldKind::Block {
                    target,
                    count,
                    element_size: s.element_size,
                    children: s.children.clone(),
                }
            } else if !s.children.is_empty() {
                FieldKind::Inline(self.fields_at(block_index, relative, &s.children)?)
            } else if s.kind == ValueKind::Data {
                let size = count(&self.bytes, offset + 24)?;
                let target = self.data_targets.get(&offset).copied();
                if size > 0 {
                    let block = self
                        .blocks
                        .get(target.context("nonempty data field has no metadata target")?)
                        .context("invalid data target")?;
                    range(0, size, block.size)?;
                }
                FieldKind::Data { target, size }
            } else if s.kind == ValueKind::Opaque {
                FieldKind::Opaque
            } else {
                FieldKind::Value(s.kind.clone())
            };
            let editable = matches!(kind, FieldKind::Value(_))
                && !matches!(s.kind, ValueKind::StringId | ValueKind::Reference)
                && !s.name.contains('!')
                && offset >= self.header_size;
            result.push(Field {
                name: s.name.clone(),
                offset,
                size: s.size,
                kind,
                editable,
            });
        }
        Ok(result)
    }
    pub fn block_fields(&self, field: &Field, index: usize) -> Result<Vec<Field>> {
        let FieldKind::Block {
            target,
            count,
            element_size,
            children,
        } = &field.kind
        else {
            bail!("not a tag block")
        };
        ensure!(index < *count, "block element index out of range");
        self.fields_at(
            target.context("block has no target")?,
            index
                .checked_mul(*element_size)
                .context("element offset overflow")?,
            children,
        )
    }
    pub fn save_as(&self, path: &Path) -> Result<()> {
        // Parse the complete output before writing, then publish atomically.
        Self::parse(self.bytes.clone(), self.schema.clone())?;
        crate::file::write_bytes_atomic(path, &self.bytes, |temporary| {
            Self::open(temporary, self.schema.clone())
                .map(|_| ())
                .map_err(|error| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                })
        })?;
        Ok(())
    }
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
