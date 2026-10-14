//! Adapt ucsh schema/data to the existing TagFile field model. The normalized
//! tree is for editing and journal snapshots; saves patch the original ucsh
//! bytes, never write the normalized container to a game tag file.
use super::{Document, Field, FieldKind, SchemaField, Source, ValueKind};
use crate as blam_tags;
use crate::data::{TagBlockData, TagStructData, TagSubChunkContent, TagSubChunkEntry};
use crate::{TagFile, TagFileHeader, TagLayout, TagStruct};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::HashMap;
#[cfg(test)]
use std::path::PathBuf;

pub struct LoadedTag {
    pub tag: TagFile,
    pub backing: TagBacking,
}

pub struct TagBacking {
    pub native: Document,
    bindings: HashMap<String, Field>,
}

impl TagBacking {
    pub fn save(&mut self, tag: &TagFile, path: &std::path::Path) -> Result<()> {
        let mut edits = Vec::new();
        collect_bytes(tag.root(), "", &self.bindings, &mut edits)?;
        let output = self.native.with_field_bytes(&edits)?;
        output.save_as(path)?;
        self.native = output;
        Ok(())
    }
}

fn collect_bytes(
    value: TagStruct<'_>,
    prefix: &str,
    bindings: &HashMap<String, Field>,
    edits: &mut Vec<(Field, Vec<u8>)>,
) -> Result<()> {
    for field in value.fields_all() {
        let path = prefix
            .parse::<crate::TagFieldPath>()
            .unwrap()
            .with_field(&field)
            .to_string();
        if let Some(native) = bindings.get(&path).filter(|field| field.editable) {
            let offset = field.definition().offset() as usize;
            ensure!(
                field.definition().wire_width() as usize == native.size,
                "field width changed"
            );
            let bytes = value
                .raw()
                .get(offset..offset + native.size)
                .context("field bytes missing")?;
            edits.push((native.clone(), bytes.to_vec()));
        }
        if let Some(nested) = field.as_struct() {
            collect_bytes(nested, &path, bindings, edits)?;
        } else if let Some(block) = field.as_block() {
            if let Some(native) = bindings.get(&path) {
                let FieldKind::Block { count, .. } = native.kind else {
                    unreachable!()
                };
                ensure!(
                    block.len() == count,
                    "block resizing is not supported by this storage"
                );
            }
            for (i, element) in block.iter().enumerate() {
                collect_bytes(element, &format!("{path}[{i}]"), bindings, edits)?;
            }
        }
    }
    Ok(())
}

pub fn read_tag(source: &Source, index: usize) -> Result<LoadedTag> {
    let native = source.read(index)?;
    let group = &source.entries[index];
    let mut schema = EditorSchema::default();
    let root = format!("{}_struct_definition", group.group_name);
    schema.add_struct(&root, &native.schema.fields)?;
    schema
        .blocks
        .insert("root".into(), json!({"max_count":1,"struct":root}));
    let json = json!({"tag":format!("{:<4}",group.group),"version":0,"flags":0,"block":"root",
        "structs":schema.structs,"blocks":schema.blocks,"enums_flags":schema.options,"datas":schema.datas});
    let mut layout = TagLayout::from_json_value(json)?;
    let explanation_name = intern(&mut layout, "explanation");
    layout
        .field_types
        .push(blam_tags::layout::TagFieldTypeLayout {
            name_offset: explanation_name,
            size: 0,
            needs_sub_chunk: 0,
        });
    layout.header.field_type_count = layout.field_types.len() as u32;
    let root_index =
        layout.block_layouts[layout.header.tag_group_block_index as usize].struct_index as usize;
    restore_names(&mut layout, root_index, &native.schema.fields);
    layout.header.string_data_size = layout.string_data.len() as u32;
    let mut bindings = HashMap::new();
    let fields = native.root_fields()?;
    let mut budget = EditorBudget {
        fields: 1_000_000,
        payload_bytes: 256 * 1024 * 1024,
    };
    let (raw, children) = encode_struct(
        &native,
        &layout,
        root_index,
        &fields,
        "",
        &mut bindings,
        &mut budget,
        0,
    )?;
    let root_block = layout.header.tag_group_block_index;
    let data = block_data(root_block, raw, vec![children]);
    let tag = TagFile::from_parts(
        TagFileHeader {
            pad: [0; 36],
            build_version: 0,
            build_number: 0,
            version: 0,
            group_tag: crate::parse_group_tag(&group.group).context("invalid group tag")?,
            group_version: 0,
            checksum: 0,
            signature: u32::from_be_bytes(*b"BLAM"),
        },
        crate::file::TagContainer::Mcc,
        crate::Endian::Le,
        crate::stream::TagStream { layout, data },
    );
    Ok(LoadedTag {
        tag,
        backing: TagBacking { native, bindings },
    })
}

