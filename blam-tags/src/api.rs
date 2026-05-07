//! Façade API — concept-oriented access on top of the structural
//! types in [`crate::data`] / [`crate::layout`] / [`crate::path`].
//!
//! Entry points are reached from [`crate::TagFile`]:
//! [`TagFile::root`][crate::TagFile::root] /
//! [`root_mut`][crate::TagFile::root_mut] for the main payload,
//! plus the optional-stream accessors
//! ([`dependency_list`][crate::TagFile::dependency_list],
//! [`import_info`][crate::TagFile::import_info],
//! [`asset_depot_storage`][crate::TagFile::asset_depot_storage]).
//!
//! From there callers walk the tree via [`TagStruct`] / [`TagField`]
//! / [`TagBlock`] / [`TagArray`] (and their `*Mut` counterparts),
//! reach values via [`TagField::value`], and mutate via
//! [`TagFieldMut::set`] / [`TagBlockMut`]'s element editors. Every
//! borrow is split-borrow safe — there's no `RefCell` or other
//! interior mutability anywhere.
//!
//! ## Examples
//!
//! ```no_run
//! use blam_tags::{TagFieldData, TagFile};
//!
//! let mut tag = TagFile::read("masterchief.biped").unwrap();
//!
//! // Read a field by `/`-separated path.
//! let field = tag.root().field_path("jump velocity").unwrap();
//! if let Some(TagFieldData::Real(v)) = field.value() {
//!     println!("jump velocity = {v}");
//! }
//!
//! // Toggle a flag bit by name.
//! tag.root_mut()
//!     .field_path_mut("unit/flags").unwrap()
//!     .flag_mut("has_hull").unwrap()
//!     .toggle();
//!
//! // Delete element 3 of a block.
//! tag.root_mut()
//!     .field_path_mut("seats").unwrap()
//!     .as_block_mut().unwrap()
//!     .delete_element(3).unwrap();
//! ```

use crate::data::{TagBlockData, TagResourceChunk, TagStructData, TagSubChunkContent};
use crate::fields::{
    field_option_names, find_flag_bit, TagFieldData, TagFieldType,
};
use crate::file::TagFile;
use crate::layout::TagLayout;

//================================================================================
// Tag-level entry points
//================================================================================

impl TagFile {
    /// What kind of tag this is — group tag and group version.
    pub fn group(&self) -> TagGroup {
        TagGroup {
            tag: self.header.group_tag,
            version: self.header.group_version,
        }
    }

    /// The tag's root element — the first (and only) element of the
    /// `tag!` stream's root block.
    pub fn root(&self) -> TagStruct<'_> {
        stream_root(&self.tag_stream).expect("tag has no root element")
    }

    /// Mutable counterpart of [`TagFile::root`].
    pub fn root_mut(&mut self) -> TagStructMut<'_> {
        stream_root_mut(&mut self.tag_stream).expect("tag has no root element")
    }

    /// Root element of the `want` stream — the dependency list — if
    /// this tag has one. Most tags do.
    pub fn dependency_list(&self) -> Option<TagStruct<'_>> {
        stream_root(self.dependency_list_stream.as_ref()?)
    }

    /// Mutable counterpart of [`TagFile::dependency_list`].
    pub fn dependency_list_mut(&mut self) -> Option<TagStructMut<'_>> {
        stream_root_mut(self.dependency_list_stream.as_mut()?)
    }

    /// Root element of the `info` stream — import / source metadata —
    /// if this tag has one.
    pub fn import_info(&self) -> Option<TagStruct<'_>> {
        stream_root(self.import_info_stream.as_ref()?)
    }

    /// Mutable counterpart of [`TagFile::import_info`].
    pub fn import_info_mut(&mut self) -> Option<TagStructMut<'_>> {
        stream_root_mut(self.import_info_stream.as_mut()?)
    }

    /// Root element of the `assd` stream — asset depot storage —
    /// if this tag has one.
    pub fn asset_depot_storage(&self) -> Option<TagStruct<'_>> {
        stream_root(self.asset_depot_storage_stream.as_ref()?)
    }

    /// Mutable counterpart of [`TagFile::asset_depot_storage`].
    pub fn asset_depot_storage_mut(&mut self) -> Option<TagStructMut<'_>> {
        stream_root_mut(self.asset_depot_storage_stream.as_mut()?)
    }
}

fn stream_root(stream: &crate::stream::TagStream) -> Option<TagStruct<'_>> {
    let layout = &stream.layout;
    let block = &stream.data;
    let struct_data = block.elements.first()?;
    let struct_raw = block.element_raw(layout, 0);
    Some(TagStruct { layout, struct_data, struct_raw })
}

fn stream_root_mut(stream: &mut crate::stream::TagStream) -> Option<TagStructMut<'_>> {
    let layout = &stream.layout;
    let block = &mut stream.data;

    // Inline the element-size math so we can disjoint-split `block`
    // into its `elements` and `raw_data` fields below.
    let struct_index = layout.block_layouts[block.block_index as usize].struct_index as usize;
    let size = layout.struct_layouts[struct_index].size;

    let struct_data = block.elements.first_mut()?;
    let struct_raw = &mut block.raw_data[0..size];
    Some(TagStructMut { layout, struct_data, struct_raw })
}

/// What kind of tag this is: the 4-byte group tag (e.g. `b"scnr"`)
/// plus its group version. For the authoring-toolset build, format
/// version, and checksum, read [`crate::file::TagFileHeader`]
/// directly via `tag.header`.
///
/// Use [`crate::fields::format_group_tag`] to render `tag` as ASCII.
#[derive(Debug, Clone, Copy)]
pub struct TagGroup {
    /// BE-packed 4-byte group tag — same representation as on disk.
    pub tag: u32,
    pub version: u32,
}

//================================================================================
// Read-side: TagStruct / TagField and their typed views
//================================================================================

/// A struct instance — the unit that fields hang off of. The root
/// element, a block's element, an array's element, and a nested
/// struct field all map to this same type.
///
/// Cheap to copy (three references); pass by value freely.
#[derive(Clone, Copy)]
pub struct TagStruct<'a> {
    layout: &'a TagLayout,
    struct_data: &'a TagStructData,
    struct_raw: &'a [u8],
}

