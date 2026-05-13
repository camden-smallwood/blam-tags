//! Cache-build replay: synthesize the per-rmsh `constants[]` array
//! that the runtime's `c_render_method_data::build_postprocess` would
//! pack into `s_render_method_postprocess_definition.real_constants`.
//!
//! ## Background
//!
//! The runtime cbuffer (`cb13` in pixl shader DXBC) is sourced from
//! `postprocess.real_constants[N]` at draw time — see
//! `submit_static_ps_parameters @ 0x180685860`:
//!   ```c
//!   *(_OWORD *)&buf[reg] = postprocess.real_constants[routing.source_index];
//!   ```
//! `update_constants @ 0x180685300` then OVERLAYS animated values from
//! `postprocess.overlays[routing.overlay_index]` onto specific channels.
//!
//! In the **CACHE format** of the rmsh both `real_constants` and
//! `overlays` are populated by the offline cache-build pipeline. In the
//! **TAG format** (what we load from `.shader` files) those blocks are
//! empty — the values live in:
//!  - `rmsh.parameters[i].real_parameter` / `animated_parameters` — per-instance
//!  - rmop defaults — per-option
//!
//! The cache-builder reads those, packs one vec4 per
//! `rmt2.float_constants[i]` slot name, and writes them out. This module
//! does the same thing at material-load time so a forward renderer can
//! upload a faithful per-material cbuffer without needing a built cache.
//!
//! ## Output
//!
//! [`ResolvedCbuffer`] has one slot per `rmt2.float_constants[i]` name.
//! Each slot is a `vec4<f32>`; the channel layout follows the runtime's
//! per-animated-parameter-type mapping (see `update_constants`):
//!  - Type 0 (Value)         → `.x`
//!  - Type 1 (Color)         → `.xyz`
//!  - Type 2 (ScaleUniform)  → `.x` AND `.y`
//!  - Type 3 (ScaleX)        → `.x`
//!  - Type 4 (ScaleY)        → `.y`
//!  - Type 5 (TranslationX)  → `.z`
//!  - Type 6 (TranslationY)  → `.w`
//!
//! Bitmap-typed parameters (`*_map`) implicitly carry an xform; with no
//! animated entries they default to identity `(1, 1, 0, 0)`.

use crate::tag_function::TagFunction;

use super::types::{
    EntryPoint, RenderMethod, RenderMethodAnimatedParameterType, RenderMethodOptionParameter,
    RenderMethodParameter, RenderMethodParameterType, RenderMethodTemplate, TagBlockIndex,
};
use crate::math::ArgbColor;

/// One cbuffer slot mirroring `rmt2.float_constants[i]`.
#[derive(Debug, Clone)]
pub struct CbufferSlot {
    /// `rmt2.float_constants[i]` — the parameter name (e.g. `base_map`,
    /// `diffuse_coefficient`).
    pub source_name: String,
    /// Whether this slot carries a texture xform (.xy = scale, .zw =
    /// translation). True for any slot whose name resolves to a Bitmap
    /// parameter in the rmsh/rmop chain. Drives WGSL field naming
    /// (`<source_name>_xform` vs just `<source_name>`).
    pub is_xform: bool,
    /// Byte offset within the cbuffer (`i * 16`).
    pub byte_offset: u32,
    /// Resolved value as 4 f32s.
    pub value: [f32; 4],
}

/// The rmt2-aligned cbuffer for one material: slot table + pre-packed
/// upload bytes. `slots[i]` and `bytes[i*16..(i+1)*16]` are 1:1.
#[derive(Debug, Clone, Default)]
pub struct ResolvedCbuffer {
    pub slots: Vec<CbufferSlot>,
    pub total_bytes: u32,
    pub bytes: Vec<u8>,
}

impl ResolvedCbuffer {
    pub fn find(&self, source_name: &str) -> Option<&CbufferSlot> {
        self.slots.iter().find(|s| s.source_name == source_name)
    }
}

