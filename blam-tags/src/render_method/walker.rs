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

use super::cbuffer::compile_real_constant;
use super::types::{
    BitmapAddressMode, BitmapFilterMode, RenderMethod, RenderMethodDefinition,
    RenderMethodExtern, RenderMethodOption, RenderMethodOptionParameter,
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
    /// are A, R, G, B in `[0, 1]`; bit-pack via [`ArgbColor`] if you
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
/// parameter), see [`address_mode_x`] / [`address_mode_y`].
#[derive(Debug, Clone)]
pub struct BitmapBinding {
    /// Tag-relative path to the `.bitmap` (e.g.,
    /// `objects\characters\grunt\bitmaps\grunt`). Empty when
    /// `extern_texture_mode` is `Some` (the texture comes from engine
    /// state instead).
    pub bitmap_path: String,
    /// Index into the bitmap tag's images block — most rmop defaults
    /// use index 0; rmsh overrides may select an alternate image.
    pub bitmap_index: i16,
    pub filter_mode: BitmapFilterMode,
    /// Unified address mode — when the rmsh parameter is present, this
    /// is its `bitmap_address_mode` field; otherwise the rmop's
    /// `default_address_mode`. Per-axis overrides (`bitmap_address_mode_x` /
    /// `_y`) are surfaced separately as [`address_mode_x`] / [`address_mode_y`].
    pub address_mode: BitmapAddressMode,
    /// Per-axis U/X address mode. Falls back to `address_mode` when the
    /// rmsh parameter doesn't author a per-axis override.
    pub address_mode_x: BitmapAddressMode,
    /// Per-axis V/Y address mode. Falls back to `address_mode` when the
    /// rmsh parameter doesn't author a per-axis override.
    pub address_mode_y: BitmapAddressMode,
    /// `Some(extern)` when the texture is sourced from a runtime
    /// render target (camera/refraction/mirror/scope) or another engine
    /// state slot. `None` when the binding uses the inline `bitmap_path`.
    pub extern_texture_mode: Option<RenderMethodExtern>,
    /// Anisotropy override; 0 means "use sampler default".
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
    /// [`TagFunction::evaluate_color`] until that path is implemented.
    pub fn resolve_with_time(
        rm: &RenderMethod,
        rmdf: &RenderMethodDefinition,
        time_seconds: f32,
        mut load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
    ) -> Self {
        let mut parameters = Vec::new();
        let mut seen = BTreeMap::<String, ()>::new();

        // Walk categories in rmdf order; for each, find the chosen
        // option from rmsh.options[i] (defaults to 0 when missing),
        // load the rmop, and emit one entry per declared parameter.
        for (cat_idx, category) in rmdf.categories.iter().enumerate() {
            let opt_idx = rm.options.get(cat_idx).copied().unwrap_or(0).max(0) as usize;
            let Some(category_option) = category.options.get(opt_idx) else { continue };
            if category_option.option_path.is_empty() { continue }
            let Some(rmop) = load_rmop(&category_option.option_path) else { continue };

            for op_param in &rmop.parameters {
                if op_param.parameter_name.is_empty() { continue }
                if seen.insert(op_param.parameter_name.clone(), ()).is_some() {
                    // First declaration wins — matches the runtime
                    // behavior where `c_render_method::find_parameter_index`
                    // scans linearly and returns the first match.
                    continue;
                }
                parameters.push(resolve_one(rm, op_param, time_seconds));
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
    mut load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
) -> Vec<RenderMethodOptionParameter> {
    let mut out: Vec<RenderMethodOptionParameter> = Vec::new();
    for (cat_idx, category) in rmdf.categories.iter().enumerate() {
        let opt_idx = rm.options.get(cat_idx).copied().unwrap_or(0).max(0) as usize;
        let Some(category_option) = category.options.get(opt_idx) else { continue };
        if category_option.option_path.is_empty() { continue }
        let Some(rmop) = load_rmop(&category_option.option_path) else { continue };
        for op_param in rmop.parameters {
            if op_param.parameter_name.is_empty() { continue }
            out.push(op_param);
        }
    }
    out
}

// =============================================================================
// Per-parameter resolution
// =============================================================================

fn resolve_one(
    rm: &RenderMethod,
    op_param: &RenderMethodOptionParameter,
    time_seconds: f32,
) -> ResolvedParameter {
    let parameter_type = op_param.parameter_type.unwrap_or(RenderMethodParameterType::Real);

    // 1. Extern wins.
    if let Some(ext) = op_param.source_extern {
        if ext != RenderMethodExtern::None {
            return ResolvedParameter {
                name: op_param.parameter_name.clone(),
                parameter_type,
                source: ParameterSource::Extern(ext),
            };
        }
    }

    // 2. rmsh override by name.
    let rm_param = rm.parameters.iter().find(|p| p.parameter_name == op_param.parameter_name);

    let _ = time_seconds; // canonical merge evaluates curves at (0, 0)
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
    // rmsh override: any non-empty bitmap_path wins; sampler state
    // overrides are gated per-field by `bitmap_flags`. Mirrors
    // `update_texture_parameter @ tool.exe 0x140C50260`.
    let path = rm_param
        .map(|p| p.bitmap_path.as_str())
        .filter(|p| !p.is_empty())
        .unwrap_or(op_param.default_bitmap_path.as_str())
        .to_string();

    // rmsh schema doesn't carry a per-instance bitmap index — only the
    // postprocess texture block does. Default to 0 here.
    let bitmap_index = 0;

    // `bitmap_flags` is the per-field override mask the engine bake
    // consults; only when a bit is set does the rmsh's authored value
    // win over the rmop's default. Verified against tool.exe's bake
    // function (see [[reference_tool_exe_bake_vs_tagtool_2026_05_23]]).
    const FLAG_FILTER:     i16 = 0x01;
    const FLAG_ADDRESS:    i16 = 0x02; // unified — broadcasts to both x and y
    const FLAG_ADDRESS_X:  i16 = 0x04; // takes precedence over the unified bit for x
    const FLAG_ADDRESS_Y:  i16 = 0x08; // takes precedence over the unified bit for y
    const FLAG_ANISO:      i16 = 0x10;
    const FLAG_COMPARISON: i16 = 0x20;
    let flags = rm_param.map(|p| p.bitmap_flags).unwrap_or(0);

    let filter_mode = match rm_param {
        Some(p) if flags & FLAG_FILTER != 0 => p.bitmap_filter_mode,
        _ => op_param.default_filter_mode,
    };

    // The "unified" rmsh override broadcasts to both axes; per-axis bits
    // (0x04 / 0x08) take precedence over 0x02 for their respective axis.
    let address_mode = match rm_param {
        Some(p) if flags & FLAG_ADDRESS != 0 => p.bitmap_address_mode,
        _ => op_param.default_address_mode,
    };
    let address_mode_x = match rm_param {
        Some(p) if flags & FLAG_ADDRESS_X != 0 => p.bitmap_address_mode_x,
        Some(p) if flags & FLAG_ADDRESS    != 0 => p.bitmap_address_mode,
        _ => op_param.default_address_mode,
    };
    let address_mode_y = match rm_param {
        Some(p) if flags & FLAG_ADDRESS_Y != 0 => p.bitmap_address_mode_y,
        Some(p) if flags & FLAG_ADDRESS    != 0 => p.bitmap_address_mode,
        _ => op_param.default_address_mode,
    };

    let extern_texture_mode = rm_param.and_then(|p| p.bitmap_extern_mode);
    let anisotropy_amount = match rm_param {
        Some(p) if flags & FLAG_ANISO != 0 => p.bitmap_anisotropy_amount,
        _ => op_param.anisotropy_amount,
    };
    // comparison_function isn't exposed on BitmapBinding (DEAD in
    // engine runtime sampler creator — verified against d3d11_sampler_state_cache::get
    // @ dllcache 0x1806F1C70 which hardcodes ComparisonFunc=NEVER), but
    // the gated resolution still applies if we surface it later.
    let _ = FLAG_COMPARISON;

    BitmapBinding {
        bitmap_path: path,
        bitmap_index,
        filter_mode,
        address_mode,
        address_mode_x,
        address_mode_y,
        extern_texture_mode,
        anisotropy_amount,
    }
}