impl<'a> TagStruct<'a> {
    /// The schema side of this instance — the struct definition it
    /// conforms to. Bridges to the [`crate::definition`] facade.
    pub fn definition(&self) -> crate::TagStructDefinition<'a> {
        crate::TagStructDefinition::new(self.layout, self.struct_data.struct_index as usize)
    }

    /// The struct type's display name (e.g. `"biped"`).
    pub fn name(&self) -> &'a str {
        let definition = &self.layout.struct_layouts[self.struct_data.struct_index as usize];
        self.layout.get_string(definition.name_offset).unwrap_or("")
    }

    /// Size in bytes of one instance of this struct.
    pub fn size(&self) -> usize {
        self.layout.struct_layouts[self.struct_data.struct_index as usize].size
    }

    /// Walk the struct's fields in declaration order. Skips padding,
    /// explanations, terminators, and unknown types.
    pub fn fields(&self) -> impl Iterator<Item = TagField<'a>> + 'a {
        let TagStruct { layout, struct_data, struct_raw } = *self;
        let definition = &layout.struct_layouts[struct_data.struct_index as usize];
        let start = definition.first_field_index as usize;
        (start..)
            .take_while(move |&i| layout.fields[i].field_type != TagFieldType::Terminator)
            .filter(move |&i| !matches!(
                layout.fields[i].field_type,
                TagFieldType::Pad | TagFieldType::UselessPad | TagFieldType::Skip
                    | TagFieldType::Explanation | TagFieldType::Unknown,
            ))
            .map(move |i| TagField { layout, struct_data, struct_raw, field_index: i })
    }

    /// Walk every field, including padding / skip / explanation /
    /// unknown fields. Intended for layout investigation tooling
    /// (e.g. `inspect --all`). Normal consumers should use
    /// [`TagStruct::fields`] which filters these out.
    pub fn fields_all(&self) -> impl Iterator<Item = TagField<'a>> + 'a {
        let TagStruct { layout, struct_data, struct_raw } = *self;
        let definition = &layout.struct_layouts[struct_data.struct_index as usize];
        let start = definition.first_field_index as usize;
        (start..)
            .take_while(move |&i| layout.fields[i].field_type != TagFieldType::Terminator)
            .map(move |i| TagField { layout, struct_data, struct_raw, field_index: i })
    }

    /// User-addressable field names in declaration order. Mirrors
    /// [`TagStructData::field_names`] — used by the CLI's "did you
    /// mean?" path.
    pub fn field_names(&self) -> impl Iterator<Item = &'a str> + 'a {
        self.struct_data.field_names(self.layout)
    }

    /// Resolve a single field by name (case-sensitive, no path
    /// descent). Use [`TagStruct::field_path`] for paths like
    /// `"unit/seats[0]/flags"`.
    pub fn field(&self, name: &str) -> Option<TagField<'a>> {
        let field_index = self.struct_data.find_field_by_name(self.layout, name)?;
        Some(TagField {
            layout: self.layout,
            struct_data: self.struct_data,
            struct_raw: self.struct_raw,
            field_index,
        })
    }

    /// Walk `path` treating every `/`-separated segment as an
    /// intermediate descent into a struct / block element / array
    /// element — like [`TagStruct::field_path`] but with no
    /// "terminal field lookup" step. Returns the struct at the end
    /// of the walk.
    ///
    /// Use this when you want to reach a struct itself (e.g. to walk
    /// everything underneath it), not a specific field. Paths like
    /// `"unit/seats[2]/variants[0]"` land inside the 0th variant of
    /// the 2nd seat.
    pub fn descend(&self, path: &str) -> Option<TagStruct<'a>> {
        let (struct_data, struct_raw) = crate::path::descend_from_struct(
            self.layout, self.struct_data, self.struct_raw, path,
        )?;
        Some(TagStruct { layout: self.layout, struct_data, struct_raw })
    }

    /// Resolve a `/`-separated field path. Accepts optional
    /// `Type:name` filter and `[N]` block/array index per segment —
    /// same grammar as [`crate::path::lookup`].
    pub fn field_path(&self, path: &str) -> Option<TagField<'a>> {
        let cursor = crate::path::lookup_from_struct(
            self.layout, self.struct_data, self.struct_raw, path,
        )?;
        Some(TagField {
            layout: self.layout,
            struct_data: cursor.struct_data,
            struct_raw: cursor.struct_raw,
            field_index: cursor.field_index,
        })
    }

    // ---- typed field readers ----
    //
    // Convenience accessors for the common "look up a field by name,
    // pattern-match its value to a typed shape" pattern. Walkers in
    // jms / ass / animation / bitmap reach for these constantly.

    /// Read any integer-shaped field as `i64`. Accepts the 16
    /// integer-like `TagFieldData` variants (regular ints, block
    /// indices, custom block indices, enums, flags). Returns `None`
    /// if the field is missing or not integer-shaped.
    pub fn read_int_any(&self, name: &str) -> Option<i64> {
        match self.field(name)?.value()? {
            TagFieldData::CharInteger(v) => Some(v as i64),
            TagFieldData::ShortInteger(v) => Some(v as i64),
            TagFieldData::LongInteger(v) => Some(v as i64),
            TagFieldData::Int64Integer(v) => Some(v),
            TagFieldData::CharBlockIndex(v) => Some(v as i64),
            TagFieldData::ShortBlockIndex(v) => Some(v as i64),
            TagFieldData::LongBlockIndex(v) => Some(v as i64),
            TagFieldData::CustomCharBlockIndex(v) => Some(v as i64),
            TagFieldData::CustomShortBlockIndex(v) => Some(v as i64),
            TagFieldData::CustomLongBlockIndex(v) => Some(v as i64),
            TagFieldData::CharEnum { value, .. } => Some(value as i64),
            TagFieldData::ShortEnum { value, .. } => Some(value as i64),
            TagFieldData::LongEnum { value, .. } => Some(value as i64),
            TagFieldData::ByteFlags { value, .. } => Some(value as i64),
            TagFieldData::WordFlags { value, .. } => Some(value as i64),
            TagFieldData::LongFlags { value, .. } => Some(value as i64),
            TagFieldData::ByteBlockFlags(v) => Some(v as i64),
            TagFieldData::WordBlockFlags(v) => Some(v as i64),
            TagFieldData::LongBlockFlags(v) => Some(v as i64),
            _ => None,
        }
    }

    /// Read a real-shaped field as `f32`. Accepts `Real`,
    /// `RealFraction`, `RealSlider`, and `Angle` — every shape that
    /// stores a single 32-bit float.
    pub fn read_real(&self, name: &str) -> Option<f32> {
        match self.field(name)?.value()? {
            TagFieldData::Real(r) => Some(r),
            TagFieldData::RealFraction(r) => Some(r),
            TagFieldData::RealSlider(r) => Some(r),
            TagFieldData::Angle(r) => Some(r),
            _ => None,
        }
    }

    /// Read a `string_id` (or legacy `old_string_id`) field's resolved
    /// string. Returns `None` for missing fields, non-string-id values,
    /// or empty strings.
    pub fn read_string_id(&self, name: &str) -> Option<String> {
        match self.field(name)?.value()? {
            TagFieldData::StringId(sid) | TagFieldData::OldStringId(sid) =>
                Some(sid.string).filter(|s| !s.is_empty()),
            _ => None,
        }
    }

    /// Read an enum field's resolved variant name regardless of width
    /// (`char_enum` / `short_enum` / `long_enum` all map the same way).
    pub fn read_enum_name(&self, name: &str) -> Option<String> {
        match self.field(name)?.value()? {
            TagFieldData::CharEnum { name, .. } => name,
            TagFieldData::ShortEnum { name, .. } => name,
            TagFieldData::LongEnum { name, .. } => name,
            _ => None,
        }
    }

    /// Read a `tag_reference` field's relative path. Returns `None`
    /// for missing fields, non-reference values, or null/empty refs.
    pub fn read_tag_ref_path(&self, name: &str) -> Option<String> {
        match self.field(name)?.value()? {
            TagFieldData::TagReference(r) => r.group_tag_and_name.map(|(_, p)| p),
            _ => None,
        }
    }

    /// Read a `tag_reference` field's `(group_tag, relative_path)` pair.
    /// Returns `None` for missing fields, non-reference values, or
    /// null/empty refs. Used when the caller needs the FOURCC group
    /// tag to determine the on-disk file extension (e.g. `rmsh` →
    /// `.shader`, `rmtr` → `.shader_terrain`).
    pub fn read_tag_ref_with_group(&self, name: &str) -> Option<(u32, String)> {
        match self.field(name)?.value()? {
            TagFieldData::TagReference(r) => r.group_tag_and_name,
            _ => None,
        }
    }

    /// Read a `real_quaternion` field as `[i, j, k, w]`. Returns the
    /// Read a `real_quaternion` field. Returns
    /// [`RealQuaternion::IDENTITY`] when the field is *missing* (not
    /// present in this struct). **Panics** if the field is present
    /// but has a non-quaternion type — that's a code-vs-schema
    /// mismatch, not a runtime data issue, and silent defaults would
    /// hide it.
    pub fn read_quat(&self, name: &str) -> crate::math::RealQuaternion {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealQuaternion(q)) => q,
            None => crate::math::RealQuaternion::IDENTITY,
            Some(other) => type_mismatch_panic(name, "RealQuaternion", &other),
        }
    }

    /// Read a `real_point_3d` field. Returns [`RealPoint3d::ZERO`]
    /// when the field is missing. **Panics** if the field is present
    /// but has a different math type — for `real_vector_3d` use
    /// [`Self::read_vec3`].
    pub fn read_point3d(&self, name: &str) -> crate::math::RealPoint3d {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealPoint3d(p)) => p,
            None => crate::math::RealPoint3d::ZERO,
            Some(other) => type_mismatch_panic(name, "RealPoint3d", &other),
        }
    }

    /// Read a `real_vector_3d` field. Returns [`RealVector3d::ZERO`]
    /// when the field is missing. **Panics** on type mismatch — see
    /// [`Self::read_point3d`].
    pub fn read_vec3(&self, name: &str) -> crate::math::RealVector3d {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealVector3d(v)) => v,
            None => crate::math::RealVector3d::ZERO,
            Some(other) => type_mismatch_panic(name, "RealVector3d", &other),
        }
    }

    /// Read a `real_point_2d` field. Returns [`RealPoint2d::ZERO`]
    /// when the field is missing. **Panics** on type mismatch.
    pub fn read_point2d(&self, name: &str) -> crate::math::RealPoint2d {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealPoint2d(p)) => p,
            None => crate::math::RealPoint2d::ZERO,
            Some(other) => type_mismatch_panic(name, "RealPoint2d", &other),
        }
    }

    /// Read a `real_plane_3d` field. Returns the default (zero
    /// normal, zero offset) when missing. **Panics** on type
    /// mismatch.
    pub fn read_plane3d(&self, name: &str) -> crate::math::RealPlane3d {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealPlane3d(p)) => p,
            None => crate::math::RealPlane3d::default(),
            Some(other) => type_mismatch_panic(name, "RealPlane3d", &other),
        }
    }

    /// Read a `real_rgb_color` field. Returns the default (all
    /// zeros) when missing. **Panics** on type mismatch.
    pub fn read_rgb(&self, name: &str) -> crate::math::RealRgbColor {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealRgbColor(c)) => c,
            None => crate::math::RealRgbColor::default(),
            Some(other) => type_mismatch_panic(name, "RealRgbColor", &other),
        }
    }

    /// Read a `real_bounds` field. Returns the default (`lower = 0`,
    /// `upper = 0`) when missing. **Panics** on type mismatch.
    pub fn read_real_bounds(&self, name: &str) -> crate::math::RealBounds {
        match self.field(name).and_then(|f| f.value()) {
            Some(TagFieldData::RealBounds(b)) => b,
            None => crate::math::RealBounds::default(),
            Some(other) => type_mismatch_panic(name, "RealBounds", &other),
        }
    }

    /// Read a block-index field as `i16` with `-1` (none) default.
    /// Convenience for walkers that treat all block-index widths as
    /// 16-bit "index or sentinel."
    pub fn read_block_index(&self, name: &str) -> i16 {
        self.read_int_any(name).map(|v| v as i16).unwrap_or(-1)
    }
}

