use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueKind {
    Signed(usize),
    Unsigned(usize),
    Float(usize),
    ShortVector(usize),
    String,
    Enum(usize),
    Flags(usize),
    StringId,
    Reference,
    Data,
    Opaque,
}
#[derive(Clone, Debug)]
pub struct SchemaField {
    pub name: String,
    pub type_name: String,
    pub offset: usize,
    pub size: usize,
    pub kind: ValueKind,
    pub children: Vec<SchemaField>,
    pub element_size: usize,
    pub is_block: bool,
    pub options: Vec<String>,
    pub data_definition: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Schema {
    pub fields: Vec<SchemaField>,
    pub size: usize,
}

#[derive(Clone, Debug)]
pub struct Definitions {
    pub root: PathBuf,
    paths: BTreeMap<String, PathBuf>,
    schemas: Arc<Mutex<BTreeMap<String, Arc<Schema>>>>,
}
impl Definitions {
    pub fn load(root: &Path) -> Result<Self> {
        let mut paths = BTreeMap::new();
        for entry in fs::read_dir(root)
            .with_context(|| format!("missing Halo 5 definitions at {}", root.display()))?
        {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if !name.ends_with(".xml") || !name.starts_with('(') {
                continue;
            }
            let Some(close) = name.find(')') else {
                continue;
            };
            paths.insert(name[close + 1..name.len() - 4].to_ascii_lowercase(), path);
        }
        ensure!(
            !paths.is_empty(),
            "no Halo 5 XML definitions at {}",
            root.display()
        );
        Ok(Self {
            root: root.to_owned(),
            paths,
            schemas: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }
    pub fn group_for_name(&self, name: &str) -> Option<String> {
        let file = self
            .paths
            .get(&name.to_ascii_lowercase())?
            .file_name()?
            .to_str()?;
        Some(file[1..file.find(')')?].to_owned())
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.paths.keys().map(String::as_str)
    }
    pub fn schema(&self, name: &str) -> Result<Arc<Schema>> {
        let name = name.to_ascii_lowercase();
        let mut schemas = self
            .schemas
            .lock()
            .map_err(|_| anyhow::anyhow!("layout cache lock poisoned"))?;
        if let Some(schema) = schemas.get(&name) {
            return Ok(schema.clone());
        }
        let path = self
            .paths
            .get(&name.to_ascii_lowercase())
            .with_context(|| format!("no Halo 5 layout for {name}"))?;
        let schema = Arc::new(
            Schema::parse(&fs::read_to_string(path)?)
                .with_context(|| format!("invalid layout {}", path.display()))?,
        );
        schemas.insert(name, schema.clone());
        Ok(schema)
    }
}
impl Schema {
    pub fn parse(xml: &str) -> Result<Self> {
        let doc = roxmltree::Document::parse(xml)?;
        ensure!(
            doc.root_element().tag_name().name() == "plugin",
            "expected plugin XML root"
        );
        let (fields, size) = fields(doc.root_element(), 0)?;
        Ok(Self { fields, size })
    }
}
fn fields(node: roxmltree::Node<'_, '_>, depth: usize) -> Result<(Vec<SchemaField>, usize)> {
    ensure!(depth < 64, "layout nesting exceeds 64 levels");
    let mut result = Vec::new();
    let mut offset = 0usize;
    for node in node.children().filter(|n| n.is_element()) {
        let t = node.tag_name().name();
        if t == "item" {
            continue;
        }
        let mut children = Vec::new();
        let mut element_size = 0;
        let is_block = t == "_field_block_64";
        let (size, kind) = if matches!(t, "_field_struct" | "_field_array" | "_field_block_64") {
            (children, element_size) = fields(node, depth + 1)?;
            (if is_block { 28 } else { element_size }, ValueKind::Opaque)
        } else {
            match t.trim_start_matches("_field_") {
                "custom" | "explanation" => (0, ValueKind::Opaque),
                "pad" | "skip" => (
                    node.attribute("length")
                        .context("padding has no length")?
                        .parse()?,
                    ValueKind::Opaque,
                ),
                "int64_integer" => (8, ValueKind::Signed(8)),
                "qword_integer" => (8, ValueKind::Unsigned(8)),
                "long_integer" | "long_block_index" | "custom_long_block_index" => {
                    (4, ValueKind::Signed(4))
                }
                "short_integer" | "short_block_index" | "custom_short_block_index" => {
                    (2, ValueKind::Signed(2))
                }
                "char_integer" | "char_block_index" => (1, ValueKind::Signed(1)),
                "dword_integer" => (4, ValueKind::Unsigned(4)),
                "word_integer" => (2, ValueKind::Unsigned(2)),
                "byte_integer" => (1, ValueKind::Unsigned(1)),
                "long_enum" => (4, ValueKind::Enum(4)),
                "short_enum" => (2, ValueKind::Enum(2)),
                "char_enum" => (1, ValueKind::Enum(1)),
                "long_flags" | "long_block_flags" => (4, ValueKind::Flags(4)),
                "word_flags" | "word_block_flags" => (2, ValueKind::Flags(2)),
                "byte_flags" | "byte_block_flags" => (1, ValueKind::Flags(1)),
                "real" | "real_fraction" | "angle" => (4, ValueKind::Float(1)),
                "real_bounds"
                | "fraction_bounds"
                | "angle_bounds"
                | "real_point_2d"
                | "real_vector_2d"
                | "real_euler_angles_2d" => (8, ValueKind::Float(2)),
                "real_point_3d"
                | "real_vector_3d"
                | "real_euler_angles_3d"
                | "real_plane_2d"
                | "real_rgb_color"
                | "real_hsv_color" => (12, ValueKind::Float(3)),
                "real_quaternion" | "real_plane_3d" | "real_argb_color" | "real_ahsv_color" => {
                    (16, ValueKind::Float(4))
                }
                "short_integer_bounds" | "point_2d" => (4, ValueKind::ShortVector(2)),
                "rectangle_2d" => (8, ValueKind::ShortVector(4)),
                "rgb_color" | "argb_color" | "tag" => (4, ValueKind::Opaque),
                "string_id" | "old_string_id" => (4, ValueKind::StringId),
                "string" => (32, ValueKind::String),
                "long_string" => (256, ValueKind::String),
                "tag_reference_64" => (32, ValueKind::Reference),
                "data_64" => (28, ValueKind::Data),
                "pageable_resource_64" => (16, ValueKind::Opaque),
                _ => bail!("unsupported field type {t}; layout refused to avoid misaligned edits"),
            }
        };
        let options = node
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "item")
            .map(|n| {
                n.attribute("Name")
                    .or_else(|| n.attribute("name"))
                    .unwrap_or("")
                    .to_owned()
            })
            .collect();
        result.push(SchemaField {
            name: node.attribute("name").unwrap_or(t).to_owned(),
            type_name: t.to_owned(),
            offset,
            size,
            kind,
            children,
            element_size,
            is_block,
            options,
            data_definition: (t == "_field_data_64").then(|| {
                if node
                    .parent()
                    .and_then(|parent| parent.attribute("srcguid"))
                    .is_some_and(|guid| {
                        guid.eq_ignore_ascii_case("5a14a98f-4484-53fa-d468-10809e45f10a")
                    })
                {
                    "function_definition_data".to_owned()
                } else {
                    "tag_data".to_owned()
                }
            }),
        });
        offset = offset.checked_add(size).context("layout size overflow")?;
    }
    Ok((result, offset))
}
