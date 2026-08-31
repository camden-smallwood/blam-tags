//! The raw-`.hlsl` compile path (`tool.exe`'s `shaders`/`shader` verbs,
//! `sub_14009D0F0`): parse a source file's `@generate` / `@entry` /
//! `@compute_shader` directives, compile every (entry × generate) variant for
//! the vertex, pixel and (optional) compute stages, and emit the
//! `vertex_shader` / `pixel_shader` / `compute_shader` tags.
//!
//! This is the path proven byte-exact by the census — it covers the ~200
//! `rasterizer\shaders\*` shaders the kit builds straight from HLSL, which is
//! what the byte-order conversion path needs to regenerate.

use super::emit::Variant;
use super::entry::{entry_by_name, Stage, VERTEX_TYPE_MACRO};
use super::macros::Platform;
use super::{CompileOutcome, ShaderCompiler};

/// Directives parsed from a raw `.hlsl` source.
#[derive(Debug, Default, Clone)]
pub struct Directives {
    /// Vertex-type ordinals from `@generate`.
    pub generates: Vec<usize>,
    /// Entry ordinals from `@entry` (defaults to `[default]` when none given).
    pub entries: Vec<usize>,
    /// `@compute_shader` present.
    pub compute: bool,
}

/// Resolve a `@generate <name>` token to a vertex-type ordinal. The directive
/// names are the engine's short aliases (`sub_140C586C0`); fall back to the
/// `s_*_vertex` macro names and their short forms.
pub fn generate_to_vertex_type(name: &str) -> Option<usize> {
    let n = name.trim();
    let alias = match n {
        "screen" => "s_screen_vertex",
        "world" => "s_world_vertex",
        "rigid" => "s_rigid_vertex",
        "skinned" => "s_skinned_vertex",
        "particle_model" => "s_particle_model_vertex",
        "flat_world" => "s_flat_world_vertex",
        "flat_rigid" => "s_flat_rigid_vertex",
        "flat_skinned" => "s_flat_skinned_vertex",
        "debug" => "s_debug_vertex",
        "transparent" => "s_transparent_vertex",
        "chud_simple" => "s_chud_vertex_simple",
        "chud_fancy" => "s_chud_vertex_fancy",
        "decorator" => "s_decorator_vertex",
        "tiny_position_only" => "s_tiny_position_vertex",
        "patchy_fog" => "s_patchy_fog_vertex",
        other => other,
    };
    VERTEX_TYPE_MACRO
        .iter()
        .position(|v| *v == alias)
        .or_else(|| super::entry::vertex_type_by_name(n))
}

/// Parse `@generate` / `@entry` / `@compute_shader` from source text. Directives
/// appear as `//@name ...` or `@name ...` on their own line.
pub fn parse_directives(source: &[u8]) -> Directives {
    let text = String::from_utf8_lossy(source);
    let mut d = Directives::default();
    for line in text.lines() {
        let l = line.trim_start().trim_start_matches('/').trim();
        if let Some(rest) = l.strip_prefix("@generate") {
            if let Some(name) = rest.trim().split_whitespace().next() {
                if let Some(vt) = generate_to_vertex_type(name) {
                    if !d.generates.contains(&vt) {
                        d.generates.push(vt);
                    }
                }
            }
        } else if let Some(rest) = l.strip_prefix("@entry") {
            if let Some(name) = rest.trim().split_whitespace().next() {
                if let Some(e) = entry_by_name(name) {
                    if !d.entries.contains(&e) {
                        d.entries.push(e);
                    }
                }
            }
        } else if l.starts_with("@compute_shader") {
            d.compute = true;
        }
    }
    if d.entries.is_empty() {
        d.entries.push(0); // default
    }
    d
}

/// Per-stage compiled variants for a raw shader.
#[derive(Default)]
pub struct RawShader {
    pub vertex: Vec<Variant>,
    pub pixel: Vec<Variant>,
    pub compute: Vec<Variant>,
    /// Entry points that produced at least one variant (for the flag word if
    /// the caller wants it).
    pub entries_hit: Vec<usize>,
}

impl RawShader {
    pub fn is_empty(&self) -> bool {
        self.vertex.is_empty() && self.pixel.is_empty() && self.compute.is_empty()
    }
}

/// Compile every variant of a raw `.hlsl` shader `base` for `platform`.
pub fn compile_raw(
    sc: &ShaderCompiler,
    base: &str,
    directives: &Directives,
    platform: Platform,
) -> Result<RawShader, String> {
    let mut out = RawShader::default();
    for &entry in &directives.entries {
        let mut hit = false;
        for &vt in &directives.generates {
            // Vertex, pixel, and (if declared) compute.
            let mut stages = vec![Stage::Vertex, Stage::Pixel];
            if directives.compute {
                stages.push(Stage::Compute);
            }
            for stage in stages {
                match sc.compile_variant(base, stage, entry, vt, 0, platform, &[])? {
                    CompileOutcome::Compiled(o) => {
                        let splut = match platform {
                            Platform::Durango => super::emit::Splut {
                                dx9: None,
                                durango: Some(o),
                                gprs: 0,
                            },
                            _ => super::emit::Splut {
                                dx9: Some(o),
                                durango: None,
                                gprs: 0,
                            },
                        };
                        let variant = Variant { entry, vertex_type: vt, splut };
                        match stage {
                            Stage::Vertex => out.vertex.push(variant),
                            Stage::Pixel => out.pixel.push(variant),
                            Stage::Compute => out.compute.push(variant),
                        }
                        hit = true;
                    }
                    CompileOutcome::EntryNotFound => {}
                }
            }
        }
        if hit {
            out.entries_hit.push(entry);
        }
    }
    Ok(out)
}
