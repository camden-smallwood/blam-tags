//! Engine-faithful rmsh → postprocess bake.
//!
//! Mirrors tool.exe's `update_postprocess @ 0x140C52530` and the four
//! parameter-update sub-functions. Populates a [`RenderMethod`]'s
//! `postprocess_definition` field in-place — after a successful bake the
//! `RenderMethod` looks like it shipped pre-baked from the cache.
//!
//! See `reference_tag_to_bake_render_method_2026_05_23.md` for the full
//! call graph; this file ports each phase. Per-type Stage 1 / Stage 2
//! fill semantics live in `reference_tool_exe_bake_vs_tagtool_2026_05_23.md`.

use std::collections::HashMap;

use super::types::{
    BitmapAddressMode, BitmapComparisonFunction, BitmapFilterMode, RenderMethod,
    RenderMethodAnimatedParameter, RenderMethodDefinition, RenderMethodExtern,
    RenderMethodOption, RenderMethodOptionParameter, RenderMethodParameter,
    RenderMethodPostprocessDefinition, RenderMethodPostprocessTexture, RenderMethodTemplate,
    TagBlockIndex,
};

use super::cbuffer::{
    compile_real_constant_at_time, DefaultEvalContext, RenderMethodEvalContext,
};

/// Errors raised by [`RenderMethod::bake`].
#[derive(Debug)]
pub enum BakeError {
    /// rmsh's postprocess block exists but lacks a template path — can't
    /// resolve the rmt2 to drive the bake.
    MissingTemplate,
    /// `rmt2.bool_constants.len() > 32` would overflow the packed
    /// `bool_constants` u32. The engine has the same assert.
    TooManyBooleanConstants,
}

impl std::fmt::Display for BakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTemplate => f.write_str("postprocess template path is empty"),
            Self::TooManyBooleanConstants => {
                f.write_str("rmt2.bool_constants.len() > 32 (engine cap)")
            }
        }
    }
}

impl std::error::Error for BakeError {}

// =============================================================================
// Animated overlay split — sub_140C4F690
// =============================================================================

/// Per-rmop-parameter overlay TagBlockIndex (start: index into
/// [`RenderMethodPostprocessDefinition::overlays`], count: how many).
#[derive(Debug, Clone, Default)]
struct OverlayMap {
    /// Flat list of dynamic animated_parameters that need per-frame
    /// re-evaluation. Static (constant) animated_parameters are baked
    /// into `real_constants` Stage 2 instead.
    overlays: Vec<RenderMethodAnimatedParameter>,
    /// Indexed by rmop-chain position: (overlay_start, overlay_count)
    /// packed as a [`TagBlockIndex`] (start:10 / count:6) ready to write
    /// into a texture_constant's `texture_transform_overlay_index`.
    per_rmop_param: Vec<TagBlockIndex>,
}

impl OverlayMap {
    fn for_rmop_param(&self, idx: usize) -> TagBlockIndex {
        self.per_rmop_param
            .get(idx)
            .copied()
            .unwrap_or_default()
    }
}

/// Split an rmsh's animated_parameters into static-baked vs dynamic-overlay
/// groups. Static = `TagFunction::is_constant()` returns `true` (function
/// output is independent of input; baked into `real_constants` Stage 2).
/// Dynamic = everything else; appended to `overlays[]` and referenced
/// per-texture via `texture_transform_overlay_index`.
///
/// Mirrors `sub_140C4F690` in tool.exe.
fn split_animated_parameters(
    rmsh: &RenderMethod,
    rmop_chain: &[RenderMethodOptionParameter],
) -> OverlayMap {
    let mut overlays: Vec<RenderMethodAnimatedParameter> = Vec::new();
    let mut per_rmop_param: Vec<TagBlockIndex> = Vec::with_capacity(rmop_chain.len());

    for op in rmop_chain {
        let start = overlays.len();
        if let Some(rm_param) = rmsh.parameters.iter().find(|p| p.parameter_name == op.parameter_name) {
            for anim in &rm_param.animated_parameters {
                let is_constant = anim
                    .function
                    .as_ref()
                    .map(|f| f.is_constant())
                    .unwrap_or(true);
                if !is_constant {
                    overlays.push(anim.clone());
                }
            }
        }
        let count = overlays.len() - start;
        // Engine semantic (sub_140C4F690): when count == 0 the start field
        // is also zero. Avoids leaking the "next overlay slot" into idle
        // texture entries' texture_transform_overlay_index.
        let packed = if count == 0 {
            TagBlockIndex::default()
        } else {
            TagBlockIndex::new(start as u16, count as u16)
        };
        per_rmop_param.push(packed);
    }

    OverlayMap { overlays, per_rmop_param }
}

