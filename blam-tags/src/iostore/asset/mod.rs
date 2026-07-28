//! Layer 4 — typed decoders built on [`super::object`].
//!
//! Where the layers below are general UE machinery, these read one specific
//! asset kind out of the values that machinery produces.

pub mod nanite;
pub mod skeletal_mesh;
pub mod static_mesh;
pub mod wwise_event;