/// Surface a code-vs-schema type mismatch as a loud panic. Called
/// from the typed-math readers (`read_quat` / `read_point3d` /
/// `read_vec3` / etc.) when the named field exists but has a
/// different math type than the reader expects. Silent defaults
/// would let bugs like "calling `read_point3d` on a
/// `real_vector_3d` field" sit undetected.
#[cold]
#[track_caller]
fn type_mismatch_panic(field: &str, expected: &str, actual: &TagFieldData) -> ! {
    panic!(
        "field `{field}` is a `{actual:?}`, but reader expected `{expected}` \
         — schema and code disagree (use the matching reader for the actual type)"
    );
}

/// A resolved field within a [`TagStruct`]. Carries the field's
/// schema, current value (for scalar fields), and — for container
/// fields — a way to step into the sub-tree without ever touching
/// `sub_chunks` directly.
///
/// Cheap to copy; pass by value freely.
#[derive(Clone, Copy)]
pub struct TagField<'a> {
    layout: &'a TagLayout,
    struct_data: &'a TagStructData,
    struct_raw: &'a [u8],
    field_index: usize,
}

impl<'a> TagField<'a> {
    /// The schema side of this field — its definition in the layout.
    /// Bridges to the [`crate::definition`] facade.
    pub fn definition(&self) -> crate::TagFieldDefinition<'a> {
        crate::TagFieldDefinition::new(self.layout, self.field_index)
    }

    /// Field display name (e.g. `"jump velocity"`).
    pub fn name(&self) -> &'a str {
        let field = &self.layout.fields[self.field_index];
        self.layout.get_string(field.name_offset).unwrap_or("")
    }

    /// The schema type's display name (e.g. `"short_integer"`).
    pub fn type_name(&self) -> &'a str {
        let field = &self.layout.fields[self.field_index];
        let type_name_offset = self.layout.field_types[field.type_index as usize].name_offset;
        self.layout.get_string(type_name_offset).unwrap_or("")
    }

    /// The field's schema type — callers dispatch on this when they
    /// need to know exactly what kind of field this is.
    pub fn field_type(&self) -> TagFieldType {
        self.layout.fields[self.field_index].field_type
    }

    /// The field's current value. `None` for container and padding
    /// fields — use [`TagField::as_struct`] / [`TagField::as_block`]
    /// / [`TagField::as_array`] / [`TagField::as_resource`] to step
    /// into containers.
    pub fn value(&self) -> Option<TagFieldData> {
        self.struct_data.parse_field(self.layout, self.struct_raw, self.field_index)
    }

    /// Typed step-in accessors for container fields. Each returns
    /// `None` either when this field isn't that specific container
    /// shape OR when the schema says it is but the sub-chunk is
    /// missing on this tag instance. Real-world tags ship with
    /// null-sized `tgst` chunks whose array / block / struct fields
    /// have no corresponding entries; callers walking many tags
    /// shouldn't crash on that.
    pub fn as_struct(&self) -> Option<TagStruct<'a>> {
        if self.layout.fields[self.field_index].field_type != TagFieldType::Struct {
            return None;
        }
        let (struct_data, struct_raw) = self
            .struct_data
            .nested_struct(self.layout, self.struct_raw, self.field_index)?;
        Some(TagStruct { layout: self.layout, struct_data, struct_raw })
    }

    /// Step into a block field. `None` if this isn't a block or if
    /// the on-disk sub-chunk for it is missing.
    pub fn as_block(&self) -> Option<TagBlock<'a>> {
        if self.layout.fields[self.field_index].field_type != TagFieldType::Block {
            return None;
        }
        let block_data = self.sub_chunk().and_then(|c| match c {
            TagSubChunkContent::Block(b) => Some(b),
            _ => None,
        })?;
        Some(TagBlock { layout: self.layout, block_data })
    }

    /// Step into a fixed-count array field. `None` if this isn't an
    /// array or if the sub-chunk is missing.
    pub fn as_array(&self) -> Option<TagArray<'a>> {
        let field = &self.layout.fields[self.field_index];
        if field.field_type != TagFieldType::Array {
            return None;
        }
        let elements = self.sub_chunk().and_then(|c| match c {
            TagSubChunkContent::Array(elements) => Some(elements.as_slice()),
            _ => None,
        })?;
        let array_layout_index = field.definition;
        let array_def = &self.layout.array_layouts[array_layout_index as usize];
        let element_size = self.layout.struct_layouts[array_def.struct_index as usize].size;
        let start = field.offset as usize;
        let array_raw = &self.struct_raw[start..start + elements.len() * element_size];
        Some(TagArray {
            layout: self.layout,
            array_layout_index,
            array_raw,
            elements,
        })
    }

    /// Borrowed slice of the bytes carried by a `data` field —
    /// avoids the clone that [`TagField::value`] performs for the
    /// `TagFieldData::Data` variant. Returns `None` for non-data
    /// fields.
    pub fn as_data(&self) -> Option<&'a [u8]> {
        if self.layout.fields[self.field_index].field_type != TagFieldType::Data {
            return None;
        }
        match self.sub_chunk()? {
            TagSubChunkContent::Data(bytes) => Some(bytes.as_slice()),
            _ => None,
        }
    }

    /// Decode the field as a [`crate::TagFunction`] (`mapping_function`
    /// data blob). The schema declares these as `data` fields with a
    /// 32-byte header + variable per-type compact data; this helper
    /// reads the bytes via [`Self::as_data`] and parses them. Returns
    /// `None` if the field isn't a `data` field, the bytes don't
    /// belong to a function blob, or parsing fails.
    pub fn as_function(&self) -> Option<crate::TagFunction> {
        let bytes = self.as_data()?;
        crate::TagFunction::parse(bytes).ok()
    }

    /// Step into a `pageable_resource` field. `None` if this isn't a
    /// resource field or if the sub-chunk is missing.
    pub fn as_resource(&self) -> Option<TagResource<'a>> {
        if self.layout.fields[self.field_index].field_type != TagFieldType::PageableResource {
            return None;
        }
        let chunk = self
            .sub_chunk()
            .and_then(|c| match c {
                TagSubChunkContent::Resource(r) => Some(r),
                _ => None,
            })?;
        Some(TagResource {
            layout: self.layout,
            chunk,
            parent_raw: self.struct_raw,
            field_index: self.field_index,
        })
    }

    /// For enum or flags fields: variant / bit names plus current
    /// state. Returns `None` for other field types.
    pub fn options(&self) -> Option<TagOptions<'a>> {
        let field = &self.layout.fields[self.field_index];
        let is_enum = matches!(
            field.field_type,
            TagFieldType::CharEnum | TagFieldType::ShortEnum | TagFieldType::LongEnum,
        );
        let is_flags = matches!(
            field.field_type,
            TagFieldType::ByteFlags | TagFieldType::WordFlags | TagFieldType::LongFlags,
        );
        if !is_enum && !is_flags {
            return None;
        }

        let names: Vec<&'a str> = field_option_names(self.layout, field).collect();
        let value = self.value();

        if is_enum {
            let current = value.and_then(|v| match v {
                TagFieldData::CharEnum { value, .. } => Some(value as i64),
                TagFieldData::ShortEnum { value, .. } => Some(value as i64),
                TagFieldData::LongEnum { value, .. } => Some(value as i64),
                _ => None,
            });
            Some(TagOptions::Enum { names, current })
        } else {
            let items = names
                .iter()
                .enumerate()
                .map(|(bit, &name)| {
                    let is_set = value.as_ref().and_then(|v| v.flag_bit(bit as u32)).unwrap_or(false);
                    TagFlagOption { name, bit: bit as u32, is_set }
                })
                .collect();
            Some(TagOptions::Flags(items))
        }
    }

    /// Look up a single flag by name on a flags-typed field.
    pub fn flag(&self, name: &str) -> Option<TagFlag<'a>> {
        let field = &self.layout.fields[self.field_index];
        let bit = find_flag_bit(self.layout, field, name)?;
        Some(TagFlag { field: *self, bit })
    }

    /// The sub-chunk content entry (if any) owned by this field —
    /// shared plumbing for [`TagField::as_block`] / `as_array` /
    /// `as_resource` and the string-id / tag-reference / data leaf
    /// variants that also live under this field's sub-chunk.
    fn sub_chunk(&self) -> Option<&'a TagSubChunkContent> {
        self.struct_data
            .sub_chunks
            .iter()
            .find(|e| e.field_index == Some(self.field_index as u32))
            .map(|e| &e.content)
    }
}

