//! Cooked *unversioned* property serialization — the public face of
//! [`super`], kept at its historical path.
//!
//! Cooked IoStore packages serialize object properties without per-property
//! tags: an `FUnversionedHeader` (a run of `skip`/`value` fragments plus an
//! optional zero-mask) says *which* schema properties are present, and the
//! values follow back-to-back. The schema (property order + types) comes from
//! the `.usmap`. Property order within a class is **derived→base**, matching
//! the engine's `UStruct::PropertyLink` walk — see
//! [`Usmap::flattened_properties`](super::usmap::Usmap::flattened_properties).
//!
//! The implementation is split by concern; this module only re-exports it:
//!
//! | Module | Concern |
//! |---|---|
//! | [`super::archive`] | the byte cursor and what it resolves against |
//! | [`super::block`] | the `FUnversionedHeader` codec and the property-block walk |
//! | [`super::property`] | one property value, by schema type |
//! | [`super::structs`] | engine structs that serialize natively |
//! | [`super::text`] | `FText` |
//! | [`super::tails`] | each class's natively serialized tail |
//! | [`super::export`] | a whole export, end to end |
//! | [`super::reflect`] | schema recovered from package bytes |
//! | [`crate::iostore::asset::mesh_sync`] | Campaign Evolved mesh-sync extraction |

pub use super::archive::{ExportContext, PackageResolver};
pub use super::edit::{
    intern_name, remove_property, set_name_property, set_property, set_property_slot,
    set_string_property,
};
pub use super::export::{
    read_export, read_export_struct, read_export_struct_len, read_export_with_trailer,
    walk_export, TailStop, TailWalk,
    write_export, Export, Trailer, NO_PROPERTY_BLOCK,
};
pub use super::property::{can_serialize_as_zero, should_save_as_zero};
pub use super::tail_models::{
    roundtrip_tail, BodySetupTail, CookedFormat, InlineShaderMaps, MaterialChainTail,
    StaticMeshComponentChainTail,
    StaticMeshComponentTail, TailContext,
    TextureChainTail, TextureCookedData, TextureMip, TexturePlatformData, MODELED_TAILS,
};
pub use super::reflect::{read_datatable, read_ufunction_script, read_userdefined_struct_layout};
pub use super::value::{
    BlockLayout, FStr, MeshTransform, PropValue, PropertyBlock, PropertyEntry, SchemaSlot,
    SoftObjectPath,
};
pub use crate::iostore::asset::mesh_sync::{
    read_material_slots, MaterialSlot, MeshRef, MeshSyncRegions, Permutation, Region,
};
