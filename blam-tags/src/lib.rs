//! # blam-tags
//!
//! A standalone Rust library for reading, writing, and manipulating
//! Halo 3 / Reach tag files. No ManagedBlam.dll, no .NET, no engine
//! required. Built around a byte-exact roundtrip read/write path
//! with a concept-oriented facade layered on top.
//!
//! ## Quick start
//!
//! ```no_run
//! use blam_tags::TagFile;
//!
//! let mut tag = TagFile::read("masterchief.biped").unwrap();
//!
//! // Read a field by `/`-separated path. Pattern-match on the
//! // returned `TagFieldData` to render the value — the library no
//! // longer ships a `Display` impl (callers own their UI).
//! let jump = tag.root().field_path("jump velocity").unwrap();
//! let value = jump.value().unwrap();
//! println!("{} ({}): {:?}", jump.name(), jump.type_name(), value);
//!
//! // Toggle a flag by name.
//! tag.root_mut()
//!     .field_path_mut("unit/flags").unwrap()
//!     .flag_mut("has_hull").unwrap()
//!     .toggle();
//!
//! tag.write("masterchief.biped.edited").unwrap();
//! ```
//!
//! ## Module tour
//!
//! **High-level facade (start here):**
//!
//! - [`api`] — data-side facade: [`TagStruct`], [`TagField`],
//!   [`TagBlock`], [`TagArray`], [`TagFlag`], [`TagResource`], and
//!   their mutable counterparts. All reachable from [`TagFile`].
//! - [`definition`] — schema-side facade: [`TagStructDefinition`],
//!   [`TagFieldDefinition`], [`TagBlockDefinition`],
//!   [`TagArrayDefinition`], reachable from [`TagFile::definitions`].
//! - [`file::TagFile`] — the fully parsed tag file (re-exported as
//!   [`TagFile`]).
//!
//! **Lower-level, used by the facade:**
//!
//! - [`fields`] — [`TagFieldType`] dispatch enum, [`TagFieldData`]
//!   per-field value enum, plus `deserialize_field` /
//!   `serialize_field`. No `Display` impl — UI rendering is the
//!   caller's responsibility (the shell crate has formatters).
//! - [`layout`] — [`layout::TagLayout`] (the schema chunk) and all
//!   its record types, plus the binary `blay`-chunk read/write path.
//! - [`schema`] — JSON schema import: [`schema::TagSchemaError`],
//!   [`schema::TagGroupMeta`], plus
//!   [`TagLayout::from_json`][layout::TagLayout::from_json] and the
//!   parent-chain merge that powers it.
//! - [`error`] — [`TagReadError`] returned by every read-path entry
//!   point, plus internal helpers shared by the read-side modules.
//! - [`data`] — per-tag instance data storage (opaque; driven through
//!   the [`api`] facade).
//! - [`path`] — `/`-separated path navigation (crate-internal).
//! - [`stream`] — the `tag!` / `want` / `info` outer stream chunks.
//! - [`io`] — primitive readers/writers + 12-byte chunk header helpers.
//! - [`math`] — bounds, colors, vectors, points, euler angles.

pub mod math;
pub mod io;
pub mod error;
pub mod fields;
pub mod field_name;
pub mod layout;
pub mod schema;
pub mod schema_compare;
pub mod schema_compat;
pub mod data;
pub mod path;
pub mod field_path;
pub mod stream;
pub mod file;
pub mod classic;
pub mod api;
pub mod definition;
pub mod bitmap;
pub mod animation;
pub mod geometry;
pub mod game;
pub mod jms;
pub mod ass;
pub mod extract;
pub mod render_geometry;
pub mod render_model;
pub mod monolithic;
pub mod tag_function;
pub mod render_method;
pub mod paths;
pub mod typed_enums;

// Both of these carry their own `//!` docs. An outer doc comment here would be
// *merged* with those, and rustdoc then resolves the merged text against the
// crate root — silently breaking every intra-doc link the module writes to its
// own children. Describe the module in the module.
#[cfg(feature = "audio")]
pub mod audio;

#[cfg(feature = "iostore")]
pub mod iostore;

// Facade re-exports — the recommended surface for editing tags.
pub use api::{
    TagArray, TagArrayMut, TagBlock, TagBlockElement, TagBlockMut, TagField, TagFieldMut, TagFlag,
    TagFlagMut, TagFlagOption, TagGroup, TagIndexError, TagOptions, TagPasteError, TagResource,
    TagResourceKind, TagSetError, TagStruct, TagStructMut,
};
pub use definition::{
    TagApiInteropDefinition, TagArrayDefinition, TagBlockDefinition, TagDefinitions,
    TagFieldDefinition, TagResourceDefinition, TagStructDefinition,
};
pub use fields::{
    format_group_tag, parse_group_tag, ApiInteropData, StringIdData, TagFieldData, TagFieldType,
    TagReferenceData,
};
pub use field_name::{clean_field_name, parse_field_name, FieldNameInfo};
pub use field_path::{TagFieldPath, TagFieldPathSegment};
pub use error::TagReadError;
pub use typed_enums::{Enum, Flags, SchemaEnum, TagInt};
pub use file::TagFile;
pub use io::Endian;
pub use layout::TagLayout;
pub use schema::{TagGroupMeta, TagSchemaError};
pub use schema_compare::{
    compare_root_layout, field_key, FieldDiff, FieldKey, LayoutComparison, LayoutSeverity,
};
pub use schema_compat::{struct_trees_are_wire_identical, StructIndexMap, WireMismatch};
pub use bitmap::{Bitmap, BitmapError, BitmapFormat, BitmapImage};
pub use jms::{
    JmsBox, JmsCapsule, JmsConvex, JmsError, JmsFile, JmsHinge, JmsMarker, JmsMaterial,
    JmsNode, JmsRagdoll, JmsSphere, JmsTriangle, JmsVertex,
};
pub use ass::{
    AssError, AssFile, AssInstance, AssLight, AssLightKind, AssMaterial, AssObject,
    AssObjectPayload, AssTriangle, AssVertex,
};
pub use render_model::{
    extract_per_instance_lightmap_uvs, Geometry, Marker, MarkerGroup, Material, MaterialProperty,
    Mesh, Node, Part, PerInstanceLightmapUvs, Permutation, Region, RenderMesh, RenderMeshPart,
    RenderModel, RenderModelError, RenderVertex, Subpart,
};
pub use tag_function::{
    ColorGraphType, FunctionFlags, FunctionKind, FunctionType, TagFunction, TagFunctionError,
    TagFunctionHeader, default_function_definition_bytes,
};
pub use tag_function::curve::{CurvePointMode, CurveSegmentType};
pub use tag_function::editor::{
    ExponentParams, FoundationMasterType, FunctionEditError, PeriodicParams, TagFunctionEditor,
    TransitionParams, PERIODIC_FUNCTIONS, TRANSITION_FUNCTIONS,
};
pub use animation::{
    Animation, AnimationClip, AnimationError, AnimationGraph, AnimationGroup, AnimationName,
    AnimationStateType, AnimationTracks, AnimatedStreamStatus, BitArray, Codec, JmaKind,
    MovementData, MovementFrame, MovementKind, NodeFlags, NodeTransform, ObjectSpaceParentNode,
    PackedDataSizes, Pose, SizeLayout, Skeleton, SkeletonNode,
};