/// A variable-count block of same-typed elements. Byte-ownership
/// boundary — a block carries its own `raw_data`.
#[derive(Clone, Copy)]
pub struct TagBlock<'a> {
    layout: &'a TagLayout,
    block_data: &'a TagBlockData,
}

impl<'a> TagBlock<'a> {
    /// The schema side of this block — its block definition.
    /// Bridges to the [`crate::definition`] facade.
    pub fn definition(&self) -> crate::TagBlockDefinition<'a> {
        crate::TagBlockDefinition::new(self.layout, self.block_data.block_index as usize)
    }

    /// Number of elements currently in this block.
    pub fn len(&self) -> usize { self.block_data.elements.len() }

    /// `true` when this block has zero elements.
    pub fn is_empty(&self) -> bool { self.block_data.elements.is_empty() }

    /// Get the element at `index`. `None` if out of range.
    pub fn element(&self, index: usize) -> Option<TagStruct<'a>> {
        let struct_data = self.block_data.elements.get(index)?;
        let size = block_element_size(self.layout, self.block_data);
        let start = index * size;
        let struct_raw = &self.block_data.raw_data[start..start + size];
        Some(TagStruct { layout: self.layout, struct_data, struct_raw })
    }

    /// Iterate every element in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = TagStruct<'a>> + 'a {
        let TagBlock { layout, block_data } = *self;
        block_data.iter_elements(layout).map(move |(struct_raw, struct_data)| {
            TagStruct { layout, struct_data, struct_raw }
        })
    }
}

