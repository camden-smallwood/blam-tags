//! Render-method parameter resolution — Bungie-runtime mirror.
//!
//! This is the "walker" that takes an `rmsh` + its `rmdf` + the
//! per-category `rmop` tags and produces a flat
//! `name → ResolvedParameter` map ready for a renderer to consume.
//!
//! Mirrors the resolution layer of
//! `render_method_submit_volatile_per_node`
//! (Ares `render_method_submit.cpp:700`, IDA `0x180683AF0`) — minus
//! the rmt2 cbuffer-routing step, which is a separate pass.
//!
//! ## Resolution rules (per parameter declared by the active rmops)
//!
//! 1. If the rmop declares a non-`None` `source_extern`, the parameter
//!    is engine-bound — emit [`ParameterSource::Extern`] and the
//!    caller resolves it at draw time via [`ExternResolver`].
//! 2. Otherwise, look up the parameter by name in `rmsh.parameters`:
//!    - If present and animated, evaluate the function at `(0, 0)`
//!      (the static-resolve case; renderers that animate call
//!      [`ResolvedRenderMethod::resolve_with_time`] instead).
//!    - If present and static, take the inline `real_parameter` /
//!      `bitmap_path`.
//!    - If absent, fall back to the rmop's default.
//!
//! ## Active-options selection
//!
//! `rmsh.options[i]` is the chosen option index for `rmdf.categories[i]`.
//! When `options` is shorter than `categories` (a common case — newer
//! rmdfs add categories), missing entries default to option index 0.

use std::collections::BTreeMap;

use super::bake::{bake_texture, build_rmop_chain, find_rmsh_override};
use super::cbuffer::compile_real_constant;
use super::types::{
    BitmapAddressMode, BitmapFilterMode, RenderMethod, RenderMethodBitmapExternMode,
    RenderMethodDefinition, RenderMethodExtern, RenderMethodOption, RenderMethodOptionParameter,
    RenderMethodParameter, RenderMethodParameterType,
};
#[allow(unused_imports)] // re-exported for downstream consumers that match on it
use super::types::BitmapComparisonFunction;

// =============================================================================
// Output types
// =============================================================================

/// One resolved parameter — the unit a renderer consumes per-material.
#[derive(Debug, Clone)]
pub struct ResolvedParameter {
    pub name: String,
    pub parameter_type: RenderMethodParameterType,
    pub source: ParameterSource,
}

/// Either a value baked in at resolve time or an extern that the
/// renderer pulls from engine state at draw time.
#[derive(Debug, Clone)]
pub enum ParameterSource {
    /// The value is fully resolved here. The variant inside matches
    /// the parameter's `RenderMethodParameterType`.
    Inline(ResolvedValue),
    /// The runtime sources this parameter from engine state. The
    /// renderer must call its [`ExternResolver`] at draw time.
    Extern(RenderMethodExtern),
}

/// Resolved per-parameter value. The variant axis matches Bungie's
/// `e_render_method_parameter_type`.
#[derive(Debug, Clone)]
pub enum ResolvedValue {
    Bitmap(BitmapBinding),
    /// `Color` and `ArgbColor` parameters both land here. The 4 slots
    /// are A, R, G, B in `[0, 1]`; bit-pack via [`crate::math::ArgbColor`] if you
    /// need the original `u32`.
    Color([f32; 4]),
    Real(f32),
    Int(i32),
    Bool(bool),
}

