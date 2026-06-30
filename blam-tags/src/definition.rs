//! Facade API — concept-oriented access over the schema types in
//! [`crate::layout`].
//!
//! Mirrors the data-side facade in [`crate::api`]: structural records
//! (`TagStructLayout`, `TagFieldLayout`, `TagBlockLayout`,
//! `TagArrayLayout`, `TagResourceLayout`, `TagInteropLayout`) are
//! wrapped by lightweight handles that carry a `&TagLayout` plus the
//! relevant table index. This lets callers walk the schema without
//! ever touching `layout.fields[i].name_offset` /
//! `layout.get_string(...)` / `layout.struct_layouts[j]` directly.
//!
//! Entry point: [`crate::file::TagFile::definitions`] returns a
//! [`TagDefinitions`] rooted at the tag's schema.

use crate::fields::TagFieldType;
use crate::file::TagFile;
use crate::layout::TagLayout;

impl TagFile {
    /// Schema facade — navigate the definitions tree without touching
    /// the underlying [`TagLayout`] tables directly.
    pub fn definitions(&self) -> TagDefinitions<'_> {
        TagDefinitions { layout: &self.tag_stream.layout }
    }
}

/// Root handle over a [`TagLayout`]. Produced by
/// [`TagFile::definitions`].
#[derive(Clone, Copy)]
pub struct TagDefinitions<'a> {
    layout: &'a TagLayout,
}

impl<'a> TagDefinitions<'a> {
    /// The root struct definition — the tag group's top-level struct
    /// (reached via `block_layouts[header.tag_group_block_index].struct_index`).
    pub fn root_struct(&self) -> TagStructDefinition<'a> {
        let root_block_index = self.layout.header.tag_group_block_index as usize;
        let struct_index = self.layout.block_layouts[root_block_index].struct_index as usize;
        TagStructDefinition { layout: self.layout, struct_index }
    }
}

//================================================================================
// TagStructDefinition
//================================================================================

/// A struct definition wrapper — bundles a [`TagLayout`] with a
/// specific [`TagStructLayout`] index.
#[derive(Clone, Copy)]
pub struct TagStructDefinition<'a> {
    layout: &'a TagLayout,
    struct_index: usize,
}

impl<'a> TagStructDefinition<'a> {
    pub(crate) fn new(layout: &'a TagLayout, struct_index: usize) -> Self {
        Self { layout, struct_index }
    }

    /// Struct display name (e.g. `"biped"`).
    pub fn name(&self) -> &'a str {
        let record = &self.layout.struct_layouts[self.struct_index];
        self.layout.get_string(record.name_offset).unwrap_or("")
    }

    /// Size in bytes of one instance of this struct.
    pub fn size(&self) -> usize {
        self.layout.struct_layouts[self.struct_index].size
    }

    /// Stable 16-byte identifier. Changes across layout versions when
    /// the struct's shape materially changes.
    pub fn guid(&self) -> [u8; 16] {
        self.layout.struct_layouts[self.struct_index].guid
    }

    /// Schema-version tag — nonzero only on V4 layouts.
    pub fn version(&self) -> u32 {
        self.layout.struct_layouts[self.struct_index].version
    }

    /// Walk this struct's field definitions in declaration order,
    /// stopping before the terminator. Does *not* skip padding — use
    /// [`TagFieldDefinition::is_padding`] to filter if needed.
    pub fn fields(&self) -> impl Iterator<Item = TagFieldDefinition<'a>> + 'a {
        let layout = self.layout;
        let record = &layout.struct_layouts[self.struct_index];
        let start = record.first_field_index as usize;
        (start..)
            .take_while(move |&i| layout.fields[i].field_type != TagFieldType::Terminator)
            .map(move |i| TagFieldDefinition { layout, field_index: i })
    }
}

//================================================================================
// TagFieldDefinition
//================================================================================

/// A field definition wrapper — one row in `layout.fields`, indexed
/// by its position.
#[derive(Clone, Copy)]
pub struct TagFieldDefinition<'a> {
    layout: &'a TagLayout,
    field_index: usize,
}

impl<'a> TagFieldDefinition<'a> {
    pub(crate) fn new(layout: &'a TagLayout, field_index: usize) -> Self {
        Self { layout, field_index }
    }