fn block_element_size(layout: &TagLayout, block_data: &TagBlockData) -> usize {
    let struct_index = layout.block_layouts[block_data.block_index as usize].struct_index as usize;
    layout.struct_layouts[struct_index].size
}

/// A fixed-count inline array. Count is schema-declared; elements'
/// bytes live contiguously in `array_raw` (a slice of the enclosing
/// struct's raw region starting at the array field's offset).
#[derive(Clone, Copy)]
pub struct TagArray<'a> {
    layout: &'a TagLayout,
    array_layout_index: u32,
    array_raw: &'a [u8],
    elements: &'a [TagStructData],
}

impl<'a> TagArray<'a> {
    /// The schema side of this array — its array definition.
    /// Bridges to the [`crate::definition`] facade.
    pub fn definition(&self) -> crate::TagArrayDefinition<'a> {
        crate::TagArrayDefinition::new(self.layout, self.array_layout_index as usize)
    }

    /// Schema-declared element count.
    pub fn len(&self) -> usize { self.elements.len() }

    /// `true` when the schema declares zero elements (rare but
    /// permitted).
    pub fn is_empty(&self) -> bool { self.elements.is_empty() }

    /// Get the element at `index`. `None` if out of range.
    pub fn element(&self, index: usize) -> Option<TagStruct<'a>> {
        let struct_data = self.elements.get(index)?;
        let size = self.layout.struct_layouts[self.element_struct_index() as usize].size;
        let start = index * size;
        let struct_raw = &self.array_raw[start..start + size];
        Some(TagStruct { layout: self.layout, struct_data, struct_raw })
    }

    /// Iterate every element in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = TagStruct<'a>> + 'a {
        let TagArray { layout, array_layout_index, array_raw, elements } = *self;
        let size = element_struct_size(layout, array_layout_index);
        elements.iter().enumerate().map(move |(i, struct_data)| {
            let start = i * size;
            TagStruct {
                layout,
                struct_data,
                struct_raw: &array_raw[start..start + size],
            }
        })
    }

    fn element_struct_index(&self) -> u32 {
        self.layout.array_layouts[self.array_layout_index as usize].struct_index
    }
}

fn element_struct_size(layout: &TagLayout, array_layout_index: u32) -> usize {
    let element_struct_index =
        layout.array_layouts[array_layout_index as usize].struct_index as usize;
    layout.struct_layouts[element_struct_index].size
}

/// Read-only view onto a pageable resource field.
///
/// Three byte regions coexist:
///
/// - The **inline 8 bytes** in the enclosing struct's raw region at
///   this field's offset. The `tag_resource` field type always
///   occupies 8 inline bytes — typically a runtime handle stub the
///   engine fills in at load time; on disk it's frequently zeros or
///   leftover memory state. Exposed via [`TagResource::inline_bytes`].
/// - The **resource header struct** — schema described by the
///   resource's layout (`TagResourceDefinition::struct_definition`).
///   For Exploded resources, the struct's raw bytes live in the
///   `tgdt` chunk payload (whose size matches the struct's declared
///   size), and its sub-chunk tree (nested blocks/data/etc.) lives
///   in the trailing `tgst`. Walkable via [`TagResource::as_struct`].
/// - The **post-struct payload** — for Exploded resources whose
///   tgdt is larger than the header struct's size, the trailing bytes
///   are opaque per-group data (e.g. animation codec stream, vertex
///   buffer). Reachable via [`TagResource::exploded_payload`] sliced
///   beyond `struct_size`.
#[derive(Clone, Copy)]
pub struct TagResource<'a> {
    layout: &'a TagLayout,
    chunk: &'a TagResourceChunk,
    /// The enclosing element's raw region — used to surface the 8
    /// inline bytes at this field's offset.
    parent_raw: &'a [u8],
    field_index: usize,
}

impl<'a> TagResource<'a> {
    /// Which on-disk shape this resource carries (Null, Exploded, or
    /// XSync). Distinguishes whether [`Self::as_struct`] /
    /// [`Self::exploded_payload`] / [`Self::xsync_payload`] return data.
    pub fn kind(&self) -> TagResourceKind {
        match self.chunk {
            TagResourceChunk::Null => TagResourceKind::Null,
            TagResourceChunk::Exploded { .. } => TagResourceKind::Exploded,
            TagResourceChunk::Xsync(_) => TagResourceKind::Xsync,
        }
    }

