//! `particle_model` (`pmdf`) tag walker — mesh-particle geometry.
//!
//! A particle system whose `appearance` flags the system as a *model*
//! particle (engine `c_particle_definition::m_appearance_flags & 0x8000`,
//! the `IS_PARTICLE_MODEL` shader path) renders each particle as an
//! instance of this tag's mesh instead of a screen-aligned billboard
//! quad. The gravlift "slipspace tube", contrail rings, etc. are model
//! particles.
//!
//! ## Layout (`particle_model_struct_definition`, schema-driven)
//!
//! The root struct is intentionally tiny — two sub-structs:
//!   - `render geometry` (`global_render_geometry_struct`, 132B) — the
//!     SAME schema render_model / sbsp / lightmap use, so it's decoded
//!     here by reusing [`crate::render_model::read_geometry`] into the
//!     shared [`Geometry`] type (meshes, parts, compression bounds,
//!     vertex/index buffers in `per mesh temporary`). One mesh of this
//!     geometry becomes one mesh-particle instance.
//!   - `m_gpu_data` (`gpu_data_struct`) — per-variant packed GPU
//!     constant registers (`m_variants[].runtime m_count[].runtime
//!     gpu_real`). Selects which mesh slice / vertices-per-particle a
//!     given emitter variant draws.
//!
//! ## `m_gpu_data` is runtime-resolved
//!
//! The `m_variants` block carries one element per authored variant, but
//! the inner `runtime m_count` register arrays are filled by tool.exe at
//! cache-compile time. **Source / loose tags read here have the variant
//! elements present but their register arrays empty** — so
//! [`GpuVariant::registers`] is typically empty off-disk. The render
//! geometry, however, is fully present in source tags, so consumers can
//! render the mesh directly. To extract renderer-ready meshes call
//! [`crate::render_model::extract_render_geometry_meshes`] on
//! [`ParticleModel::render_geometry_root`].

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::render_model::{
    extract_render_geometry_meshes, read_geometry, Geometry, RenderMesh, RenderModelError,
};

const PMDF_GROUP: [u8; 4] = *b"pmdf";

#[derive(Debug)]
pub enum ParticleModelError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
    /// The shared render-geometry decode failed (missing / malformed
    /// `render geometry` struct).
    Geometry(RenderModelError),
}

impl std::fmt::Display for ParticleModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "particle_model: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
            Self::Geometry(e) => write!(f, "particle_model: render geometry: {e}"),
        }
    }
}

impl std::error::Error for ParticleModelError {}

impl From<RenderModelError> for ParticleModelError {
    fn from(e: RenderModelError) -> Self {
        Self::Geometry(e)
    }
}

/// One `m_variants` element — the packed GPU constant registers tool.exe
/// resolves per authored variant. Empty off-disk (see module docs); when
/// present, register `[0]` is the variant's vertices-per-particle.
#[derive(Debug, Clone, Default)]
pub struct GpuVariant {
    /// `runtime m_count[].runtime gpu_real` — the constant-register
    /// floats for this variant, in order.
    pub registers: Vec<f32>,
}

/// Walked `particle_model` tag.
#[derive(Debug, Clone, Default)]
pub struct ParticleModel {
    /// Shared render geometry (meshes, parts, compression, vertex/index
    /// buffers) decoded by the same path render_model uses.
    pub render_geometry: Geometry,
    /// Render-ready meshes (vertices decompressed through the compression
    /// bounds, strips decoded to triangle lists) — the same layer render_model
    /// / decorators consume. One mesh-particle instance is the union of all of
    /// these. Consumers reconstruct the GPU particle-model vertex stream + the
    /// per-variant `vertices_per_particle` (`variant.end_index`, a tool.exe
    /// cache output absent off-disk) from this. Empty on a decode error.
    pub render_meshes: Vec<RenderMesh>,
    /// Per-authored-variant packed GPU registers. One entry per
    /// `m_gpu_data/m_variants` element; `registers` empty in source tags.
    pub variants: Vec<GpuVariant>,
}