/// Synthesize the `constants[]` array for a `(rmsh, rmt2, rmop_chain)`
/// triple — the cache-build replay described in the module docs.
///
/// Iterates `rmt2.float_constants[]` in slot order. For each name:
///   1. Look up the matching `op_param` in the rmop chain (Stage 1: rmop
///      default sets the slot value).
///   2. Look up the matching `rmsh.parameters[]` entry (Stage 2: rmsh
///      override conditionally edits the slot per parameter type).
///
/// This mirrors `c_render_method::compile_single_real_constant
/// @ 0x826E42E8` (Reach tag-debug) — the canonical cache-builder bake.
/// `rmop_params` is the flat rmop chain parameter list, equivalent to
/// `c_render_method_definition::build_parameter_list @ 0x826E31D8`.
///
/// Names not found in either rmop chain or rmsh fall back to the
/// extern/multiplier defaults (engine-bound at runtime).
///
/// Animated parameters are evaluated at `eval_time = 0.0` — the static
/// load-time bake. Use [`rebuild_cbuffer_bytes_at_time`] to re-evaluate
/// at a per-frame `current_time` for animated rmsh tags.
pub fn resolve_pixel_user_cbuffer(
    rmsh: &RenderMethod,
    rmt2: &RenderMethodTemplate,
    rmop_params: &[RenderMethodOptionParameter],
) -> ResolvedCbuffer {
    resolve_pixel_user_cbuffer_at_time(rmsh, rmt2, rmop_params, 0.0)
}