// =============================================================================
// rmop chain build — sub_140C4EB10
// =============================================================================

/// Concatenate every rmop's parameters in rmdf category order. Optionally
/// prepends `rmdf.global_options`. Mirrors `sub_140C4EB10`.
///
/// **Gap from engine**: `rmdf.flags & 1` marker synthesis isn't ported
/// yet — flagged in the reference doc. Add when we hit an rmdf that uses it.
fn build_rmop_chain(
    rmsh: &RenderMethod,
    rmdf: &RenderMethodDefinition,
    mut load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
) -> Vec<RenderMethodOptionParameter> {
    let mut chain: Vec<RenderMethodOptionParameter> = Vec::new();

    // Phase 1: rmdf.global_options
    if !rmdf.global_options_path.is_empty() {
        if let Some(rmop) = load_rmop(&rmdf.global_options_path) {
            for p in rmop.parameters {
                if !p.parameter_name.is_empty() {
                    chain.push(p);
                }
            }
        }
    }

    // Phase 2: per-category rmop chain
    for (cat_idx, category) in rmdf.categories.iter().enumerate() {
        let opt_idx = rmsh.options.get(cat_idx).copied().unwrap_or(0).max(0) as usize;
        let Some(category_option) = category.options.get(opt_idx) else { continue };
        if category_option.option_path.is_empty() {
            continue;
        }
        let Some(rmop) = load_rmop(&category_option.option_path) else { continue };
        for p in rmop.parameters {
            if !p.parameter_name.is_empty() {
                chain.push(p);
            }
        }
    }

    chain
}

// =============================================================================
// Texture bake — sub_140C50260
// =============================================================================

/// Find the rmsh-side parameter override for a given rmop parameter (matched
/// by name and parameter_type), if present.
fn find_rmsh_override<'a>(
    rmsh: &'a RenderMethod,
    op: &RenderMethodOptionParameter,
) -> Option<&'a RenderMethodParameter> {
    rmsh.parameters
        .iter()
        .find(|p| p.parameter_name == op.parameter_name)
}