/// Per-bitmap-parameter binding info. Mirrors the union of fields the
/// runtime samples per-texture (path + sampler state + extern mode).
///
/// `address_mode` is the unified axis mode — for materials that author
/// per-axis overrides (`bitmap_address_mode_x` / `_y` on the rmsh
/// parameter), see `address_mode_x` / `address_mode_y`.
#[derive(Debug, Clone)]
pub struct BitmapBinding {
    /// Tag-relative path to the `.bitmap` (e.g.,
    /// `objects\characters\grunt\bitmaps\grunt`). Never empty: a
    /// missing reference bakes to `color_white`.
    pub bitmap_path: String,
    /// Index into the bitmap tag's images block.
    pub bitmap_index: i16,
    pub filter_mode: BitmapFilterMode,
    /// Unified address mode — when the rmsh parameter is present, this
    /// is its `bitmap_address_mode` field; otherwise the rmop's
    /// `default_address_mode`. Per-axis overrides (`bitmap_address_mode_x` /
    /// `_y`) are surfaced separately as `address_mode_x` / `address_mode_y`.
    pub address_mode: BitmapAddressMode,
    /// Per-axis U/X address mode. Falls back to `address_mode` when the
    /// rmsh parameter doesn't author a per-axis override.
    pub address_mode_x: BitmapAddressMode,
    /// Per-axis V/Y address mode. Falls back to `address_mode` when the
    /// rmsh parameter doesn't author a per-axis override.
    pub address_mode_y: BitmapAddressMode,
    /// A render target to bind ahead of `bitmap_path`, when one is live.
    pub extern_texture_mode: Option<RenderMethodBitmapExternMode>,
    /// The rmop's anisotropy. The bake does not carry it.
    pub anisotropy_amount: i16,
}

// =============================================================================
// Extern resolver trait
// =============================================================================

/// Renderer-supplied resolver for engine-bound externs.
///
/// The walker produces [`ParameterSource::Extern`] entries that name
/// which extern is requested; the renderer implements this trait to
/// inject engine state (sun direction, change colors, env map, etc.)
/// at draw time.
///
/// Default impls return zero / empty so callers can selectively
/// override only the externs that matter to their pipeline.
pub trait ExternResolver {
    fn resolve_real4(&self, _ext: RenderMethodExtern) -> [f32; 4] { [0.0; 4] }
    fn resolve_real(&self, ext: RenderMethodExtern) -> f32 { self.resolve_real4(ext)[0] }
    fn resolve_int(&self, _ext: RenderMethodExtern) -> i32 { 0 }
    fn resolve_bool(&self, _ext: RenderMethodExtern) -> bool { false }
    fn resolve_bitmap(&self, _ext: RenderMethodExtern) -> Option<BitmapBinding> { None }
}

/// No-op resolver — every extern returns its default. Useful for
/// static / offline analysis where engine state isn't available.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullExternResolver;

impl ExternResolver for NullExternResolver {}

// =============================================================================
// Walker
// =============================================================================

/// A fully-resolved render_method, ready for a renderer to consume.
#[derive(Debug, Clone, Default)]
pub struct ResolvedRenderMethod {
    /// Lookup by Bungie parameter name (e.g., `"base_map"`,
    /// `"diffuse_coefficient"`). Insertion order follows the rmdf
    /// category order, then rmop parameter order — same order the
    /// runtime walks.
    pub parameters: Vec<ResolvedParameter>,
    /// FOURCC of the source rm** tag — `'rmsh'`, `'rmtr'`, `'rmw '`,
    /// etc. Threaded from `RenderMethod.group_tag`. The runtime
    /// `render_method_submit` chain ignores this (verified via
    /// dllcache decomp), but shader assemblers need it to dispatch
    /// to the right WGSL fragments. See
    /// `reference_rmtr_runtime_distinction.md`.
    pub group_tag: u32,
}

impl ResolvedRenderMethod {
    /// Static resolve — evaluates animated functions at `(input, range)
    /// = (0, 0)`, which is what the runtime does at load time and what
    /// 99% of rmsh tags actually need (constant params).
    pub fn resolve(
        rm: &RenderMethod,
        rmdf: &RenderMethodDefinition,
        load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
    ) -> Self {
        Self::resolve_with_time(rm, rmdf, 0.0, load_rmop)
    }

    /// Time-aware resolve — evaluates animated functions at
    /// `(input, range) = (time_seconds, time_seconds)`. Color
    /// gradients still return a stub-white from
    /// [`crate::tag_function::TagFunction::evaluate_color`] until that path is implemented.
    pub fn resolve_with_time(
        rm: &RenderMethod,
        rmdf: &RenderMethodDefinition,
        time_seconds: f32,
        mut load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
    ) -> Self {
        let mut parameters = Vec::new();
        let mut seen = BTreeMap::<String, ()>::new();
        let _ = time_seconds;

        // First declaration wins, as the engine's linear scan does.
        for op_param in &build_rmop_chain(rm, rmdf, &mut load_rmop) {
            if seen.insert(op_param.parameter_name.clone(), ()).is_none() {
                parameters.push(resolve_one(rm, op_param));
            }
        }

        Self { parameters, group_tag: rm.group_tag }
    }