    /// The schema side of this resource — the resource definition
    /// declared in the layout.
    pub fn definition(&self) -> crate::TagResourceDefinition<'a> {
        let resource_layout_index =
            self.layout.fields[self.field_index].definition as usize;
        crate::TagResourceDefinition::new(self.layout, resource_layout_index)
    }

    /// The 8 inline bytes for this `tag_resource` field as they
    /// appear in the enclosing struct's raw region. These are
    /// preserved verbatim through roundtrip; their interpretation is
    /// engine-internal (typically a runtime handle).
    pub fn inline_bytes(&self) -> &'a [u8] {
        let offset = self.layout.fields[self.field_index].offset as usize;
        // tag_resource is always 8 inline bytes per the schema.
        &self.parent_raw[offset..offset + 8]
    }

    /// The resource header as a walkable struct. Returns `Some` only
    /// for Exploded resources, where the header struct's raw bytes
    /// live in the `tgdt` payload and its sub-chunk tree was parsed.
    /// Null and Xsync have no parsed struct to descend into.
    pub fn as_struct(&self) -> Option<TagStruct<'a>> {
        let TagResourceChunk::Exploded { struct_data, exploded } = self.chunk else {
            return None;
        };
        let struct_size =
            self.layout.struct_layouts[struct_data.struct_index as usize].size;
        // The struct's raw bytes are the leading `struct_size` bytes
        // of the tgdt payload. Anything past that is opaque
        // per-group data — see [`exploded_payload`].
        let struct_raw = exploded.get(..struct_size)?;
        Some(TagStruct {
            layout: self.layout,
            struct_data,
            struct_raw,
        })
    }

    /// The raw `tgdt` payload bytes for an Exploded resource. The
    /// leading `struct_size` bytes are the header struct's raw bytes
    /// (also reachable via [`as_struct`]); any trailing bytes are
    /// opaque per-group data.
    pub fn exploded_payload(&self) -> Option<&'a [u8]> {
        match self.chunk {
            TagResourceChunk::Exploded { exploded, .. } => Some(exploded.as_slice()),
            _ => None,
        }
    }

    /// The opaque XSync payload bytes. Not seen in the Halo 3 / Reach
    /// MCC corpus; present so future tags don't panic.
    pub fn xsync_payload(&self) -> Option<&'a [u8]> {
        match self.chunk {
            TagResourceChunk::Xsync(bytes) => Some(bytes.as_slice()),
            _ => None,
        }
    }
}

/// Wire-format shape of a `pageable_resource` field.
#[derive(Debug, Clone, Copy)]
pub enum TagResourceKind {
    /// `tg\0c` — empty / sentinel resource, no payload to walk.
    Null,
    /// `tgrc` — exploded resource. Carries a `tgdt` payload (header
    /// struct bytes + opaque per-group bytes) plus a nested `tgst`.
    Exploded,
    /// `tgxc` — XSync resource. Opaque payload.
    Xsync,
}

/// Enum or flags option set, as surfaced to the CLI `options`
/// command and to "did you mean?" value parsing.
pub enum TagOptions<'a> {
    /// Enum field — a single integer value picked from a named set.
    /// `current` is the stored value (or `None` if it didn't resolve);
    /// `names` lists every variant name in declaration order.
    Enum { names: Vec<&'a str>, current: Option<i64> },
    /// Flags field — one entry per named bit with its current state.
    Flags(Vec<TagFlagOption<'a>>),
}

/// One named bit in a flags field's declaration.
#[derive(Debug, Clone, Copy)]
pub struct TagFlagOption<'a> {
    /// Display name of this bit.
    pub name: &'a str,
    /// Bit position (0-based).
    pub bit: u32,
    /// `true` if this bit is set in the field's current value.
    pub is_set: bool,
}

/// A single flag bit addressed by name.
pub struct TagFlag<'a> {
    field: TagField<'a>,
    bit: u32,
}

impl<'a> TagFlag<'a> {
    /// Display name of this bit.
    pub fn name(&self) -> &'a str { self.field.flag_from_bit(self.bit) }

    /// Bit position (0-based) within the flags field.
    pub fn bit(&self) -> u32 { self.bit }

    /// `true` if this bit is set in the field's current value.
    pub fn is_set(&self) -> bool {
        self.field.value().and_then(|v| v.flag_bit(self.bit)).unwrap_or(false)
    }
}

//================================================================================
// Write-side: mirrors of the read types
//================================================================================

/// Mutable counterpart of [`TagStruct`].
pub struct TagStructMut<'a> {
    layout: &'a TagLayout,
    struct_data: &'a mut TagStructData,
    struct_raw: &'a mut [u8],
}

impl<'a> TagStructMut<'a> {
    /// Re-borrow as a read-only [`TagStruct`] for inspection.
    pub fn as_ref(&self) -> TagStruct<'_> {
        TagStruct {
            layout: self.layout,
            struct_data: &*self.struct_data,
            struct_raw: &*self.struct_raw,
        }
    }

    /// Resolve a single field by name (case-sensitive, no path
    /// descent).
    pub fn field_mut(&mut self, name: &str) -> Option<TagFieldMut<'_>> {
        let field_index = self.struct_data.find_field_by_name(self.layout, name)?;
        Some(TagFieldMut {
            layout: self.layout,
            struct_data: &mut *self.struct_data,
            struct_raw: &mut *self.struct_raw,
            field_index,
        })
    }

    /// Resolve a `/`-separated field path. Mirrors
    /// [`TagStruct::field_path`].
    pub fn field_path_mut(&mut self, path: &str) -> Option<TagFieldMut<'_>> {
        let cursor = crate::path::lookup_mut_from_struct(
            self.layout, &mut *self.struct_data, &mut *self.struct_raw, path,
        )?;
        Some(TagFieldMut {
            layout: self.layout,
            struct_data: cursor.struct_data,
            struct_raw: cursor.struct_raw,
            field_index: cursor.field_index,
        })
    }

    /// Walk the struct's fields in declaration order, yielding a
    /// mutable handle for each. Mirrors [`TagStruct::fields`]'s
    /// filtering (skips padding, explanations, terminators, unknown).
    ///
    /// Uses a visitor closure rather than returning an iterator
    /// because each yielded [`TagFieldMut`] reborrows through `self`
    /// — Rust's borrow checker rules out giving out multiple
    /// simultaneous `&mut` iterators.
    pub fn for_each_field_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(TagFieldMut<'_>),
    {
        let layout = self.layout;
        let struct_index = self.struct_data.struct_index as usize;
        let start = layout.struct_layouts[struct_index].first_field_index as usize;

        let mut i = start;
        loop {
            let ft = layout.fields[i].field_type;
            if ft == TagFieldType::Terminator {
                break;
            }
            let is_padding = matches!(
                ft,
                TagFieldType::Pad | TagFieldType::UselessPad | TagFieldType::Skip
                    | TagFieldType::Explanation | TagFieldType::Unknown,
            );
            if !is_padding {
                f(TagFieldMut {
                    layout,
                    struct_data: &mut *self.struct_data,
                    struct_raw: &mut *self.struct_raw,
                    field_index: i,
                });
            }
            i += 1;
        }
    }
}

/// Mutable counterpart of [`TagField`].
pub struct TagFieldMut<'a> {
    layout: &'a TagLayout,
    struct_data: &'a mut TagStructData,
    struct_raw: &'a mut [u8],
    field_index: usize,
}