    /// Field display name (e.g. `"jump velocity"`).
    pub fn name(&self) -> &'a str {
        let record = &self.layout.fields[self.field_index];
        self.layout.get_string(record.name_offset).unwrap_or("")
    }

    /// Canonical type name (e.g. `"real point 3d"`).
    pub fn type_name(&self) -> &'a str {
        let record = &self.layout.fields[self.field_index];
        let type_name_offset = self.layout.field_types[record.type_index as usize].name_offset;
        self.layout.get_string(type_name_offset).unwrap_or("")
    }

    /// Resolved type discriminant.
    pub fn field_type(&self) -> TagFieldType {
        self.layout.fields[self.field_index].field_type
    }

    /// Byte offset of this field's raw data within its containing
    /// struct. Set after layout read; zero for padding.
    pub fn offset(&self) -> u32 {
        self.layout.fields[self.field_index].offset
    }

    /// `true` for pad / useless_pad / skip / explanation / unknown /
    /// terminator fields. These have no user-visible value.
    pub fn is_padding(&self) -> bool {
        matches!(
            self.field_type(),
            TagFieldType::Pad
                | TagFieldType::UselessPad
                | TagFieldType::Skip
                | TagFieldType::Explanation
                | TagFieldType::Unknown
                | TagFieldType::Terminator,
        )
    }

    /// If this is a `Struct` field, return the nested struct
    /// definition. `None` for any other field type.
    pub fn as_struct(&self) -> Option<TagStructDefinition<'a>> {
        let record = &self.layout.fields[self.field_index];
        if record.field_type != TagFieldType::Struct {
            return None;
        }
        Some(TagStructDefinition {
            layout: self.layout,
            struct_index: record.definition as usize,
        })
    }

    /// If this is a `Block` field, return the block definition.
    pub fn as_block(&self) -> Option<TagBlockDefinition<'a>> {
        let record = &self.layout.fields[self.field_index];
        if record.field_type != TagFieldType::Block {
            return None;
        }
        Some(TagBlockDefinition {
            layout: self.layout,
            block_layout_index: record.definition as usize,
        })
    }

    /// For a (non-custom) block-index field, the block definition it indexes
    /// into — its `definition` holds the target block's layout index. `None`
    /// for other field types and for `custom_*_block_index` fields (whose
    /// target search-name isn't carried in the definition data).
    pub fn block_index_target(&self) -> Option<TagBlockDefinition<'a>> {
        let record = &self.layout.fields[self.field_index];
        if !matches!(
            record.field_type,
            TagFieldType::CharBlockIndex
                | TagFieldType::ShortBlockIndex
                | TagFieldType::LongBlockIndex
        ) {
            return None;
        }
        Some(TagBlockDefinition {
            layout: self.layout,
            block_layout_index: record.definition as usize,
        })
    }

    /// If this is an `Array` field, return the array definition.
    pub fn as_array(&self) -> Option<TagArrayDefinition<'a>> {
        let record = &self.layout.fields[self.field_index];
        if record.field_type != TagFieldType::Array {
            return None;
        }
        Some(TagArrayDefinition {
            layout: self.layout,
            array_layout_index: record.definition as usize,
        })
    }

    /// If this is a `PageableResource` field, return the resource
    /// definition.
    pub fn as_resource(&self) -> Option<TagResourceDefinition<'a>> {
        let record = &self.layout.fields[self.field_index];
        if record.field_type != TagFieldType::PageableResource {
            return None;
        }
        Some(TagResourceDefinition {
            layout: self.layout,
            resource_layout_index: record.definition as usize,
        })
    }

    /// If this is an `ApiInterop` field, return the interop
    /// definition — gives the linked descriptor struct and stable
    /// guid for the interop's runtime type.
    pub fn as_api_interop(&self) -> Option<TagApiInteropDefinition<'a>> {
        let record = &self.layout.fields[self.field_index];
        if record.field_type != TagFieldType::ApiInterop {
            return None;
        }
        Some(TagApiInteropDefinition {
            layout: self.layout,
            interop_layout_index: record.definition as usize,
        })
    }
}

//================================================================================
// TagBlockDefinition / TagArrayDefinition / TagResourceDefinition
//================================================================================

/// A block definition — names a block whose elements are instances
/// of a struct.
#[derive(Clone, Copy)]
pub struct TagBlockDefinition<'a> {
    layout: &'a TagLayout,
    block_layout_index: usize,
}