impl ParticleModel {
    pub fn from_tag(tag: &TagFile) -> Result<Self, ParticleModelError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != PMDF_GROUP {
            return Err(ParticleModelError::WrongGroup { expected: PMDF_GROUP, actual });
        }
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, ParticleModelError> {
        // Reconstruct the render-ready meshes from the `render geometry` root
        // (the same path render_model / decorators use). A decode failure leaves
        // it empty — the particle simply has no drawable model geometry.
        let render_meshes = Self::render_geometry_root(s)
            .and_then(|root| extract_render_geometry_meshes(&root).ok())
            .unwrap_or_default();
        Ok(Self {
            render_geometry: read_geometry(s)?,
            render_meshes,
            variants: read_gpu_variants(s),
        })
    }

    /// Authored variant count (length of `m_gpu_data/m_variants`).
    pub fn variant_count(&self) -> usize {
        self.variants.len()
    }

    /// Mesh count from the decoded render geometry.
    pub fn mesh_count(&self) -> usize {
        self.render_geometry.meshes.len()
    }

    /// The `render geometry` sub-struct root, for callers that want to
    /// run [`crate::render_model::extract_render_geometry_meshes`]
    /// directly against this tag.
    pub fn render_geometry_root<'a>(s: &TagStruct<'a>) -> Option<TagStruct<'a>> {
        s.field("render geometry").and_then(|f| f.as_struct())
    }
}

/// Decode `m_gpu_data/m_variants[]` into [`GpuVariant`]s. Each variant's
/// `runtime m_count` is an array of single-`runtime gpu_real` elements.
fn read_gpu_variants(root: &TagStruct<'_>) -> Vec<GpuVariant> {
    let Some(variants) = root
        .field("m_gpu_data")
        .and_then(|f| f.as_struct())
        .and_then(|gpu| gpu.field("m_variants"))
        .and_then(|f| f.as_block())
    else {
        return Vec::new();
    };
    (0..variants.len())
        .filter_map(|v| variants.element(v))
        .map(|variant| {
            let registers = variant
                .field("runtime m_count")
                .and_then(|f| f.as_array())
                .map(|arr| {
                    (0..arr.len())
                        .filter_map(|k| arr.element(k))
                        .map(|e| e.read_real("runtime gpu_real").unwrap_or(0.0))
                        .collect()
                })
                .unwrap_or_default();
            GpuVariant { registers }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a real model particle (descent gravlift "slipspace tube")
    /// end-to-end: full walker + shared mesh extractor. Skips silently if
    /// the loose tag isn't present on this machine.
    #[test]
    fn decode_slipspace_tube_mesh() {
        let p = "/Users/camden/Halo/halo3_mcc/tags/fx/particles/models/mesh_fx/slipspace_tube/slipspace_tube.particle_model";
        let Ok(tag) = TagFile::read(p) else {
            eprintln!("skip: slipspace_tube.particle_model not found");
            return;
        };
        let pm = ParticleModel::from_tag(&tag).expect("decode pmdf");
        let root = tag.root();
        // The mesh extractor reads `root.field("render geometry")` itself, so
        // it takes the tag root (same as render_model / sbsp).
        let meshes =
            crate::render_model::extract_render_geometry_meshes(&root).expect("extract meshes");
        eprintln!(
            "slipspace_tube: variants={} mesh_count={} extracted_meshes={}",
            pm.variant_count(),
            pm.mesh_count(),
            meshes.len()
        );
        for (i, m) in meshes.iter().enumerate() {
            eprintln!(
                "  mesh[{i}]: verts={} indices={} parts={}",
                m.vertices.len(),
                m.indices.len(),
                m.parts.len()
            );
        }
        assert!(pm.mesh_count() >= 1, "expected >=1 mesh");
        assert!(meshes.iter().any(|m| !m.vertices.is_empty()), "expected vertices");
        assert!(meshes.iter().any(|m| !m.indices.is_empty()), "expected indices");
    }
}
