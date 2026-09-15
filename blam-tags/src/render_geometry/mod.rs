//! Render-geometry resource decoding.
//!
//! The render-geometry API resource (`render_geometry_api_resource`
//! in the schema) carries the per-mesh vertex and index buffers. On
//! MCC PC tags these buffers are inline under `per mesh temporary[i]`
//! in the author format; on Halo 4 X360 monolithic builds they live
//! in the pageable cache via an xsync resource.
//!
//! This module's job is to bridge the two: when the author-format
//! data is empty but the pageable resource is present, decode the
//! GPU buffers and synthesize the author-format records, so the
//! existing JMS / ASS exporter walks them without knowing about the
//! GPU path.
//!
//! The first piece is [`vertex_type`] — a stable Rust enum that
//! covers every variant the schema's `mesh_vertex_type_definition`
//! declares. Resolution goes through the schema's option-string
//! table so it's robust to per-build index shuffling.

pub mod decode;
pub mod hydrate;
pub mod resource;
pub mod vertex_type;

pub use decode::{decode_vertex_buffer, AuthorVertex, VertexDecodeError};
pub use hydrate::{author_geometry_populated, hydrate, HydrateError, API_RESOURCE_FIELD};
pub use resource::{IndexBufferDescriptor, RenderGeometryResource, VertexBufferDescriptor};
pub use vertex_type::{MeshPrtVertexType, MeshVertexType};