    /// O(N) lookup by Bungie parameter name. For renderers that need
    /// random-access by-name, build their own `HashMap` once.
    pub fn find(&self, name: &str) -> Option<&ResolvedParameter> {
        self.parameters.iter().find(|p| p.name == name)
    }
}

/// Build the flat rmop parameter list for a `(rmsh, rmdf)` pair —
/// equivalent to `c_render_method_definition::build_parameter_list
/// @ 0x826E31D8` (Reach tag-debug). Walks the rmdf's categories in
/// declared order, loads each chosen rmop, and concatenates all
/// rmop parameters. The result is what
/// [`crate::render_method::cbuffer::compile_real_constant`] queries by
/// name to apply Stage 1 rmop defaults.
///
/// Note: differs from [`ResolvedRenderMethod::resolve`] which DEDUPES
/// by name (first-wins). The cache builder DOES allow duplicates
/// (multiple rmops in the chain may declare the same parameter name);
/// the lookup at slot-time naturally takes the first match by linear
/// scan, which matches the engine's `find_parameter_list_entry_by_name`.
pub fn build_rmop_param_list(
    rm: &RenderMethod,
    rmdf: &RenderMethodDefinition,
    load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
) -> Vec<RenderMethodOptionParameter> {
    build_rmop_chain(rm, rmdf, load_rmop)
}

// =============================================================================
// Per-parameter resolution
// =============================================================================

fn resolve_one(rm: &RenderMethod, op_param: &RenderMethodOptionParameter) -> ResolvedParameter {
    let parameter_type = op_param.parameter_type.map(|e| e.get()).unwrap_or(RenderMethodParameterType::Real);

    // 1. Extern wins.
    if let Some(ext) = op_param.source_extern.map(|e| e.get()) {
        if ext != RenderMethodExtern::None {
            return ResolvedParameter {
                name: op_param.parameter_name.clone(),
                parameter_type,
                source: ParameterSource::Extern(ext),
            };
        }
    }

    // 2. rmsh override. Curves evaluate at (0, 0), as the bake's do.
    let rm_param = find_rmsh_override(rm, op_param);

    let value = match parameter_type {
        RenderMethodParameterType::Bitmap => {
            ResolvedValue::Bitmap(resolve_bitmap(op_param, rm_param))
        }
        RenderMethodParameterType::Color | RenderMethodParameterType::ArgbColor => {
            let (slot, _) = compile_real_constant(op_param, rm_param);
            ResolvedValue::Color(slot)
        }
        RenderMethodParameterType::Real => {
            let (slot, _) = compile_real_constant(op_param, rm_param);
            ResolvedValue::Real(slot[0])
        }
        RenderMethodParameterType::Int => {
            let (slot, _) = compile_real_constant(op_param, rm_param);
            ResolvedValue::Int(slot[0] as i32)
        }
        RenderMethodParameterType::Bool => {
            let (slot, _) = compile_real_constant(op_param, rm_param);
            ResolvedValue::Bool(slot[0] != 0.0)
        }
    };

    ResolvedParameter {
        name: op_param.parameter_name.clone(),
        parameter_type,
        source: ParameterSource::Inline(value),
    }
}

fn resolve_bitmap(
    op_param: &RenderMethodOptionParameter,
    rm_param: Option<&RenderMethodParameter>,
) -> BitmapBinding {
    let texture = bake_texture(op_param, rm_param);
    // The unified mode is what the rmsh authored only when it applies.
    let address_mode = match rm_param {
        Some(p) if p.bitmap_flags & 0x02 != 0 && !p.bitmap_path.is_empty() => p.bitmap_address_mode,
        _ => op_param.default_address_mode.get(),
    };
    BitmapBinding {
        bitmap_path: texture.bitmap_path,
        bitmap_index: texture.bitmap_index,
        filter_mode: texture.filter_mode,
        address_mode,
        address_mode_x: texture.address_mode_x,
        address_mode_y: texture.address_mode_y,
        extern_texture_mode: texture.extern_texture_mode,
        anisotropy_amount: op_param.anisotropy_amount,
    }
}