impl<'a> TagFieldMut<'a> {
    /// Re-borrow as a read-only [`TagField`] for inspection.
    pub fn as_ref(&self) -> TagField<'_> {
        TagField {
            layout: self.layout,
            struct_data: &*self.struct_data,
            struct_raw: &*self.struct_raw,
            field_index: self.field_index,
        }
    }

    /// Write `value`. Returns [`TagSetError::NotAssignable`] for
    /// container fields (struct/block/array/pageable_resource) —
    /// those must be mutated via [`TagFieldMut::as_struct_mut`] /
    /// `as_block_mut` / `as_array_mut`.
    pub fn set(&mut self, value: TagFieldData) -> Result<(), TagSetError> {
        let ft = self.layout.fields[self.field_index].field_type;
        if matches!(
            ft,
            TagFieldType::Struct
                | TagFieldType::Block
                | TagFieldType::Array
                | TagFieldType::PageableResource,
        ) {
            return Err(TagSetError::NotAssignable);
        }
        self.struct_data.set_field(self.layout, &mut *self.struct_raw, self.field_index, value);
        Ok(())
    }

    /// Look up a single flag by name and return a mutable handle.
    pub fn flag_mut(&mut self, name: &str) -> Option<TagFlagMut<'_>> {
        let field = &self.layout.fields[self.field_index];
        let bit = find_flag_bit(self.layout, field, name)?;
        Some(TagFlagMut {
            field: TagFieldMut {
                layout: self.layout,
                struct_data: &mut *self.struct_data,
                struct_raw: &mut *self.struct_raw,
                field_index: self.field_index,
            },
            bit,
        })
    }

    /// Same shape-vs-missing distinction as [`TagField::as_struct`] —
    /// Returns `None` either when this isn't a struct field OR when
    /// its sub-chunk is missing on the loaded tag.
    pub fn as_struct_mut(&mut self) -> Option<TagStructMut<'_>> {
        if self.layout.fields[self.field_index].field_type != TagFieldType::Struct {
            return None;
        }
        let field_index = self.field_index;
        let (struct_data, struct_raw) = self
            .struct_data
            .nested_struct_mut(self.layout, &mut *self.struct_raw, field_index)?;
        Some(TagStructMut { layout: self.layout, struct_data, struct_raw })
    }

    /// Same shape-vs-missing distinction as [`TagField::as_block`].
    pub fn as_block_mut(&mut self) -> Option<TagBlockMut<'_>> {
        if self.layout.fields[self.field_index].field_type != TagFieldType::Block {
            return None;
        }
        let field_index = self.field_index;
        let block_data = self
            .struct_data
            .sub_chunks
            .iter_mut()
            .find(|e| e.field_index == Some(field_index as u32))
            .and_then(|e| match &mut e.content {
                TagSubChunkContent::Block(b) => Some(b),
                _ => None,
            })?;
        Some(TagBlockMut { layout: self.layout, block_data })
    }

    /// Same shape-vs-missing distinction as [`TagField::as_array`].
    pub fn as_array_mut(&mut self) -> Option<TagArrayMut<'_>> {
        let field = &self.layout.fields[self.field_index];
        if field.field_type != TagFieldType::Array {
            return None;
        }
        let array_layout_index = field.definition;
        let array_def = &self.layout.array_layouts[array_layout_index as usize];
        let element_size = self.layout.struct_layouts[array_def.struct_index as usize].size;
        let start = field.offset as usize;

        let field_index = self.field_index;
        let elements = self
            .struct_data
            .sub_chunks
            .iter_mut()
            .find(|e| e.field_index == Some(field_index as u32))
            .and_then(|e| match &mut e.content {
                TagSubChunkContent::Array(elements) => Some(elements.as_mut_slice()),
                _ => None,
            })?;
        let end = start + elements.len() * element_size;
        let array_raw = &mut self.struct_raw[start..end];
        Some(TagArrayMut {
            layout: self.layout,
            array_layout_index,
            array_raw,
            elements,
        })
    }
}

/// Mutable counterpart of [`TagBlock`]. All structural edits
/// (`add`/`insert`/`delete`/`clear`) funnel through here so callers
/// never touch `TagBlockData` directly.
pub struct TagBlockMut<'a> {
    layout: &'a TagLayout,
    block_data: &'a mut TagBlockData,
}

impl<'a> TagBlockMut<'a> {
    /// The schema side of this block — its block definition.
    /// Bridges to the [`crate::definition`] facade.
    pub fn definition(&self) -> crate::TagBlockDefinition<'_> {
        crate::TagBlockDefinition::new(self.layout, self.block_data.block_index as usize)
    }

    /// Number of elements currently in this block.
    pub fn len(&self) -> usize { self.block_data.elements.len() }

    /// `true` when this block has zero elements.
    pub fn is_empty(&self) -> bool { self.block_data.elements.is_empty() }

    /// Mutable handle to the element at `index`. `None` if out of range.
    pub fn element_mut(&mut self, index: usize) -> Option<TagStructMut<'_>> {
        if index >= self.block_data.elements.len() {
            return None;
        }
        let size = block_element_size(self.layout, &*self.block_data);
        let start = index * size;
        let struct_data = &mut self.block_data.elements[index];
        let struct_raw = &mut self.block_data.raw_data[start..start + size];
        Some(TagStructMut { layout: self.layout, struct_data, struct_raw })
    }

    /// Walk the block's elements in order, yielding a mutable handle
    /// for each. Visitor-closure form for the same borrow-checker
    /// reason as [`TagStructMut::for_each_field_mut`].
    pub fn for_each_element_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(TagStructMut<'_>),
    {
        let layout = self.layout;
        let size = block_element_size(layout, &*self.block_data);
        let count = self.block_data.elements.len();
        for i in 0..count {
            let start = i * size;
            let struct_data = &mut self.block_data.elements[i];
            let struct_raw = &mut self.block_data.raw_data[start..start + size];
            f(TagStructMut { layout, struct_data, struct_raw });
        }
    }

    /// Append a default-initialized element. Returns its new index.
    pub fn add_element(&mut self) -> usize {
        self.block_data.add_element(self.layout);
        self.block_data.elements.len() - 1
    }

    /// Insert a default element at `index`. Error on out-of-range
    /// (valid range is `0..=len`).
    pub fn insert_element(&mut self, index: usize) -> Result<(), TagIndexError> {
        let len = self.block_data.elements.len();
        if index > len {
            return Err(TagIndexError::OutOfRange { index, len });
        }
        self.block_data.insert_element(self.layout, index);
        Ok(())
    }

    /// Duplicate element `index`, placing the copy at `index + 1`.
    /// Returns the copy's index.
    pub fn duplicate_element(&mut self, index: usize) -> Result<usize, TagIndexError> {
        let len = self.block_data.elements.len();
        if index >= len {
            return Err(TagIndexError::OutOfRange { index, len });
        }
        self.block_data.duplicate_element(self.layout, index);
        Ok(index + 1)
    }

    /// Remove the element at `index`. Error on out-of-range.
    pub fn delete_element(&mut self, index: usize) -> Result<(), TagIndexError> {
        let len = self.block_data.elements.len();
        if index >= len {
            return Err(TagIndexError::OutOfRange { index, len });
        }
        self.block_data.remove_element(self.layout, index);
        Ok(())
    }

    /// Swap elements at `i` and `j`.
    pub fn swap_elements(&mut self, i: usize, j: usize) -> Result<(), TagIndexError> {
        let len = self.block_data.elements.len();
        if i >= len {
            return Err(TagIndexError::OutOfRange { index: i, len });
        }
        if j >= len {
            return Err(TagIndexError::OutOfRange { index: j, len });
        }
        self.block_data.swap_elements(self.layout, i, j);
        Ok(())
    }

    /// Move the element at `from` to final position `to` (Vec::remove
    /// + Vec::insert semantics).
    pub fn move_element(&mut self, from: usize, to: usize) -> Result<(), TagIndexError> {
        let len = self.block_data.elements.len();
        if from >= len {
            return Err(TagIndexError::OutOfRange { index: from, len });
        }
        if to >= len {
            return Err(TagIndexError::OutOfRange { index: to, len });
        }
        self.block_data.move_element(self.layout, from, to);
        Ok(())
    }

    /// Remove every element.
    pub fn clear(&mut self) { self.block_data.clear(); }
}