impl TagBacking {
    /// Fields the native format can safely write without rebuilding metadata.
    pub fn writable_fields(&self) -> impl Iterator<Item = &str> {
        self.bindings
            .iter()
            .filter(|(_, field)| field.editable)
            .map(|(path, _)| path.as_str())
    }
}

#[derive(Default)]
struct EditorSchema {
    structs: serde_json::Map<String, Value>,
    blocks: serde_json::Map<String, Value>,
    options: serde_json::Map<String, Value>,
    datas: serde_json::Map<String, Value>,
    next: usize,
}
impl EditorSchema {
    fn add_struct(&mut self, name: &str, fields: &[SchemaField]) -> Result<usize> {
        let mut result = Vec::new();
        let mut size = 0;
        for field in fields {
            self.next += 1;
            let id = format!("definition_{}", self.next);
            let (ty, width) = editor_type(field);
            let definition = if ty == "block" || ty == "struct" {
                let child_size = self.add_struct(&id, &field.children)?;
                if ty == "block" {
                    self.blocks
                        .insert(id.clone(), json!({"max_count":u32::MAX,"struct":id}));
                }
                if ty == "struct" {
                    size += child_size;
                }
                json!(id)
            } else if matches!(field.kind, ValueKind::Enum(_) | ValueKind::Flags(_)) {
                self.options
                    .insert(id.clone(), json!({"options":field.options}));
                json!(id)
            } else if ty == "pad" {
                json!(field.size)
            } else if ty == "tag_reference" {
                json!({"flags":0,"allowed":[]})
            } else if ty == "data" {
                let name = field.data_definition.as_deref().unwrap_or("tag_data");
                self.datas.insert(name.into(), json!({}));
                json!(name)
            } else {
                Value::Null
            };
            result.push(json!({"type":ty,"name":field.name,"definition":definition}));
            size += width;
        }
        result.push(json!({"type":"terminator"}));
        let guid = format!("{:032X}", self.structs.len() + 1);
        self.structs.insert(
            name.to_owned(),
            json!({"guid":guid,"size":size,"fields":result}),
        );
        Ok(size)
    }
}

fn editor_type(field: &SchemaField) -> (String, usize) {
    if field.is_block {
        return ("block".into(), 12);
    }
    if !field.children.is_empty() {
        return ("struct".into(), 0);
    }
    let ty = match field.kind {
        ValueKind::Signed(n) => match n {
            1 => "char_integer",
            2 => "short_integer",
            4 => "long_integer",
            _ => "int64_integer",
        }
        .to_owned(),
        ValueKind::Unsigned(n) => match n {
            1 => "byte_integer",
            2 => "word_integer",
            4 => "dword_integer",
            _ => "qword_integer",
        }
        .to_owned(),
        ValueKind::Enum(n) => match n {
            1 => "char_enum",
            2 => "short_enum",
            _ => "long_enum",
        }
        .to_owned(),
        ValueKind::Flags(n) => match n {
            1 => "byte_flags",
            2 => "word_flags",
            _ => "long_flags",
        }
        .to_owned(),
        ValueKind::Reference => return ("tag_reference".into(), 16),
        ValueKind::Data => return ("data".into(), 20),
        ValueKind::StringId => return ("string_id".into(), 4),
        ValueKind::Opaque if field.size == 0 => return ("explanation".into(), 0),
        ValueKind::Opaque => "pad".into(),
        _ => match field.type_name.trim_start_matches("_field_") {
            "short_integer_bounds" => "short_bounds".into(),
            other => other.to_owned(),
        },
    };
    (ty, field.size)
}

fn intern(layout: &mut TagLayout, text: &str) -> u32 {
    let offset = layout.string_data.len() as u32;
    layout.string_data.extend(text.as_bytes());
    layout.string_data.push(0);
    offset
}
fn restore_names(layout: &mut TagLayout, index: usize, fields: &[SchemaField]) {
    let first = layout.struct_layouts[index].first_field_index as usize;
    for (i, field) in fields.iter().enumerate() {
        let at = first + i;
        layout.fields[at].name_offset = intern(layout, &field.name);
        if field.size == 0 && field.children.is_empty() {
            layout.fields[at].type_index = (layout.field_types.len() - 1) as u32;
            layout.fields[at].field_type = blam_tags::TagFieldType::Explanation;
            layout.fields[at].definition = intern(layout, &field.name);
        }
        if !field.children.is_empty() {
            let definition = layout.fields[at].definition as usize;
            let child = if field.is_block {
                layout.block_layouts[definition].struct_index as usize
            } else {
                definition
            };
            restore_names(layout, child, &field.children);
        }
    }
}