impl<'a> TagBlockDefinition<'a> {
    pub(crate) fn new(layout: &'a TagLayout, block_layout_index: usize) -> Self {
        Self { layout, block_layout_index }
    }

    /// Block display name (e.g. `"materials"`, `"contact points"`).
    pub fn name(&self) -> &'a str {
        let record = &self.layout.block_layouts[self.block_layout_index];
        self.layout.get_string(record.name_offset).unwrap_or("")
    }

    /// Element-count cap declared by the schema. Not enforced at
    /// runtime.
    pub fn max_count(&self) -> u32 {
        self.layout.block_layouts[self.block_layout_index].max_count
    }

    /// The struct definition for one element of this block.
    pub fn struct_definition(&self) -> TagStructDefinition<'a> {
        let struct_index = self.layout.block_layouts[self.block_layout_index].struct_index as usize;
        TagStructDefinition { layout: self.layout, struct_index }
    }
}

/// An array definition — a fixed-count inline array of a struct.
#[derive(Clone, Copy)]
pub struct TagArrayDefinition<'a> {
    layout: &'a TagLayout,
    array_layout_index: usize,
}

impl<'a> TagArrayDefinition<'a> {
    pub(crate) fn new(layout: &'a TagLayout, array_layout_index: usize) -> Self {
        Self { layout, array_layout_index }
    }

    /// Array display name.
    pub fn name(&self) -> &'a str {
        let record = &self.layout.array_layouts[self.array_layout_index];
        self.layout.get_string(record.name_offset).unwrap_or("")
    }

    /// Schema-declared element count (fixed at layout-compile time).
    pub fn count(&self) -> u32 {
        self.layout.array_layouts[self.array_layout_index].count
    }

    /// The struct definition for one element of this array.
    pub fn struct_definition(&self) -> TagStructDefinition<'a> {
        let struct_index = self.layout.array_layouts[self.array_layout_index].struct_index as usize;
        TagStructDefinition { layout: self.layout, struct_index }
    }
}

/// An api-interop definition — declares the runtime type an
/// `api_interop` field points at. Sourced from the `]==[` chunk.
#[derive(Clone, Copy)]
pub struct TagApiInteropDefinition<'a> {
    layout: &'a TagLayout,
    interop_layout_index: usize,
}

impl<'a> TagApiInteropDefinition<'a> {
    /// Declared name of the interop type.
    pub fn name(&self) -> &'a str {
        let record = &self.layout.interop_layouts[self.interop_layout_index];
        self.layout.get_string(record.name_offset).unwrap_or("")
    }

    /// Stable 16-byte identifier for the interop type across layout
    /// versions. Same role as [`TagStructDefinition::guid`].
    pub fn guid(&self) -> [u8; 16] {
        self.layout.interop_layouts[self.interop_layout_index].guid
    }

    /// The descriptor struct — the runtime object's schema shape.
    /// Not a struct of on-disk data; only useful for introspection.
    pub fn descriptor(&self) -> TagStructDefinition<'a> {
        let struct_index =
            self.layout.interop_layouts[self.interop_layout_index].struct_index as usize;
        TagStructDefinition::new(self.layout, struct_index)
    }
}

/// A pageable-resource definition — declares a resource field's
/// shape.
#[derive(Clone, Copy)]
pub struct TagResourceDefinition<'a> {
    layout: &'a TagLayout,
    resource_layout_index: usize,
}

impl<'a> TagResourceDefinition<'a> {
    pub(crate) fn new(layout: &'a TagLayout, resource_layout_index: usize) -> Self {
        Self { layout, resource_layout_index }
    }

    /// Resource definition display name.
    pub fn name(&self) -> &'a str {
        let record = &self.layout.resource_layouts[self.resource_layout_index];
        self.layout.get_string(record.name_offset).unwrap_or("")
    }

    /// The struct definition wrapping the resource's exploded payload
    /// (non-null resources carry their own struct tree at this shape).
    pub fn struct_definition(&self) -> TagStructDefinition<'a> {
        let struct_index =
            self.layout.resource_layouts[self.resource_layout_index].struct_index as usize;
        TagStructDefinition { layout: self.layout, struct_index }
    }
}