/// Mutable counterpart of [`TagArray`]. Arrays are fixed-count, so no
/// add/remove — only per-element mutation.
pub struct TagArrayMut<'a> {
    layout: &'a TagLayout,
    array_layout_index: u32,
    array_raw: &'a mut [u8],
    elements: &'a mut [TagStructData],
}

impl<'a> TagArrayMut<'a> {
    /// The schema side of this array — its array definition.
    /// Bridges to the [`crate::definition`] facade.
    pub fn definition(&self) -> crate::TagArrayDefinition<'_> {
        crate::TagArrayDefinition::new(self.layout, self.array_layout_index as usize)
    }

    /// Schema-declared element count.
    pub fn len(&self) -> usize { self.elements.len() }

    /// `true` when the schema declares zero elements (rare but
    /// permitted).
    pub fn is_empty(&self) -> bool { self.elements.is_empty() }

    /// Mutable handle to the element at `index`. `None` if out of range.
    pub fn element_mut(&mut self, index: usize) -> Option<TagStructMut<'_>> {
        if index >= self.elements.len() {
            return None;
        }
        let size = element_struct_size(self.layout, self.array_layout_index);
        let start = index * size;
        let struct_data = &mut self.elements[index];
        let struct_raw = &mut self.array_raw[start..start + size];
        Some(TagStructMut { layout: self.layout, struct_data, struct_raw })
    }

    /// Swap elements at `i` and `j`. Arrays are fixed-count so
    /// reordering is the only structural edit available.
    pub fn swap(&mut self, i: usize, j: usize) -> Result<(), TagIndexError> {
        let len = self.elements.len();
        if i >= len {
            return Err(TagIndexError::OutOfRange { index: i, len });
        }
        if j >= len {
            return Err(TagIndexError::OutOfRange { index: j, len });
        }
        if i == j {
            return Ok(());
        }
        self.elements.swap(i, j);
        let size = element_struct_size(self.layout, self.array_layout_index);
        let (lo, hi) = if i < j { (i, j) } else { (j, i) };
        let lo_start = lo * size;
        let hi_start = hi * size;
        let mut buf = vec![0u8; size];
        buf.copy_from_slice(&self.array_raw[lo_start..lo_start + size]);
        self.array_raw.copy_within(hi_start..hi_start + size, lo_start);
        self.array_raw[hi_start..hi_start + size].copy_from_slice(&buf);
        Ok(())
    }

    /// Walk the array's elements in order, yielding a mutable handle
    /// for each. Visitor-closure form mirroring
    /// [`TagBlockMut::for_each_element_mut`].
    pub fn for_each_element_mut<F>(&mut self, mut f: F)
    where
        F: FnMut(TagStructMut<'_>),
    {
        let layout = self.layout;
        let size = element_struct_size(layout, self.array_layout_index);
        let count = self.elements.len();
        for i in 0..count {
            let start = i * size;
            let struct_data = &mut self.elements[i];
            let struct_raw = &mut self.array_raw[start..start + size];
            f(TagStructMut { layout, struct_data, struct_raw });
        }
    }
}

/// Mutable single-flag handle.
pub struct TagFlagMut<'a> {
    field: TagFieldMut<'a>,
    bit: u32,
}

impl<'a> TagFlagMut<'a> {
    /// Display name of this bit.
    pub fn name(&self) -> &str {
        self.field.as_ref().flag_from_bit(self.bit)
    }

    /// Bit position (0-based) within the flags field.
    pub fn bit(&self) -> u32 { self.bit }

    /// `true` if this bit is currently set.
    pub fn is_set(&self) -> bool {
        self.field.as_ref().value().and_then(|v| v.flag_bit(self.bit)).unwrap_or(false)
    }

    /// Set or clear this bit.
    pub fn set(&mut self, on: bool) {
        let Some(mut value) = self.field.as_ref().value() else { return };
        if value.set_flag_bit(self.bit, on) {
            let _ = self.field.set(value);
        }
    }

    /// Toggle and return the new state.
    pub fn toggle(&mut self) -> bool {
        let new_state = !self.is_set();
        self.set(new_state);
        new_state
    }
}

impl<'a> TagField<'a> {
    /// Resolve bit `bit`'s display name via this field's string list.
    /// Internal helper shared between [`TagFlag::name`] and
    /// [`TagFlagMut::name`].
    fn flag_from_bit(&self, bit: u32) -> &'a str {
        let field = &self.layout.fields[self.field_index];
        let Some(string_list) = self.layout.string_lists.get(field.definition as usize) else {
            return "";
        };
        if bit >= string_list.count {
            return "";
        }
        let offset_index = (string_list.first + bit) as usize;
        let Some(&string_offset) = self.layout.string_offsets.get(offset_index) else {
            return "";
        };
        self.layout.get_string(string_offset).unwrap_or("")
    }
}

//================================================================================
// Errors
//================================================================================

/// Failure modes for [`TagFieldMut::set`].
#[derive(Debug)]
pub enum TagSetError {
    /// The supplied [`TagFieldData`] variant doesn't match the
    /// field's schema type.
    TypeMismatch { expected: &'static str, got: &'static str },
    /// The field is a container — use `as_block_mut()` / etc.
    NotAssignable,
}

/// Failure modes for block / array structural edits.
#[derive(Debug)]
pub enum TagIndexError {
    /// An index argument was outside the block / array's `0..len` range.
    OutOfRange { index: usize, len: usize },
}