fn block_data(block_index: u32, raw_data: Vec<u8>, elements: Vec<TagStructData>) -> TagBlockData {
    TagBlockData {
        block_index,
        flags: 0,
        raw_data,
        endian: crate::Endian::Le,
        elements,
        classic_block_header: None,
        classic_structural_dirty: false,
        classic_trailing: None,
    }
}
struct EditorBudget {
    fields: usize,
    payload_bytes: usize,
}

fn encode_struct(
    doc: &Document,
    layout: &TagLayout,
    index: usize,
    fields: &[Field],
    prefix: &str,
    bindings: &mut HashMap<String, Field>,
    budget: &mut EditorBudget,
    depth: usize,
) -> Result<(Vec<u8>, TagStructData)> {
    ensure!(
        depth < 64 && fields.len() <= budget.fields,
        "tag tree exceeds editor limits"
    );
    budget.fields -= fields.len();
    let definition = &layout.struct_layouts[index];
    let mut raw = vec![0; definition.size];
    let mut children = Vec::new();
    for (i, native) in fields.iter().enumerate() {
        let f = &layout.fields[definition.first_field_index as usize + i];
        let name = blam_tags::clean_field_name(&native.name);
        let segment = crate::field_path::TagFieldPathSegment {
            name: name.into_owned(),
            ordinal: Some(i),
            ..Default::default()
        }
        .to_string();
        let path = if prefix.is_empty() {
            segment
        } else {
            format!("{prefix}/{segment}")
        };
        let offset = f.offset as usize;
        let width = layout.field_types[f.type_index as usize].size as usize;
        bindings.insert(path.clone(), native.clone());
        let content = match &native.kind {
            FieldKind::Inline(fields) => {
                let (data, tree) = encode_struct(
                    doc,
                    layout,
                    f.definition as usize,
                    fields,
                    &path,
                    bindings,
                    budget,
                    depth + 1,
                )?;
                raw[offset..offset + data.len()].copy_from_slice(&data);
                Some(TagSubChunkContent::Struct(tree))
            }
            FieldKind::Block { count, .. } => {
                let child_index = layout.block_layouts[f.definition as usize].struct_index as usize;
                let mut data = Vec::new();
                let mut tree = Vec::new();
                ensure!(*count <= budget.fields, "tag block exceeds editor limits");
                for n in 0..*count {
                    let fields = doc.block_fields(native, n)?;
                    let (element, sub) = encode_struct(
                        doc,
                        layout,
                        child_index,
                        &fields,
                        &format!("{path}[{n}]"),
                        bindings,
                        budget,
                        depth + 1,
                    )?;
                    data.extend(element);
                    tree.push(sub);
                }
                raw[offset..offset + 4].copy_from_slice(&(*count as u32).to_le_bytes());
                Some(TagSubChunkContent::Block(block_data(
                    f.definition,
                    data,
                    tree,
                )))
            }
            FieldKind::Value(ValueKind::Reference) => {
                let target = doc.tag_reference(native)?;
                let target = blam_tags::TagReferenceData {
                    group_tag_and_name: target.map(|reference| (reference.group, reference.path)),
                };
                Some(TagSubChunkContent::TagReference(
                    target.to_bytes(blam_tags::Endian::Le),
                ))
            }
            FieldKind::Value(ValueKind::StringId) => {
                raw[offset..offset + width]
                    .copy_from_slice(&doc.bytes()[native.offset..native.offset + native.size]);
                Some(TagSubChunkContent::StringId(
                    doc.string_id(native)?.into_bytes(),
                ))
            }
            FieldKind::Data { .. } => {
                let bytes = doc.data(native)?;
                ensure!(
                    bytes.len() <= budget.payload_bytes,
                    "data payloads exceed editor limits"
                );
                budget.payload_bytes -= bytes.len();
                Some(TagSubChunkContent::Data(bytes.to_vec()))
            }
            _ if width > 0 => {
                ensure!(
                    width == native.size,
                    "field width mismatch: {}",
                    native.name
                );
                raw[offset..offset + width]
                    .copy_from_slice(&doc.bytes()[native.offset..native.offset + native.size]);
                None
            }
            _ => None,
        };
        if let Some(content) = content {
            children.push(TagSubChunkEntry {
                field_index: Some((definition.first_field_index as usize + i) as u32),
                content,
            });
        }
    }
    Ok((
        raw,
        TagStructData {
            struct_index: index as u32,
            classic_struct_header: None,
            sub_chunks: children,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unique_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "blam-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn all_authored_schemas_feed_the_common_tag_model_and_save_ucsh() {
        let Some(definitions) = std::env::var_os("HALO5_DEFINITIONS") else {
            return;
        };
        let defs = super::super::Definitions::load(&PathBuf::from(definitions)).unwrap();
        let temp = Fixture(unique_temp_dir("halo5-schemas"));
        for name in defs.names() {
            let schema = defs.schema(name).unwrap();
            let mut bytes = vec![0; 128 + schema.size];
            bytes[..4].copy_from_slice(b"ucsh");
            for (offset, value) in [
                (4, 27u32),
                (32, 1),
                (36, 1),
                (60, 128),
                (64, schema.size as u32),
                (80, schema.size as u32),
                (120, u32::MAX),
            ] {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            bytes[86] = 1;
            let input = temp.0.join(format!("example.{name}"));
            std::fs::write(&input, &bytes).unwrap();
            let source = Source::open(&input, defs.clone()).unwrap();
            let mut loaded = read_tag(&source, 0).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(
                loaded.tag.group().tag,
                blam_tags::parse_group_tag(&source.entries[0].group).unwrap()
            );
            let output = temp.0.join("copy.tag");
            loaded
                .backing
                .save(&loaded.tag, &output)
                .unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(std::fs::read(output).unwrap(), bytes, "{name}");
        }
    }
    #[test]
    fn real_biped_normalization_preserves_all_field_bytes_and_nested_blocks() {
        let Some(root) = std::env::var_os("HALO5_TEST_DUMP") else {
            return;
        };
        let path =
            PathBuf::from(root).join("tag_dump/objects/characters/grunt_mecha/grunt_mecha.biped");
        let Some(definitions) = std::env::var_os("HALO5_DEFINITIONS") else {
            return;
        };
        let defs = super::super::Definitions::load(&PathBuf::from(definitions)).unwrap();
        let source = Source::open(&path, defs).unwrap();
        let mut loaded = read_tag(&source, 0).unwrap();
        let original = loaded.backing.native.bytes().to_vec();
        let mut functions = 0;
        for (path, native) in &loaded.backing.bindings {
            if !matches!(native.kind, FieldKind::Data { .. }) {
                continue;
            }
            let field = loaded.tag.root().field_path(path).unwrap();
            assert_eq!(
                field.as_data().unwrap(),
                loaded.backing.native.data(native).unwrap(),
                "{path}"
            );
            if field.is_function_data() && !field.as_data().unwrap().is_empty() {
                let function = field
                    .as_function()
                    .unwrap_or_else(|| panic!("function failed to parse: {path}"));
                assert!(function.evaluate(0.5, 0.5).is_finite(), "{path}");
                functions += 1;
            }
        }
        assert!(
            functions >= 14,
            "the real biped's functions are adapted to the shared decoder"
        );
        assert!(
            loaded
                .backing
                .bindings
                .keys()
                .any(|path| path.contains("[0]")),
            "populated nested blocks are represented"
        );
        let temp = Fixture(unique_temp_dir("halo5-real-model"));
        let output = temp.0.join("copy.biped");
        loaded.backing.save(&loaded.tag, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), original);
        // Exercise the actual serialized bytes the common undo journal stores.
        loaded.tag = TagFile::read_from_bytes(&loaded.tag.write_to_bytes().unwrap()).unwrap();
        loaded.backing.save(&loaded.tag, &output).unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), original);
        let path = loaded
            .backing
            .bindings
            .iter()
            .find(|(path, field)| {
                path.contains("[0]")
                    && field.editable
                    && matches!(field.kind, FieldKind::Value(ValueKind::Float(1)))
                    && loaded
                        .tag
                        .root()
                        .field_path(path)
                        .is_some_and(|field| field.type_name() == "real")
            })
            .map(|(path, _)| path.clone())
            .unwrap();
        let native = loaded.backing.bindings[&path].clone();
        loaded
            .tag
            .root_mut()
            .field_path_mut(&path)
            .unwrap()
            .set(blam_tags::TagFieldData::Real(2.5))
            .unwrap();
        loaded.backing.save(&loaded.tag, &output).unwrap();
        let after = std::fs::read(&output).unwrap();
        assert_eq!(&after[..native.offset], &original[..native.offset]);
        assert_eq!(
            &after[native.offset + native.size..],
            &original[native.offset + native.size..]
        );
        assert_eq!(
            &after[native.offset..native.offset + native.size],
            &2.5f32.to_le_bytes()
        );
        assert_eq!(std::fs::read(path_from_source(&source)).unwrap(), original);
    }
    fn path_from_source(source: &Source) -> &std::path::Path {
        &source.entries[0].path
    }
}