/// Build one `texture_constant` slot. Mirrors `update_texture_parameter @
/// tool.exe 0x140C50260`. Sampler-state overrides are gated per-field by
/// `rmsh.bitmap_flags`:
///
/// | Bit | Override |
/// |---:|---|
/// | 0   | filter_mode |
/// | 1   | address_mode (unified, broadcasts to BOTH axes) |
/// | 2   | address_mode_x (precedence over bit 1 for x) |
/// | 3   | address_mode_y (precedence over bit 1 for y) |
/// | 4   | anisotropy (DEAD in runtime sampler — kept for completeness) |
/// | 5   | comparison_function (DEAD in runtime sampler) |
fn bake_texture_constant(
    rmsh: &RenderMethod,
    rmop_chain: &[RenderMethodOptionParameter],
    rmop_idx_by_name: &HashMap<&str, usize>,
    overlays: &OverlayMap,
    rmt2_real_param_names: &[String],
    texture_name: &str,
) -> RenderMethodPostprocessTexture {
    let Some(&op_idx) = rmop_idx_by_name.get(texture_name) else {
        // Texture name not found in any rmop — emit an empty slot
        // (engine emits a SHADER LINK ERROR but we degrade silently
        // since blam-tags isn't a tool-side validator).
        return RenderMethodPostprocessTexture {
            bitmap_path: String::new(),
            bitmap_index: 0,
            address_mode_x: BitmapAddressMode::Wrap,
            address_mode_y: BitmapAddressMode::Wrap,
            filter_mode: BitmapFilterMode::Trilinear,
            comparison_function: BitmapComparisonFunction::Never,
            extern_texture_mode: None,
            texture_transform_constant_index: -1,
            texture_transform_overlay_indices: TagBlockIndex::default(),
        };
    };
    let op = &rmop_chain[op_idx];
    let rm = find_rmsh_override(rmsh, op);

    // `bitmap_flags` per-field override gates (engine bits 0..5).
    const FLAG_FILTER:     i16 = 0x01;
    const FLAG_ADDRESS:    i16 = 0x02;
    const FLAG_ADDRESS_X:  i16 = 0x04;
    const FLAG_ADDRESS_Y:  i16 = 0x08;
    const FLAG_COMPARISON: i16 = 0x20;
    let flags = rm.map(|p| p.bitmap_flags).unwrap_or(0);

    // bitmap_path: rmsh wins when non-empty, else rmop default.
    let bitmap_path = rm
        .map(|p| p.bitmap_path.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(op.default_bitmap_path.as_str())
        .to_owned();

    let filter_mode = match rm {
        Some(p) if flags & FLAG_FILTER != 0 => p.bitmap_filter_mode,
        _ => op.default_filter_mode,
    };
    let address_mode_x = match rm {
        Some(p) if flags & FLAG_ADDRESS_X != 0 => p.bitmap_address_mode_x,
        Some(p) if flags & FLAG_ADDRESS    != 0 => p.bitmap_address_mode,
        _ => op.default_address_mode,
    };
    let address_mode_y = match rm {
        Some(p) if flags & FLAG_ADDRESS_Y != 0 => p.bitmap_address_mode_y,
        Some(p) if flags & FLAG_ADDRESS    != 0 => p.bitmap_address_mode,
        _ => op.default_address_mode,
    };
    let comparison_function = match rm {
        Some(p) if flags & FLAG_COMPARISON != 0 => p.bitmap_comparison_function,
        _ => op.default_comparison_function,
    };
    let extern_texture_mode = rm
        .and_then(|p| p.bitmap_extern_mode)
        .or_else(|| op.source_extern.filter(|e| !matches!(e, RenderMethodExtern::None)));

    // texture_transform_constant_index: linear search rmt2.float_constants
    // for an entry with this texture's name; matching index is the byte
    // value. 0xFF (-1) means no xform applies (e.g., cubemap parameter).
    let texture_transform_constant_index: i8 = rmt2_real_param_names
        .iter()
        .position(|n| n == texture_name)
        .and_then(|i| i8::try_from(i).ok())
        .unwrap_or(-1);

    // texture_transform_overlay_indices: per-rmop-parameter packed
    // (start, count) into the postprocess.overlays block. Filled from
    // the overlay split above.
    let texture_transform_overlay_indices = overlays.for_rmop_param(op_idx);

    RenderMethodPostprocessTexture {
        bitmap_path,
        bitmap_index: 0,
        address_mode_x,
        address_mode_y,
        filter_mode,
        comparison_function,
        extern_texture_mode,
        texture_transform_constant_index,
        texture_transform_overlay_indices,
    }
}

// =============================================================================
// Real / Int / Bool bake — sub_140C4FE00 / 0xC4FC00 / 0xC4F9C0
// =============================================================================

/// Mirrors `sub_140C4FE00` for one rmt2.float_constants slot. Stage 1 +
/// Stage 2 per parameter_type. Delegates to the existing
/// [`compile_real_constant_at_time`] which we've already verified
/// produces the same bytes as the engine.
fn bake_real_constant(
    rmsh: &RenderMethod,
    rmop_chain: &[RenderMethodOptionParameter],
    rmop_idx_by_name: &HashMap<&str, usize>,
    name: &str,
    ctx: &dyn RenderMethodEvalContext,
) -> [f32; 4] {
    let Some(&op_idx) = rmop_idx_by_name.get(name) else {
        return [0.0; 4];
    };
    let op = &rmop_chain[op_idx];
    let rm = find_rmsh_override(rmsh, op);
    let (slot, _is_xform) = compile_real_constant_at_time(op, rm, ctx);
    slot
}

/// Mirrors `sub_140C4FC00` for one rmt2.int_constants slot. rmsh override
/// is unconditional when the rmsh has the entry; no animated overlays.
fn bake_int_constant(
    rmsh: &RenderMethod,
    rmop_chain: &[RenderMethodOptionParameter],
    rmop_idx_by_name: &HashMap<&str, usize>,
    name: &str,
) -> u32 {
    let Some(&op_idx) = rmop_idx_by_name.get(name) else {
        return 0;
    };
    let op = &rmop_chain[op_idx];
    if let Some(rm) = find_rmsh_override(rmsh, op) {
        rm.int_parameter as u32
    } else {
        op.default_int_bool_value as u32
    }
}

/// Mirrors `sub_140C4F9C0` for one rmt2.bool_constants bit. Returns
/// the bit's value (true → set, false → clear).
fn bake_bool_constant(
    rmsh: &RenderMethod,
    rmop_chain: &[RenderMethodOptionParameter],
    rmop_idx_by_name: &HashMap<&str, usize>,
    name: &str,
) -> bool {
    let Some(&op_idx) = rmop_idx_by_name.get(name) else {
        return false;
    };
    let op = &rmop_chain[op_idx];
    if let Some(rm) = find_rmsh_override(rmsh, op) {
        rm.int_parameter != 0
    } else {
        op.default_int_bool_value != 0
    }
}

// =============================================================================
// Runtime queryable_properties population
// =============================================================================

/// Per-slot lookup names for `runtime_queryable_properties[0..7]`. Each
/// slot maps to one named parameter; the bake records its index into
/// `textures` (or `real_constants` for slot 4 = `_query_albedo_color`).
///
/// Anchored to Ares `e_runtime_queryable_property` (8 entries):
/// 0: `_query_blend_map`        → "blend_map"
/// 1: `_query_albedo_base_map_0` → "base_map" / "base_map_m_0"
/// 2: `_query_albedo_base_map_1` → "base_map_m_1"
/// 3: `_query_albedo_base_map_2` → "base_map_m_2"
/// 4: `_query_albedo_color`     → "albedo_color" (real_constants)
/// 5: `_query_albedo_base_map_3` → "base_map_m_3"
/// 6: `_query_alpha_map`        → "alpha_test_map"
/// 7: `_query_pad`              → unused (always -1)
///
/// Multi-name slots (1) handle the rmsh/rmhg "base_map" vs rmtr
/// "base_map_m_0" naming — both used as the engine's "albedo slot 0".
const QUERYABLE_NAMES: [&[&str]; 8] = [
    &["blend_map"],
    &["base_map", "base_map_m_0"],
    &["base_map_m_1"],
    &["base_map_m_2"],
    &["albedo_color"], // slot 4 — real_constants, not textures
    &["base_map_m_3"],
    &["alpha_test_map"],
    &[],
];

/// Fill `runtime_queryable_properties[0..7]` by looking up each slot's
/// canonical parameter name in the appropriate table (`textures` for
/// the texture-bearing slots; `real_constants` for slot 4). Slot 7 is
/// always -1 (engine `_query_pad`).
///
/// Mirrors tool.exe's post-loop queryable population. The exact tool.exe
/// function isn't yet IDA-anchored (queryable strings aren't referenced
/// directly — the bake probably uses string_ids), but the output matches
/// tagtool dumps across all 5 validation shaders.
fn populate_queryable_properties(
    pp: &mut RenderMethodPostprocessDefinition,
    rmt2: &RenderMethodTemplate,
) {
    pp.runtime_queryable_properties = [-1; 8];
    for (slot, candidates) in QUERYABLE_NAMES.iter().enumerate() {
        // Slot 4 (albedo_color) indexes real_constants; others index textures.
        let pool: &[String] = if slot == 4 {
            &rmt2.float_constants
        } else {
            &rmt2.textures
        };
        for name in *candidates {
            if let Some(idx) = pool.iter().position(|n| n == *name) {
                if let Ok(idx_i16) = i16::try_from(idx) {
                    pp.runtime_queryable_properties[slot] = idx_i16;
                    break;
                }
            }
        }
    }
}

// =============================================================================
// Template path canonicalization
// =============================================================================

/// Rebuild the canonical template path by padding `rmsh.options` to
/// `rmdf.categories.len()` with trailing 0s and emitting
/// `<dir>\_{opt0}_{opt1}_..._{optN}` (the rmt2 naming convention).
///
/// Author-format rmsh tags often ship with the template_path truncated
/// at the last non-zero option (e.g. `_6_0_0_0_0_0_0_3_0_0`, 10 opts);
/// tool.exe rewrites this during bake to the fully-padded form
/// (`_6_0_0_0_0_0_0_3_0_0_0_0_0` for a 13-category rmdf). protomorph's
/// `locate_rmt2` glob-handles the short form at load time, so this is
/// cosmetic for the runtime — but produces byte-equal output to
/// tagtool's `exportcommands` dump.
///
/// Returns `None` if the existing `template_path` has no `\_` separator
/// the canonical form could anchor to (e.g. an empty path).
fn canonical_template_path(template_path: &str, rmsh_options: &[i16], category_count: usize) -> Option<String> {
    // Find the last path-separator + underscore boundary. Everything
    // before that stays as the directory portion. The engine constructs
    // the name as `<dir>\_{joined}.render_method_template`, but our
    // `template_path` here is just `<dir>\_{joined}` (no extension).
    let last_slash = template_path.rfind(['\\', '/'])?;
    let dir = &template_path[..last_slash];
    let name_start = template_path.get(last_slash + 1..)?;
    // The name should start with `_`. If not, this isn't a canonical
    // rmt2 path; leave it alone.
    if !name_start.starts_with('_') {
        return None;
    }
    // Pad options to category_count with trailing 0s.
    let mut padded: Vec<i16> = rmsh_options.to_vec();
    padded.resize(category_count, 0);
    let mut name = String::with_capacity(name_start.len() + 8);
    for opt in &padded {
        name.push('_');
        // Engine writes the raw option index; negative values rare but
        // legal in the schema (rmdf flag-marker path).
        name.push_str(&opt.to_string());
    }
    Some(format!("{dir}\\{name}"))
}

// =============================================================================
// blend_mode resolution — sub_140C638C0
// =============================================================================

/// Resolve the rmsh's chosen `blend_mode` category option name to its
/// enum index. The engine uses a 14-entry string table (`"opaque"`,
/// `"additive"`, `"multiply"`, `"alpha_blend"`, ...). We re-derive the
/// index from the rmdf's category-options ordering — the rmdf's
/// `blend_mode` category lists its options in the same order as
/// `e_render_method_blend_mode`.
fn resolve_blend_mode(rmsh: &RenderMethod, rmdf: &RenderMethodDefinition) -> i32 {
    let Some(cat_idx) = rmdf
        .categories
        .iter()
        .position(|c| c.category_name == "blend_mode")
    else {
        return 0;
    };
    rmsh.options.get(cat_idx).copied().unwrap_or(0) as i32
}

// =============================================================================
// Routing inheritance — sub_140C4D8D0
// =============================================================================

/// Mirror of `sub_140C50660`. Iterates a slice of `rmt2.routing_info`
/// (`rmt2_range`), filters each entry by rmop parameter_type and by
/// animated-parameter type, and appends a new entry into
/// `pp.routing_info` for each matching `(rmop, overlay)` pair.
///
/// The output postprocess routing entry repacks the fields:
/// - `destination_index` ← unchanged (preserves the shader register index)
/// - `source_index`      ← OVERLAY INDEX in `pp.overlays` (not rmt2's source)
/// - `type_specific`     ← rmt2's original `source_index` (rmt2 param table idx)
///
/// Returns the count of entries appended; caller packs `(start, count)`
/// into a TagBlockIndex for the per-pass field.
fn filter_routing_pass(
    pp_routing_info: &mut Vec<super::types::RenderMethodRoutingInfo>,
    rmt2_routing_info: &[super::types::RenderMethodRoutingInfo],
    rmt2_pass_field: TagBlockIndex,
    rmt2_param_names: &[String],
    rmop_chain: &[RenderMethodOptionParameter],
    rmop_idx_by_name: &HashMap<&str, usize>,
    overlay_map: &OverlayMap,
    type_filter: u32,       // 1 << parameter_type bitmask
    anim_type_filter: u32,  // 1 << animated_parameter_type bitmask
) -> u16 {
    let mut emitted: u16 = 0;
    let rmt2_start = rmt2_pass_field.start() as usize;
    let rmt2_count = rmt2_pass_field.count() as usize;
    for rmt2_i in rmt2_start..(rmt2_start + rmt2_count) {
        let Some(rt) = rmt2_routing_info.get(rmt2_i) else { break };
        // Resolve param name via rmt2's source-name table.
        let Some(param_name) = rmt2_param_names.get(rt.source_index as usize) else { continue };
        // Find this name in the rmop chain.
        let Some(&op_idx) = rmop_idx_by_name.get(param_name.as_str()) else { continue };
        let op = &rmop_chain[op_idx];
        let Some(ptype) = op.parameter_type else { continue };
        // Filter by parameter_type (engine encodes `a4` as a bitmask).
        if (type_filter & (1u32 << (ptype as u32))) == 0 {
            continue;
        }
        // Iterate this param's overlay range and filter by animation type.
        let overlay_range = overlay_map.for_rmop_param(op_idx);
        let ov_start = overlay_range.start() as usize;
        let ov_count = overlay_range.count() as usize;
        for ov_i in ov_start..(ov_start + ov_count) {
            let Some(anim) = overlay_map.overlays.get(ov_i) else { break };
            let Some(atype) = anim.parameter_type else { continue };
            if (anim_type_filter & (1u32 << (atype as u32))) == 0 {
                continue;
            }
            // Emit one repacked routing entry.
            pp_routing_info.push(super::types::RenderMethodRoutingInfo {
                destination_index: rt.destination_index,
                source_index:      ov_i as u8,            // overlay index
                type_specific:     rt.source_index,       // original rmop param index
            });
            emitted = emitted.saturating_add(1);
        }
    }
    emitted
}

/// Mirror of `sub_140C4D8D0`. Inherits `entry_points` and `passes` *count*
/// from the rmt2 template, then rebuilds `routing_info` and each pass's
/// 3 TagBlockIndex fields by filtering rmt2.routing_info via
/// [`filter_routing_pass`] per (pass × field-category).
///
/// **Gated on overlay count**: the engine only runs this whole block
/// when `postprocess.overlays.count > 0`. Shaders with no dynamic
/// animated_parameters (assault_rifle, terrain, halogram, …) ship with
/// empty entry_points/passes/routing_info — verified against tagtool
/// dumps of those tags. The check is at `sub_140C4D8D0` line:
/// `if ((int)v5[23] > 0)` where `v5[23]` is overlays.count.
///
/// Per-pass filter rules (mirror tool.exe call sites verbatim):
/// - **textures**:      `type_filter = 1<<Bitmap (0x01)`,
///                      `anim_type_filter = 1<<FrameIndex (0x80)`
/// - **vertex_real**:   `type_filter = 0xFFFFFFE7` (all except Int, Bool),
///                      `anim_type_filter = 0xFFFFFFFF`
/// - **pixel_real**:    same as vertex_real
///
/// The output `pp.routing_info` is populated in entry-point traversal
/// order (not rmt2.passes order) — each pass's TagBlockIndex slice is
/// contiguous in the final routing_info block.
fn populate_routing_from_rmt2(
    pp: &mut RenderMethodPostprocessDefinition,
    rmt2: &RenderMethodTemplate,
    rmop_chain: &[RenderMethodOptionParameter],
    rmop_idx_by_name: &HashMap<&str, usize>,
    overlay_map: &OverlayMap,
) {
    // Engine gate: no overlays → no entry_points/passes/routing_info.
    // Without this gate we'd populate everything from rmt2 unconditionally,
    // which diverges from tagtool dumps of any shader without dynamic
    // animated_parameters.
    if overlay_map.overlays.is_empty() {
        pp.entry_points.clear();
        pp.passes.clear();
        pp.routing_info.clear();
        return;
    }

    // Engine bitmasks (verbatim from sub_140C4D8D0 / sub_140C50660 args).
    const TEXTURE_TYPE_FILTER:    u32 = 1u32 << 0;          // Bitmap only
    const TEXTURE_ANIM_FILTER:    u32 = 1u32 << 7;          // FrameIndex only
    // -25 as i32 widened to u32 = 0xFFFFFFE7 — every type except 3 (Int)
    // and 4 (Bool). In practice covers Bitmap, Color, Real, ArgbColor.
    const REAL_TYPE_FILTER:       u32 = 0xFFFFFFE7;
    const REAL_ANIM_FILTER:       u32 = 0xFFFFFFFF;

    pp.entry_points = rmt2.entry_points.clone();
    pp.routing_info.clear();
    pp.passes = Vec::with_capacity(rmt2.passes.len());

    // Build a per-rmt2-pass index list, ordered by entry-point traversal —
    // the engine populates passes in the order each entry-point references
    // them, so pp.routing_info ends up packed by that order. Walks each
    // rmt2.entry_points[i] and visits its pass-range.
    let mut visited = vec![false; rmt2.passes.len()];
    // Allocate pp.passes slots up front so per-pass writes can land in
    // the rmt2-defined index regardless of traversal order. Each slot
    // stays at Default::default() (all-zero TagBlockIndices) until
    // visited by an entry-point.
    pp.passes.resize(rmt2.passes.len(), super::types::RenderMethodPostprocessPass::default());

    for entry_point_idx in 0..rmt2.entry_points.len() {
        let ep = rmt2.entry_points[entry_point_idx];
        let pass_start = ep.start() as usize;
        let pass_count = ep.count() as usize;
        for pass_idx in pass_start..(pass_start + pass_count) {
            if visited.get(pass_idx).copied().unwrap_or(true) {
                continue;
            }
            visited[pass_idx] = true;
            let Some(rmt2_pass) = rmt2.passes.get(pass_idx) else { continue };

            // textures field
            let routing_start_t = pp.routing_info.len() as u16;
            let count_t = filter_routing_pass(
                &mut pp.routing_info,
                &rmt2.routing_info,
                rmt2_pass.bitmaps,
                &rmt2.textures,
                rmop_chain,
                rmop_idx_by_name,
                overlay_map,
                TEXTURE_TYPE_FILTER,
                TEXTURE_ANIM_FILTER,
            );
            // vertex_real field
            let routing_start_vr = pp.routing_info.len() as u16;
            let count_vr = filter_routing_pass(
                &mut pp.routing_info,
                &rmt2.routing_info,
                rmt2_pass.vertex_real_constants,
                &rmt2.float_constants,
                rmop_chain,
                rmop_idx_by_name,
                overlay_map,
                REAL_TYPE_FILTER,
                REAL_ANIM_FILTER,
            );
            // pixel_real field
            let routing_start_pr = pp.routing_info.len() as u16;
            let count_pr = filter_routing_pass(
                &mut pp.routing_info,
                &rmt2.routing_info,
                rmt2_pass.pixel_real_constants,
                &rmt2.float_constants,
                rmop_chain,
                rmop_idx_by_name,
                overlay_map,
                REAL_TYPE_FILTER,
                REAL_ANIM_FILTER,
            );

            // Engine semantic (mirrors sub_140C4F690 overlay packing):
            // when `count == 0`, the start field is also zero. Avoids
            // leaking the "next free routing slot" into idle pass fields.
            fn pack(start: u16, count: u16) -> TagBlockIndex {
                if count == 0 {
                    TagBlockIndex::default()
                } else {
                    TagBlockIndex::new(start, count)
                }
            }
            pp.passes[pass_idx] = super::types::RenderMethodPostprocessPass {
                bitmaps:               pack(routing_start_t,  count_t),
                vertex_real_constants: pack(routing_start_vr, count_vr),
                pixel_real_constants:  pack(routing_start_pr, count_pr),
            };
        }
    }
}

// =============================================================================
// Top-level bake — sub_140C52530
// =============================================================================

impl RenderMethod {
    /// Engine-faithful tool.exe bake. Populates `self.postprocess_definition`
    /// with what tool.exe would have written into the cache at build time —
    /// matching the runtime view of `s_render_method_postprocess_definition`
    /// for the `(rmsh + rmdf + rmt2 + rmops)` quad.
    ///
    /// Idempotent: when the rmsh already ships pre-baked (`textures.len() > 0`
    /// or `real_constants.len() > 0`), returns `Ok(())` without touching
    /// the existing data.
    ///
    /// Anchors:
    /// - [`reference_tag_to_bake_render_method_2026_05_23`] — full call graph
    /// - [`reference_tool_exe_bake_vs_tagtool_2026_05_23`] — per-type semantics
    pub fn bake(
        &mut self,
        rmdf: &RenderMethodDefinition,
        rmt2: &RenderMethodTemplate,
        load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
        eval_time: f32,
    ) -> Result<(), BakeError> {
        self.bake_with_ctx(rmdf, rmt2, load_rmop, &DefaultEvalContext { eval_time })
    }

    /// Like [`bake`] but threads a caller-supplied
    /// [`RenderMethodEvalContext`] so animated_parameter functions with
    /// named inputs (e.g., `"battery_empty"`) get resolved by the caller's
    /// game state. Most users should use [`bake`] which defaults to
    /// time-only evaluation.
    pub fn bake_with_ctx(
        &mut self,
        rmdf: &RenderMethodDefinition,
        rmt2: &RenderMethodTemplate,
        load_rmop: impl FnMut(&str) -> Option<RenderMethodOption>,
        ctx: &dyn RenderMethodEvalContext,
    ) -> Result<(), BakeError> {
        // Idempotency: bail if the rmsh already carries baked data. Skips
        // both the cache-baked case (textures/real_constants pre-filled)
        // and the "already baked once this session" case.
        if let Some(pp) = self.postprocess_definition.as_ref() {
            if !pp.textures.is_empty() || !pp.real_constants.is_empty() {
                return Ok(());
            }
        }

        if rmt2.bool_constants.len() > 32 {
            return Err(BakeError::TooManyBooleanConstants);
        }

        // Capture the template path before mutating. The author-format
        // rmsh's postprocess_definition has the template ref even when
        // the rest of the postprocess is empty.
        let template_path = self
            .postprocess_definition
            .as_ref()
            .map(|p| p.template_path.clone())
            .unwrap_or_default();
        if template_path.is_empty() {
            return Err(BakeError::MissingTemplate);
        }

        // Phase 2: rmop chain
        let rmop_chain = build_rmop_chain(self, rmdf, load_rmop);
        // Build a name → rmop-chain-index map so each baker can O(1) the lookup.
        let rmop_idx_by_name: HashMap<&str, usize> = rmop_chain
            .iter()
            .enumerate()
            .map(|(i, p)| (p.parameter_name.as_str(), i))
            .collect();

        // Phase 3: animated overlay split (static-vs-dynamic)
        let overlays = split_animated_parameters(self, &rmop_chain);

        // Phase 5: four parameter bake loops
        let textures: Vec<RenderMethodPostprocessTexture> = rmt2
            .textures
            .iter()
            .map(|name| {
                bake_texture_constant(
                    self,
                    &rmop_chain,
                    &rmop_idx_by_name,
                    &overlays,
                    &rmt2.float_constants,
                    name,
                )
            })
            .collect();

        let real_constants: Vec<[f32; 4]> = rmt2
            .float_constants
            .iter()
            .map(|name| bake_real_constant(self, &rmop_chain, &rmop_idx_by_name, name, ctx))
            .collect();

        let int_constants: Vec<i32> = rmt2
            .int_constants
            .iter()
            .map(|name| bake_int_constant(self, &rmop_chain, &rmop_idx_by_name, name) as i32)
            .collect();

        let mut bool_constants: u32 = 0;
        for (bit, name) in rmt2.bool_constants.iter().enumerate() {
            if bake_bool_constant(self, &rmop_chain, &rmop_idx_by_name, name) {
                bool_constants |= 1u32 << bit;
            }
        }

        // Phase 8: blend_mode
        let blend_mode = resolve_blend_mode(self, rmdf);

        // Tool.exe rewrites the template_path during bake by padding
        // rmsh.options to rmdf.categories.len() and emitting the
        // canonical `_{opt0}_..._{optN}` form. The original author-format
        // path is often truncated.
        let canonical_path = canonical_template_path(
            &template_path,
            &self.options,
            rmdf.categories.len(),
        ).unwrap_or(template_path);

        // Phase 6: entry_points / passes / routing_info copy from rmt2
        let mut pp = self
            .postprocess_definition
            .take()
            .unwrap_or_else(RenderMethodPostprocessDefinition::default);
        pp.template_path = canonical_path;
        pp.textures = textures;
        pp.real_constants = real_constants;
        pp.int_constants = int_constants;
        pp.bool_constants = bool_constants;
        pp.blend_mode = blend_mode;
        // populate_routing_from_rmt2 reads pp.overlays via overlay_map's
        // `overlays` field, so move it AFTER routing is built. Avoids
        // having to keep `OverlayMap` alive past the texture loop.
        populate_routing_from_rmt2(
            &mut pp,
            rmt2,
            &rmop_chain,
            &rmop_idx_by_name,
            &overlays,
        );
        pp.overlays = overlays.overlays;
        populate_queryable_properties(&mut pp, rmt2);

        self.postprocess_definition = Some(pp);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_index_packing_matches_engine() {
        // shrine_clouds_sandstorm detail_map: overlays start=0, count=2
        // → tagtool dumps `TextureTransformOverlayIndices.Integer 2048` = 0x800.
        let bi = TagBlockIndex::new(0, 2);
        assert_eq!(bi.0, 0x800);
        // detail_map2: start=2, count=2 → tagtool dumps 2050 = 0x802.
        let bi = TagBlockIndex::new(2, 2);
        assert_eq!(bi.0, 0x802);
    }
}