/// Time-aware variant — evaluates animated functions at `eval_time`
/// instead of 0. Engine equivalent at runtime: `update_constants @
/// 0x180685300` overlays animated values from `postprocess.overlays[]`
/// per frame. The overlays are pre-baked at startup; we re-evaluate
/// on the fly (cheaper than materializing 256-step tables, and we can
/// use float time directly).
///
/// Engine layout vs ours: the engine writes per `routing_info[N]`
/// where `dest_register = (destination_index & 0xFF)` may reorder
/// `float_constants[i]` to non-sequential registers. We use a simpler
/// sequential `byte_offset = i * 16` layout because our generated
/// WGSL declares its struct fields in `float_constants` order — so
/// our cbuffer layout matches our WGSL field offsets, even though it
/// doesn't match the engine's offline-DXBC register allocation.
/// Result is identical, the route is just different.
pub fn resolve_pixel_user_cbuffer_at_time(
    rmsh: &RenderMethod,
    rmt2: &RenderMethodTemplate,
    rmop_params: &[RenderMethodOptionParameter],
    eval_time: f32,
) -> ResolvedCbuffer {
    let names = &rmt2.float_constants;
    let n = names.len() as u32;
    let total_bytes = (n * 16).max(16);
    let mut slots = Vec::with_capacity(names.len());
    let mut bytes = vec![0u8; total_bytes as usize];

    for (i, name) in names.iter().enumerate() {
        let op_param = rmop_params.iter().find(|p| p.parameter_name == *name);
        let rmsh_param = rmsh.parameters.iter().find(|p| p.parameter_name == *name);

        let (value, is_xform) = match op_param {
            Some(op) => compile_real_constant_at_time(op, rmsh_param, eval_time),
            None => (default_for_unknown(name), name_is_xform(name)),
        };

        let byte_offset = (i as u32) * 16;
        let off = byte_offset as usize;
        for (c, v) in value.iter().enumerate() {
            bytes[off + c * 4..off + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }

        slots.push(CbufferSlot {
            source_name: name.clone(),
            is_xform,
            byte_offset,
            value,
        });
    }

    ResolvedCbuffer { slots, total_bytes, bytes }
}

/// Just the bytes — useful when a renderer caches the slot table from
/// a static `resolve_pixel_user_cbuffer` and only needs the per-frame
/// upload buffer. Walks the same path as `resolve_pixel_user_cbuffer_at_time`.
pub fn rebuild_cbuffer_bytes_at_time(
    rmsh: &RenderMethod,
    rmt2: &RenderMethodTemplate,
    rmop_params: &[RenderMethodOptionParameter],
    eval_time: f32,
) -> Vec<u8> {
    resolve_pixel_user_cbuffer_at_time(rmsh, rmt2, rmop_params, eval_time).bytes
}

/// Time-aware re-resolve that handles both load-time paths:
/// - `Some(rmt2)` → walks `rmt2.float_constants[i]` order (the engine's
///   cache-built path, same as `resolve_pixel_user_cbuffer_at_time`).
/// - `None` → walks `rmop_params[i]` order (author-format tags with
///   empty `rmsh.postprocess_definition` — e.g. vahalla_waterfall).
///
/// Caller must use the SAME path here that was used at load time so
/// the produced bytes match the layout that `CbufferLayout` was built
/// against. `MaterialData::cbuffer.bytes` was produced by whichever
/// path applied; pass `source.rmt2.as_ref()` to mirror it.
pub fn rebuild_cbuffer_bytes_with_optional_rmt2(
    rmsh: &RenderMethod,
    rmt2: Option<&RenderMethodTemplate>,
    rmop_params: &[RenderMethodOptionParameter],
    eval_time: f32,
) -> Vec<u8> {
    match rmt2 {
        Some(rmt2) => resolve_pixel_user_cbuffer_at_time(rmsh, rmt2, rmop_params, eval_time).bytes,
        None => {
            // Mirror of `loader.rs::shader_from_render_method` rmop
            // fallback (the `cbuffer_from_rmt2.unwrap_or_else(...)`
            // arm). Walks rmop_params source order, evaluates each
            // animated function at `eval_time`.
            let mut bytes = vec![0u8; rmop_params.len() * 16];
            for (i, op) in rmop_params.iter().enumerate() {
                let rmsh_param = rmsh
                    .parameters
                    .iter()
                    .find(|p| p.parameter_name == op.parameter_name);
                let (value, _is_xform) =
                    compile_real_constant_at_time(op, rmsh_param, eval_time);
                let off = i * 16;
                for (c, v) in value.iter().enumerate() {
                    bytes[off + c * 4..off + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
            bytes
        }
    }
}

/// Canonical per-slot merge — mirror of
/// `c_render_method::compile_single_real_constant @ 0x826E42E8` from
/// Reach tag-debug XEX. Reproduces the cache-builder's two-stage bake.
/// Animated functions evaluate at `(input, range) = (0, 0)` — what the
/// engine does at load time for the static cache. Use
/// [`compile_real_constant_at_time`] for the per-frame runtime overlay.
pub fn compile_real_constant(
    op_param: &RenderMethodOptionParameter,
    rmsh_param: Option<&RenderMethodParameter>,
) -> ([f32; 4], bool) {
    compile_real_constant_at_time(op_param, rmsh_param, 0.0)
}

/// Time-aware variant — animated functions evaluate at `eval_time`
/// instead of 0, with cyclic-input handling per the function's
/// `time_period_in_seconds` (input = (t mod period) / period when
/// period > 0; raw t otherwise).
pub fn compile_real_constant_at_time(
    op_param: &RenderMethodOptionParameter,
    rmsh_param: Option<&RenderMethodParameter>,
    eval_time: f32,
) -> ([f32; 4], bool) {
    use RenderMethodAnimatedParameterType as A;
    use RenderMethodParameterType as P;

    let pt = op_param.parameter_type;
    let is_xform = matches!(pt, Some(P::Bitmap));

    // ---- Stage 1: rmop default ----
    let mut slot: [f32; 4] = match pt {
        Some(P::Bitmap) => [1.0, 1.0, 0.0, 0.0],
        Some(P::Color) => {
            let mut c = argb_u32_to_rgba(op_param.default_color);
            c[3] = 1.0; // type Color forces alpha=1
            c
        }
        Some(P::ArgbColor) => argb_u32_to_rgba(op_param.default_color),
        Some(P::Real) => [op_param.default_real_value; 4],
        Some(P::Int | P::Bool) => [op_param.default_int_bool_value as f32; 4],
        None => [0.0; 4],
    };

    // ---- Stage 2: rmsh override ----
    let Some(rm) = rmsh_param else { return (slot, is_xform) };

    match pt {
        Some(P::Bitmap) => {
            // Per-channel write based on each animated_parameter's type.
            for anim in &rm.animated_parameters {
                let v = anim.function.as_ref()
                    .map(|f| eval_value_at(f, anim.time_period_in_seconds, eval_time))
                    .unwrap_or(0.0);
                match anim.parameter_type {
                    Some(A::ScaleUniform) => { slot[0] = v; slot[1] = v; }
                    Some(A::ScaleX) => slot[0] = v,
                    Some(A::ScaleY) => slot[1] = v,
                    Some(A::TranslationX) => slot[2] = v,
                    Some(A::TranslationY) => slot[3] = v,
                    _ => {}
                }
            }
        }
        Some(P::Real) => {
            // Engine: starts with rmsh.m_real_parameter (even if 0),
            // then animated_parameters[Value] overrides via broadcast.
            slot = [rm.real_parameter; 4];
            for anim in &rm.animated_parameters {
                if matches!(anim.parameter_type, Some(A::Value)) {
                    let v = anim.function.as_ref()
                        .map(|f| eval_value_at(f, anim.time_period_in_seconds, eval_time))
                        .unwrap_or(0.0);
                    slot = [v; 4];
                }
            }
        }
        Some(P::Int | P::Bool) => {
            slot = [rm.int_parameter as f32; 4];
        }
        Some(P::Color | P::ArgbColor) => {
            // Color: animated[Color] writes RGB, animated[Alpha] writes .w.
            // No animated entries → rmop default survives Stage 1.
            for anim in &rm.animated_parameters {
                match anim.parameter_type {
                    Some(A::Color) => {
                        if let Some(c) = anim.function.as_ref().and_then(extract_first_color) {
                            slot[0] = c[0]; slot[1] = c[1]; slot[2] = c[2];
                            // alpha kept from Stage 1 / earlier anim
                        }
                    }
                    Some(A::Alpha) => {
                        let v = anim.function.as_ref()
                            .map(|f| eval_value_at(f, anim.time_period_in_seconds, eval_time))
                            .unwrap_or(0.0);
                        slot[3] = v;
                    }
                    _ => {}
                }
            }
        }
        None => {}
    }

    (slot, is_xform)
}

fn argb_u32_to_rgba(c: ArgbColor) -> [f32; 4] {
    let v = c.0;
    let a = ((v >> 24) & 0xff) as f32 / 255.0;
    let r = ((v >> 16) & 0xff) as f32 / 255.0;
    let g = ((v >> 8) & 0xff) as f32 / 255.0;
    let b = (v & 0xff) as f32 / 255.0;
    [r, g, b, a]
}

/// Names ending in `_map` are bitmap xforms by convention — give them
/// identity (1,1,0,0) when the rmsh has no matching parameter (engine
/// extern path, fills the value at draw time).
fn name_is_xform(name: &str) -> bool {
    name.ends_with("_map")
}

fn default_for_unknown(name: &str) -> [f32; 4] {
    if name_is_xform(name) {
        [1.0, 1.0, 0.0, 0.0]
    } else if name_is_multiplier(name) {
        // Tints, coefficients, contributions etc. default to 1.0 so
        // they're a no-op when the rmsh doesn't override them. With
        // a 0.0 default they'd zero out the term they multiply.
        [1.0, 0.0, 0.0, 0.0]
    } else {
        [0.0; 4]
    }
}

fn name_is_multiplier(name: &str) -> bool {
    name == "global_albedo_tint"
        || name.starts_with("diffuse_coefficient")
        || name.starts_with("analytical_specular_contribution")
        || name.starts_with("area_specular_contribution")
        || name.starts_with("environment_specular_contribution")
        || name.starts_with("environment_map_specular_contribution")
}

fn eval_value(f: &TagFunction) -> f32 {
    f.evaluate(0.0, 0.0)
}

/// Time-aware scalar eval — feeds the function input from
/// `(time_period, eval_time)`. If `time_period > 0`, input is the
/// normalized phase `(t mod period) / period` (cyclic). Otherwise the
/// input is `eval_time` directly (engine path for non-cyclic
/// time-driven params).
fn eval_value_at(f: &TagFunction, time_period: f32, eval_time: f32) -> f32 {
    let input = if time_period > 0.0 {
        (eval_time.rem_euclid(time_period)) / time_period
    } else {
        eval_time
    };
    f.evaluate(input, 0.0)
}

fn extract_first_color(f: &TagFunction) -> Option<[f32; 4]> {
    use crate::math::ArgbColor;
    use crate::tag_function::ColorGraphType;
    if f.color_graph_type() == ColorGraphType::Scalar {
        return None;
    }
    let packed = ArgbColor(f.header().colors[0]);
    let v = packed.0;
    Some([
        ((v >> 16) & 0xff) as f32 / 255.0,
        ((v >> 8) & 0xff) as f32 / 255.0,
        (v & 0xff) as f32 / 255.0,
        ((v >> 24) & 0xff) as f32 / 255.0,
    ])
}

// =============================================================================
// Cbuffer animation detection (P6.2)
// =============================================================================

/// Returns `true` if any of the rmsh's `animated_parameters` carries a
/// non-constant TagFunction — i.e., the cbuffer has a slot whose value
/// changes over the input domain.
///
/// Engine equivalent: `c_render_method::has_animated_parameters` (not
/// directly named in dllcache; engine drives per-frame
/// `update_constants @ 0x180685300` calls only for materials that
/// declared `m_overlays` non-empty). The engine walks the overlays at
/// tag-build time, dropping constant entries — so a non-empty
/// `animated_parameters` with at least one non-constant function is the
/// authoritative "this material animates" signal.
///
/// Previously this used an empirical `t=0` vs `t=1` byte comparison,
/// which silently false-negatived periodic TagFunctions where both
/// endpoints happen to evaluate equal (e.g. `sin(2πt)` at integer t).
/// The structural check below catches those.
///
/// `rmt2` and `rmop_params` are kept in the signature for API
/// continuity (callers pass them already-resolved) even though the
/// current check only consults `rmsh.animated_parameters`. If a future
/// audit shows the engine also pulls overlays from rmt2-side state,
/// extend here.
///
/// O(N) in animated_parameters count (small — typically 0..10). Called
/// once at material load; result cached on `MaterialData::is_animated`
/// on the protomorph side.
pub fn is_cbuffer_animated(
    rmsh: &RenderMethod,
    _rmt2: Option<&RenderMethodTemplate>,
    _rmop_params: &[RenderMethodOptionParameter],
) -> bool {
    rmsh.parameters.iter().any(|p| {
        p.animated_parameters.iter().any(|anim| match &anim.function {
            Some(f) => !f.is_constant(),
            None => false,
        })
    })
}

// =============================================================================
// Engine-faithful cb13 packer (P6.1)
//
// Mirrors `submit_static_ps_parameters @ 0x180685860` (dllcache,
// audit-verified 2026-05-13). Differs from
// `resolve_pixel_user_cbuffer_at_time` in two ways:
//   1. Bytes are keyed by `destination_index & 0xFF` (the
//      offline-DXBC-allocated register), NOT by source-index order.
//   2. Buffer size comes from `pass.pixel_parameters_size * 16`
//      (the engine's total cb13 size, not the source-name count).
//
// Engine cbuffer pool slots:
//   0x6E (110) = _ParametersVS  (cb13 vertex)
//   0x6F (111) = _ParametersPS  (cb13 pixel)
//
// Both ports below assert `(destination_index >> 8) == expected_pool_slot`
// to catch schema-version drift and corrupted routing tables.
// =============================================================================

const PARAMETERS_PS_POOL: u8 = 0x6F;
const PARAMETERS_VS_POOL: u8 = 0x6E;

/// Engine-faithful pixel cb13 packer. Walks the entry-point's pass's
/// `pixel_real_constants` routing range and lays out each source value
/// at the byte offset its `destination_index` decodes to.
///
/// Returns `None` if the rmt2 doesn't support the requested entry point
/// (the engine path equivalent is a no-op submit).
pub fn pack_pixel_cbuffer_at_time(
    rmsh: &RenderMethod,
    rmt2: &RenderMethodTemplate,
    rmop_params: &[RenderMethodOptionParameter],
    entry_point: EntryPoint,
    eval_time: f32,
) -> Option<ResolvedCbuffer> {
    pack_cbuffer_at_time(
        rmsh,
        rmt2,
        rmop_params,
        entry_point,
        eval_time,
        Stage::Pixel,
    )
}

/// Engine-faithful vertex cb13 packer. Same shape as
/// [`pack_pixel_cbuffer_at_time`] but routes the
/// `vertex_real_constants` slot and asserts cbuffer pool 0x6E
/// (_ParametersVS).
pub fn pack_vertex_cbuffer_at_time(
    rmsh: &RenderMethod,
    rmt2: &RenderMethodTemplate,
    rmop_params: &[RenderMethodOptionParameter],
    entry_point: EntryPoint,
    eval_time: f32,
) -> Option<ResolvedCbuffer> {
    pack_cbuffer_at_time(
        rmsh,
        rmt2,
        rmop_params,
        entry_point,
        eval_time,
        Stage::Vertex,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Pixel,
    Vertex,
}

fn pack_cbuffer_at_time(
    rmsh: &RenderMethod,
    rmt2: &RenderMethodTemplate,
    rmop_params: &[RenderMethodOptionParameter],
    entry_point: EntryPoint,
    eval_time: f32,
    stage: Stage,
) -> Option<ResolvedCbuffer> {
    // Look up the entry_point → pass mapping. `entry_points[ep]` is a
    // TagBlockIndex; `.start()` is the pass index. `.count() == 0` means
    // the entry point isn't supported by this rmt2.
    let entry_block = rmt2.entry_points.get(entry_point as usize)?;
    if entry_block.count() == 0 {
        return None;
    }
    let pass = rmt2.passes.get(entry_block.start() as usize)?;

    let (routing_block, parameters_size, expected_pool): (TagBlockIndex, u16, u8) = match stage {
        Stage::Pixel => (
            pass.pixel_real_constants,
            pass.pixel_parameters_size,
            PARAMETERS_PS_POOL,
        ),
        Stage::Vertex => (
            pass.vertex_real_constants,
            pass.vertex_parameters_size,
            PARAMETERS_VS_POOL,
        ),
    };

    // wgpu requires uniform buffers ≥ 16 bytes — clamp empty cbuffers.
    let total_bytes = (parameters_size as u32 * 16).max(16);
    let mut bytes = vec![0u8; total_bytes as usize];
    let mut slots = Vec::new();

    let routing_range = routing_block.range();
    let routing_entries = rmt2
        .routing_info
        .get(routing_range)
        .unwrap_or(&[]);

    for entry in routing_entries {
        let dest_high = (entry.destination_index >> 8) as u8;
        debug_assert_eq!(
            dest_high, expected_pool,
            "cb13 {:?} routing dest_high should be 0x{:02X} (pool {}), got 0x{:02X} \
             (source {}, dest 0x{:04X}) — likely schema-version drift or corrupted routing",
            stage,
            expected_pool,
            match stage {
                Stage::Pixel => "_ParametersPS",
                Stage::Vertex => "_ParametersVS",
            },
            dest_high,
            entry.source_index,
            entry.destination_index,
        );

        let entry_idx = (entry.destination_index & 0xFF) as u32;
        let byte_offset = entry_idx * 16;
        if byte_offset as usize + 16 > bytes.len() {
            // Routing entry exceeds the declared parameters_size — this
            // is malformed cache data; skip silently rather than
            // out-of-bounds write. Engine would assert here.
            continue;
        }

        let source_index = entry.source_index as usize;
        let name = match rmt2.float_constants.get(source_index) {
            Some(n) => n.clone(),
            None => continue,
        };
        let op_param = rmop_params.iter().find(|p| p.parameter_name == name);
        let rmsh_param = rmsh.parameters.iter().find(|p| p.parameter_name == name);

        let (value, is_xform) = match op_param {
            Some(op) => compile_real_constant_at_time(op, rmsh_param, eval_time),
            None => (default_for_unknown(&name), name_is_xform(&name)),
        };

        let off = byte_offset as usize;
        for (c, v) in value.iter().enumerate() {
            bytes[off + c * 4..off + c * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }

        slots.push(CbufferSlot {
            source_name: name,
            is_xform,
            byte_offset,
            value,
        });
    }

    Some(ResolvedCbuffer {
        slots,
        total_bytes,
        bytes,
    })
}
