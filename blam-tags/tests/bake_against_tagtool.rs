//! Engine-faithful bake validation.
//!
//! Loads real H3 MCC author-format `.shader` tags from
//! `~/Halo/halo3_mcc/tags/...`, runs [`RenderMethod::bake`], and checks
//! the resulting `postprocess_definition` against ground-truth values
//! captured from TagTool's `exportcommands` dump of the same shader as
//! it appears in a cache-compiled map (i.e. what tool.exe wrote).
//!
//! See `reference_tag_to_bake_render_method_2026_05_23.md` and
//! `reference_tool_exe_bake_vs_tagtool_2026_05_23.md` for the engine
//! anchors driving each assertion.
//!
//! Tests are gated on the H3 MCC tag tree existing on disk — they skip
//! gracefully when run on a machine without the asset corpus.

use std::path::{Path, PathBuf};

use blam_tags::render_method::{
    BitmapAddressMode, BitmapFilterMode, RenderMethod, RenderMethodDefinition, RenderMethodOption,
    RenderMethodTemplate,
};
use blam_tags::TagFile;

fn tags_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let p = PathBuf::from(home).join("Halo/halo3_mcc/tags");
    p.is_dir().then_some(p)
}

fn load_tag(path: &Path) -> TagFile {
    TagFile::read(path).unwrap_or_else(|e| {
        panic!("failed to read {}: {e}", path.display());
    })
}

/// Resolve `shaders\foo\bar.render_method_option` → the absolute filesystem path.
fn resolve_rmop_path(tags_root: &Path, tag_relative: &str) -> PathBuf {
    let normalized = tag_relative.replace('\\', "/");
    tags_root.join(format!("{normalized}.render_method_option"))
}

fn load_rmop(tags_root: &Path, tag_relative: &str) -> Option<RenderMethodOption> {
    let p = resolve_rmop_path(tags_root, tag_relative);
    if !p.exists() {
        return None;
    }
    RenderMethodOption::from_tag(&load_tag(&p)).ok()
}

#[test]
fn shrine_clouds_sandstorm_bake_matches_tagtool() {
    let Some(tags) = tags_root() else {
        eprintln!("skipping: ~/Halo/halo3_mcc/tags not present");
        return;
    };

    // Load rmsh
    let rmsh_path = tags.join("levels/multi/shrine/sky/shaders/shrine_clouds_sandstorm.shader");
    let mut rmsh = RenderMethod::from_tag(&load_tag(&rmsh_path))
        .expect("parse shrine_clouds_sandstorm.shader");

    // Load rmdf
    let rmdf_path = tags.join("shaders/shader.render_method_definition");
    let rmdf = RenderMethodDefinition::from_tag(&load_tag(&rmdf_path))
        .expect("parse shaders/shader.rmdf");

    // Load rmt2
    let rmt2_path = tags.join(
        "shaders/shader_templates/_6_0_0_0_0_0_0_3_0_0_0_0_0.render_method_template",
    );
    let rmt2 = RenderMethodTemplate::from_tag(&load_tag(&rmt2_path))
        .expect("parse rmt2 for shrine_clouds_sandstorm");

    // Bake
    rmsh.bake(&rmdf, &rmt2, |p| load_rmop(&tags, p), 0.0)
        .expect("bake should succeed");

    let pp = rmsh
        .postprocess_definition
        .as_ref()
        .expect("post-bake postprocess present");

    // ---- Ground-truth comparison (from TagTool exportcommands of the
    // ---- cache-compiled shrine_clouds_sandstorm.shader) ----

    // Real constants — Stage 1 hardcoded [1,1,0,0] for Bitmap params,
    // Stage 2 overlaid with rmsh's static-period ScaleX/ScaleY animated_parameters.
    // Source: tool.exe's `SetArgument <name> X Y Z W` exportcommands.
    assert!(pp.real_constants.len() >= 4, "expected ≥4 real_constants, got {}", pp.real_constants.len());
    assert_eq!(pp.real_constants[0], [1.0, 1.0, 0.0, 0.0], "base_map");
    assert_eq!(pp.real_constants[1], [7.0, 2.0, 0.0, 0.0], "detail_map ScaleX/Y constant");
    assert_eq!(pp.real_constants[2], [10.5, 5.0, 0.0, 0.0], "detail_map2 ScaleX/Y constant");
    assert_eq!(pp.real_constants[3], [1.0, 1.0, 0.0, 0.0], "detail_map_overlay (no animated)");

    // Texture constants — sampler state. The rmsh authors
    // `bitmap_address_mode_x = 1 (Clamp)` for detail_map but does NOT set
    // `bitmap_flags & 0x04` → engine ignores per-axis _x → falls through
    // to rmop default (Wrap). TagTool dump: SamplerAddressUV = 0 for all.
    assert!(pp.textures.len() >= 4, "expected ≥4 texture_constants");
    for (i, tex) in pp.textures.iter().take(4).enumerate() {
        assert_eq!(
            tex.address_mode_x,
            BitmapAddressMode::Wrap,
            "texture[{i}].address_mode_x should be Wrap (bitmap_flags not set)",
        );
        assert_eq!(
            tex.address_mode_y,
            BitmapAddressMode::Wrap,
            "texture[{i}].address_mode_y should be Wrap",
        );
        assert_eq!(
            tex.filter_mode,
            BitmapFilterMode::Trilinear,
            "texture[{i}].filter_mode should be Trilinear",
        );
    }

    // texture_transform_constant_index = the index of the matching real_constants
    // slot (same name). For shrine_clouds_sandstorm: textures and real_constants
    // share their parameter ordering 1:1, so each maps to its own index.
    assert_eq!(pp.textures[0].texture_transform_constant_index, 0);
    assert_eq!(pp.textures[1].texture_transform_constant_index, 1);
    assert_eq!(pp.textures[2].texture_transform_constant_index, 2);
    assert_eq!(pp.textures[3].texture_transform_constant_index, 3);

    // texture_transform_overlay_indices — packed (start:10, count:6).
    // detail_map has 2 dynamic animated_parameters (TranslationX/Y, period=22s,
    // is_constant=false). detail_map2 has 2 more. base_map / detail_map_overlay
    // have none. TagTool dump matches:
    //   - textures[0] (base_map):       0       (count=0, start=0)
    //   - textures[1] (detail_map):     0x800   (count=2, start=0)
    //   - textures[2] (detail_map2):    0x802   (count=2, start=2)
    //   - textures[3] (detail_map_overlay): 0   (count=0, start=0)
    assert_eq!(pp.textures[0].texture_transform_overlay_indices.0, 0);
    assert_eq!(pp.textures[1].texture_transform_overlay_indices.0, 0x800);
    assert_eq!(pp.textures[2].texture_transform_overlay_indices.0, 0x802);
    assert_eq!(pp.textures[3].texture_transform_overlay_indices.0, 0);

    // Overlays — total of 4 dynamic animated_parameters across the two
    // detail_maps (2 translations each at period=22s / 20s / 230s).
    assert_eq!(pp.overlays.len(), 4, "expected 4 dynamic animated_parameters");

    // blend_mode = rmsh.options[blend_mode_category_index] = 3 = AlphaBlend
    // (TagTool dump: `SetField ShaderProperties[0].BlendMode AlphaBlend`).
    assert_eq!(pp.blend_mode, 3, "blend_mode = AlphaBlend");

    // Bitmap paths plumbed through from rmsh overrides.
    assert!(
        pp.textures[0].bitmap_path.ends_with("shrine_clouds_sandstorm_base"),
        "base_map bitmap_path: {}",
        pp.textures[0].bitmap_path,
    );
    assert!(
        pp.textures[1].bitmap_path.ends_with("shrine_clouds_sandstorm_dif"),
        "detail_map bitmap_path: {}",
        pp.textures[1].bitmap_path,
    );
}

/// Comprehensive byte-for-byte comparison against TagTool's
/// `exportcommands` dump of shrine_clouds_sandstorm.shader as
/// it appears in a cache-compiled map. Every assertion here
/// quotes the dump line it's verifying.
#[test]
fn shrine_clouds_sandstorm_full_postprocess_diff() {
    let Some(tags) = tags_root() else { return };

    let rmsh_path = tags.join("levels/multi/shrine/sky/shaders/shrine_clouds_sandstorm.shader");
    let mut rmsh = RenderMethod::from_tag(&load_tag(&rmsh_path)).unwrap();
    let rmdf = RenderMethodDefinition::from_tag(&load_tag(
        &tags.join("shaders/shader.render_method_definition"),
    )).unwrap();
    let rmt2 = RenderMethodTemplate::from_tag(&load_tag(&tags.join(
        "shaders/shader_templates/_6_0_0_0_0_0_0_3_0_0_0_0_0.render_method_template",
    ))).unwrap();
    rmsh.bake(&rmdf, &rmt2, |p| load_rmop(&tags, p), 0.0).unwrap();
    let pp = rmsh.postprocess_definition.as_ref().unwrap();

    // ─── Template path ────────────────────────────────────────────────
    // TagTool: `Template shaders\shader_templates\_6_0_0_0_0_0_0_3_0_0_0_0_0.render_method_template`
    // The author-format rmsh on disk has only 10 underscores; tool.exe
    // pads rmsh.options to rmdf.categories.len()=13 and rebuilds the
    // canonical path. Our bake does the same.
    assert_eq!(
        pp.template_path,
        r"shaders\shader_templates\_6_0_0_0_0_0_0_3_0_0_0_0_0",
        "template_path padded to canonical 13-option form",
    );

    // ─── TextureConstants[4] ──────────────────────────────────────────
    // TagTool ground-truth (per-texture):
    //   [0] base_map               SamplerAddressUV=0  Filter=Trilinear Cmp=0 Extern=UseBitmapAsNormal TransformIdx=0 OverlayIndices=0
    //   [1] detail_map             SamplerAddressUV=0  Filter=Trilinear Cmp=0 Extern=UseBitmapAsNormal TransformIdx=1 OverlayIndices=2048
    //   [2] detail_map2            SamplerAddressUV=0  Filter=Trilinear Cmp=0 Extern=UseBitmapAsNormal TransformIdx=2 OverlayIndices=2050
    //   [3] detail_map_overlay     SamplerAddressUV=0  Filter=Trilinear Cmp=0 Extern=UseBitmapAsNormal TransformIdx=3 OverlayIndices=0
    assert_eq!(pp.textures.len(), 4, "expected 4 texture_constants");
    for (i, t) in pp.textures.iter().enumerate() {
        assert_eq!(t.bitmap_index, 0, "texture[{i}].bitmap_index");
        assert_eq!(t.address_mode_x, BitmapAddressMode::Wrap, "texture[{i}].addr_x (SamplerAddressUV nibble lo)");
        assert_eq!(t.address_mode_y, BitmapAddressMode::Wrap, "texture[{i}].addr_y (SamplerAddressUV nibble hi)");
        assert_eq!(t.filter_mode, BitmapFilterMode::Trilinear, "texture[{i}].filter");
        // comparison_function = 0 in dump → blam-tags enum 0 = Never.
        assert!(
            matches!(t.comparison_function,
                blam_tags::render_method::BitmapComparisonFunction::Never),
            "texture[{i}].comparison_function = Never (0)",
        );
        assert_eq!(t.texture_transform_constant_index as i32, i as i32, "texture[{i}].transform_constant_index");
    }
    assert_eq!(pp.textures[0].texture_transform_overlay_indices.0, 0,      "tex[0].overlay = 0");
    assert_eq!(pp.textures[1].texture_transform_overlay_indices.0, 2048,   "tex[1].overlay = 2048 (0x800)");
    assert_eq!(pp.textures[2].texture_transform_overlay_indices.0, 2050,   "tex[2].overlay = 2050 (0x802)");
    assert_eq!(pp.textures[3].texture_transform_overlay_indices.0, 0,      "tex[3].overlay = 0");

    // ─── RealConstants[4] ─────────────────────────────────────────────
    // TagTool `SetArgument` ground-truth.
    assert_eq!(pp.real_constants.len(), 4);
    assert_eq!(pp.real_constants[0], [1.0, 1.0, 0.0, 0.0]);
    assert_eq!(pp.real_constants[1], [7.0, 2.0, 0.0, 0.0]);
    assert_eq!(pp.real_constants[2], [10.5, 5.0, 0.0, 0.0]);
    assert_eq!(pp.real_constants[3], [1.0, 1.0, 0.0, 0.0]);

    // ─── BooleanConstants ─────────────────────────────────────────────
    // TagTool: `SetField ShaderProperties[0].BooleanConstants 0`
    assert_eq!(pp.bool_constants, 0, "bool_constants");

    // ─── EntryPoints[18] ──────────────────────────────────────────────
    // TagTool ground-truth (.Integer fields):
    let expected_entry_points: [u16; 18] = [
        0, 1024, 0, 1025, 1027, 1026, 1030, 1031, 1032, 1028, 1029, 0, 1033, 1035, 1034, 0, 0, 1036,
    ];
    assert_eq!(pp.entry_points.len(), expected_entry_points.len(),
        "entry_points count (rmt2 had {})", rmt2.entry_points.len());
    for (i, &expected) in expected_entry_points.iter().enumerate() {
        assert_eq!(pp.entry_points[i].0, expected, "entry_points[{i}]");
    }

    // ─── Passes[13] ───────────────────────────────────────────────────
    // TagTool ground-truth (Texture.Integer, RealVertex.Integer, RealPixel.Integer):
    let expected_passes: [(u16, u16, u16); 13] = [
        (0, 0, 4096),
        (0, 0, 4100),
        (0, 0, 4108),
        (0, 0, 4104),
        (0, 0, 4124),
        (0, 0, 0),
        (0, 0, 4112),
        (0, 0, 4116),
        (0, 0, 4120),
        (0, 0, 0),
        (0, 0, 4128),
        (0, 0, 0),
        (0, 0, 4132),
    ];
    assert_eq!(pp.passes.len(), expected_passes.len(),
        "passes count (rmt2 had {})", rmt2.passes.len());
    for (i, &(tex, vs, ps)) in expected_passes.iter().enumerate() {
        assert_eq!(pp.passes[i].bitmaps.0,               tex, "pass[{i}].bitmaps");
        assert_eq!(pp.passes[i].vertex_real_constants.0, vs,  "pass[{i}].vertex_real_constants");
        assert_eq!(pp.passes[i].pixel_real_constants.0,  ps,  "pass[{i}].pixel_real_constants");
    }

    // ─── RoutingInfo[40] ──────────────────────────────────────────────
    // TagTool ground-truth (RegisterIndex, FunctionIndex, SourceIndex):
    // Pattern repeats 10× of [28929/0/1, 28929/1/1, 28930/2/2, 28930/3/2].
    //
    // Engine field mapping (sub_140C50660):
    //   destination_index ← rmt2.routing.destination_index (= RegisterIndex)
    //   source_index      ← overlay loop index v26         (= FunctionIndex)
    //   type_specific     ← rmt2.routing.source_index      (= SourceIndex)
    let expected_routing: [(u16, u8, u8); 40] = {
        // tuple = (RegisterIndex, FunctionIndex, SourceIndex)
        let mut out = [(0u16, 0u8, 0u8); 40];
        let pattern = [(28929u16, 0u8, 1u8), (28929, 1, 1), (28930, 2, 2), (28930, 3, 2)];
        for i in 0..40 {
            out[i] = pattern[i % 4];
        }
        out
    };
    assert_eq!(pp.routing_info.len(), expected_routing.len(),
        "routing_info count (rmt2 had {})", rmt2.routing_info.len());
    for (i, &(register, function, source)) in expected_routing.iter().enumerate() {
        assert_eq!(pp.routing_info[i].destination_index, register, "routing[{i}].destination_index (RegisterIndex)");
        assert_eq!(pp.routing_info[i].source_index,      function, "routing[{i}].source_index (FunctionIndex)");
        assert_eq!(pp.routing_info[i].type_specific,     source,   "routing[{i}].type_specific (SourceIndex)");
    }

    // ─── Functions[4] (overlays) ─────────────────────────────────────
    // TagTool ground-truth (animated_parameters with non-constant functions):
    //   [0] TranslationX period=22, input=invalid, range=invalid
    //   [1] TranslationY period=22
    //   [2] TranslationX period=20
    //   [3] TranslationY period=230
    // (The two static-period ScaleX/Y functions are NOT in overlays —
    // they get baked into real_constants instead.)
    use blam_tags::render_method::RenderMethodAnimatedParameterType as APT;
    assert_eq!(pp.overlays.len(), 4, "overlays count");
    let expect_overlays = [
        (APT::TranslationX, 22.0),
        (APT::TranslationY, 22.0),
        (APT::TranslationX, 20.0),
        (APT::TranslationY, 230.0),
    ];
    for (i, &(ptype, period)) in expect_overlays.iter().enumerate() {
        assert_eq!(pp.overlays[i].parameter_type, Some(ptype), "overlays[{i}].type");
        assert_eq!(pp.overlays[i].time_period_in_seconds, period, "overlays[{i}].period");
    }

    // ─── BlendMode / Flags / ImSoFiredPad ────────────────────────────
    // TagTool: BlendMode = AlphaBlend (enum index 3)
    //          Flags = None (0)
    //          ImSoFiredPad = 0
    assert_eq!(pp.blend_mode, 3, "blend_mode = AlphaBlend");
    assert_eq!(pp.flags, 0, "flags = None");

    // ─── QueryableProperties[8] ──────────────────────────────────────
    // TagTool ground-truth: [-1, 0, -1, -1, -1, -1, -1, -1].
    // Slot 1 (_query_albedo_base_map_0) → textures.position_by_name("base_map") = 0.
    assert_eq!(pp.runtime_queryable_properties, [-1, 0, -1, -1, -1, -1, -1, -1]);

    // ─── Top-level rmsh fields (not part of postprocess) ─────────────
    // TagTool: RenderFlags=None, SortLayer=Normal (=2), RuntimeFlags=0,
    //          CustomFogSettingIndex=0, PredictionAtomIndex=-1.
    assert_eq!(rmsh.flags, 0, "rmsh.flags = None");
    assert_eq!(rmsh.sort_layer, 2, "rmsh.sort_layer = Normal (2)");
    assert_eq!(rmsh.runtime_flags, 0, "rmsh.runtime_flags");
    assert_eq!(rmsh.custom_fog_setting_index, 0, "rmsh.custom_fog_setting_index");
    assert_eq!(rmsh.prediction_atom_index, -1, "rmsh.prediction_atom_index");
}

/// One-shot helper: load (rmsh, rmdf, rmt2), bake, return the rmsh.
fn bake_for(tags: &Path, rmsh_rel: &str, rmt2_rel: &str) -> RenderMethod {
    let mut rmsh = RenderMethod::from_tag(&load_tag(&tags.join(rmsh_rel))).unwrap();
    let rmdf_rel = format!("{}.render_method_definition", rmsh.definition_path.replace('\\', "/"));
    let rmdf = RenderMethodDefinition::from_tag(&load_tag(&tags.join(&rmdf_rel))).unwrap();
    let rmt2 = RenderMethodTemplate::from_tag(&load_tag(&tags.join(rmt2_rel))).unwrap();
    rmsh.bake(&rmdf, &rmt2, |p| load_rmop(tags, p), 0.0).unwrap();
    rmsh
}

// =====================================================================
// rmw — water shader. Cover: per-axis Clamp via SamplerAddressUV=17,
// Color (alpha=1) and ArgbColor (non-1 alpha) Stage 1 fills, 8 dynamic
// overlays driving 24 routing entries, 16 entry_points and 4 passes.
// =====================================================================
#[test]
fn riverworld_water_rough_bake_matches_tagtool() {
    let Some(tags) = tags_root() else { return };
    let rmsh = bake_for(
        &tags,
        "levels/multi/riverworld/shaders/riverworld_water_rough.shader_water",
        "shaders/water_templates/_0_1_1_1_1_0_1_3.render_method_template",
    );
    let pp = rmsh.postprocess_definition.as_ref().unwrap();

    // BlendMode = Opaque (0).
    assert_eq!(pp.blend_mode, 0, "blend_mode = Opaque");

    // Texture count + per-axis sampler routing. SamplerAddressUV=17
    // means address_mode_x=1 AND address_mode_y=1 (both Clamp), which
    // comes from the rmop default since `bitmap_flags` is 0.
    assert_eq!(pp.textures.len(), 7);
    // global_shape_texture (idx 1) has SamplerAddressUV=17.
    assert_eq!(pp.textures[1].address_mode_x, BitmapAddressMode::Clamp, "global_shape addr_x");
    assert_eq!(pp.textures[1].address_mode_y, BitmapAddressMode::Clamp, "global_shape addr_y");
    assert_eq!(pp.textures[1].texture_transform_constant_index, -1, "global_shape no xform");
    // wave_slope_array (idx 2) has SamplerAddressUV=0 (Wrap).
    assert_eq!(pp.textures[2].address_mode_x, BitmapAddressMode::Wrap);
    assert_eq!(pp.textures[2].texture_transform_overlay_indices.0, 2049, "wave_slope overlays");
    // watercolor_texture (idx 3) Clamp.
    assert_eq!(pp.textures[3].address_mode_x, BitmapAddressMode::Clamp);
    assert_eq!(pp.textures[3].address_mode_y, BitmapAddressMode::Clamp);
    // environment_map (idx 4) Clamp.
    assert_eq!(pp.textures[4].address_mode_x, BitmapAddressMode::Clamp);
    // foam_texture / foam_texture_detail dynamic overlays.
    assert_eq!(pp.textures[5].texture_transform_overlay_indices.0, 2052, "foam overlays");
    assert_eq!(pp.textures[6].texture_transform_overlay_indices.0, 2054, "foam_detail overlays");

    // Real constants — verify key Color/ArgbColor and Real broadcasts.
    assert_eq!(pp.real_constants.len(), 37);
    // water_diffuse (idx 21): Color, alpha forced to 1.0.
    let wd = pp.real_constants[21];
    assert!((wd[0] - 0.0666_6667).abs() < 1e-5, "water_diffuse R");
    assert!((wd[1] - 0.1137_2550).abs() < 1e-5, "water_diffuse G");
    assert!((wd[2] - 0.1372_5491).abs() < 1e-5, "water_diffuse B");
    assert_eq!(wd[3], 1.0,                       "water_diffuse alpha forced to 1");
    // slope_range_x: real broadcast.
    assert_eq!(pp.real_constants[0], [0.5, 0.5, 0.5, 0.5]);
    // reflection_coefficient = 600 broadcast.
    assert_eq!(pp.real_constants[11], [600.0, 600.0, 600.0, 600.0]);
    // wave_displacement_array bitmap xform: 1 3 0 0 (static ScaleX=1, ScaleY=3).
    assert_eq!(pp.real_constants[2], [1.0, 3.0, 0.0, 0.0], "wave_displacement_array xform");
    // foam_texture bitmap xform.
    assert_eq!(pp.real_constants[22], [2.0, 1.0, 0.0, 0.0], "foam_texture xform");

    // Overlays — 8 dynamic functions (water has Value + Translation mix).
    assert_eq!(pp.overlays.len(), 8, "8 dynamic animated_parameters");

    // entry_points populated, count=16, only entries 3/4/5/15 non-zero.
    assert_eq!(pp.entry_points.len(), 16);
    assert_eq!(pp.entry_points[3].0,  1025);
    assert_eq!(pp.entry_points[4].0,  1026);
    assert_eq!(pp.entry_points[5].0,  1027);
    assert_eq!(pp.entry_points[15].0, 1024);
    assert_eq!(pp.entry_points[0].0,  0);
    assert_eq!(pp.entry_points[7].0,  0);

    // Passes: 4 entries; routing_info: 24 entries.
    assert_eq!(pp.passes.len(),       4);
    assert_eq!(pp.routing_info.len(), 24, "filtered down from rmt2 routing");
}

// =====================================================================
// rmhg — halogram. Cover: no-overlays gating (entry_points/passes/
// routing empty), Color vs ArgbColor alpha handling, multi-color
// rmop fills.
// =====================================================================
#[test]
fn guardian_light_volume_a_bake_matches_tagtool() {
    let Some(tags) = tags_root() else { return };
    let rmsh = bake_for(
        &tags,
        "levels/multi/guardian/shaders/guardian_light_volume_a.shader_halogram",
        "shaders/halogram_templates/_0_1_1_0_0_0_1_0_0.render_method_template",
    );
    let pp = rmsh.postprocess_definition.as_ref().unwrap();

    // BlendMode = Additive (1).
    assert_eq!(pp.blend_mode, 1, "blend_mode = Additive");

    // No dynamic overlays → entry_points/passes/routing_info all empty.
    assert_eq!(pp.overlays.len(),     0, "no overlays");
    assert!(pp.entry_points.is_empty(),  "entry_points empty when no overlays");
    assert!(pp.passes.is_empty(),        "passes empty when no overlays");
    assert!(pp.routing_info.is_empty(),  "routing_info empty when no overlays");

    // 3 textures, 9 real_constants.
    assert_eq!(pp.textures.len(),       3);
    assert_eq!(pp.real_constants.len(), 9);

    // albedo_color (idx 2) — ArgbColor preserves authored alpha=0.2.
    let ac = pp.real_constants[2];
    assert!((ac[0] - 0.5137_2550).abs() < 1e-5, "albedo R");
    assert!((ac[1] - 0.8313_7260).abs() < 1e-5, "albedo G");
    assert_eq!(ac[2], 1.0,                       "albedo B");
    assert_eq!(ac[3], 0.2,                       "albedo alpha = 0.2 (ArgbColor)");

    // edge_fade_edge_tint (idx 6) — Color, alpha forced to 1.
    assert_eq!(pp.real_constants[6], [0.0, 0.0, 0.0, 1.0]);
    // self_illum_color (idx 4) — Color (1,1,1,1).
    assert_eq!(pp.real_constants[4], [1.0, 1.0, 1.0, 1.0]);
    // self_illum_intensity (idx 5) — Real broadcast 0.3.
    assert_eq!(pp.real_constants[5], [0.3, 0.3, 0.3, 0.3]);

    // queryable_properties — tagtool: [-1, 0, -1, -1, 2, -1, -1, -1]
    //   slot 1 (_query_albedo_base_map_0) → textures[0] = base_map
    //   slot 4 (_query_albedo_color)      → real_constants[2] = albedo_color
    assert_eq!(pp.runtime_queryable_properties, [-1, 0, -1, -1, 2, -1, -1, -1]);
}

// =====================================================================
// rmtr — terrain. Cover: blend_map per-axis Clamp routing, many static
// bitmap xforms baked from animated_parameters with period=0, opaque
// blend_mode, 13 textures, 23 real_constants.
// =====================================================================
#[test]
fn riverworld_ground_cliff_bake_matches_tagtool() {
    let Some(tags) = tags_root() else { return };
    let rmsh = bake_for(
        &tags,
        "levels/multi/riverworld/shaders/riverworld_ground_cliff.shader_terrain",
        "shaders/terrain_templates/_0_0_1_2_0_1.render_method_template",
    );
    let pp = rmsh.postprocess_definition.as_ref().unwrap();

    assert_eq!(pp.blend_mode, 0, "blend_mode = Opaque");

    // No animated_parameters → empty entry_points/passes/routing.
    assert!(pp.overlays.is_empty(),
        "terrain has no dynamic overlays (all ScaleX/Y are static-period)");
    assert!(pp.entry_points.is_empty());
    assert!(pp.passes.is_empty());
    assert!(pp.routing_info.is_empty());

    // 13 textures, 23 real_constants.
    assert_eq!(pp.textures.len(),       13);
    assert_eq!(pp.real_constants.len(), 23);

    // blend_map (idx 0) authored Clamp on both axes (SamplerAddressUV=17).
    assert_eq!(pp.textures[0].address_mode_x, BitmapAddressMode::Clamp);
    assert_eq!(pp.textures[0].address_mode_y, BitmapAddressMode::Clamp);
    // All other textures Wrap.
    for (i, t) in pp.textures.iter().enumerate().skip(1) {
        assert_eq!(t.address_mode_x, BitmapAddressMode::Wrap,
            "textures[{i}].addr_x should be Wrap");
    }

    // Bitmap xform values from static animated ScaleX/ScaleY.
    assert_eq!(pp.real_constants[2],  [8.0, 16.0, 0.0, 0.0], "base_map_m_0 = 8x16");
    assert_eq!(pp.real_constants[6],  [15.0, 30.0, 0.0, 0.0], "base_map_m_2 = 15x30");
    assert_eq!(pp.real_constants[10], [15.0, 30.0, 0.0, 0.0], "base_map_m_3 = 15x30");
    assert_eq!(pp.real_constants[12], [10.0, 20.0, 0.0, 0.0], "bump_map_m_3 = 10x20");

    // Real broadcasts from rmsh overrides.
    assert_eq!(pp.real_constants[15], [0.2, 0.2, 0.2, 0.2], "specular_coefficient_m_0");
    assert_eq!(pp.real_constants[16], [22.0, 22.0, 22.0, 22.0], "specular_power_m_0");
    assert_eq!(pp.real_constants[19], [0.8, 0.8, 0.8, 0.8],  "area_specular_contribution_m_0");

    // queryable_properties — tagtool: [0, 1, -1, 5, -1, 9, -1, -1]
    //   slot 0 (_query_blend_map)         → textures[0] = blend_map
    //   slot 1 (_query_albedo_base_map_0) → textures[1] = base_map_m_0
    //   slot 3 (_query_albedo_base_map_2) → textures[5] = base_map_m_2
    //   slot 5 (_query_albedo_base_map_3) → textures[9] = base_map_m_3
    // No "albedo_color" param on terrain → slot 4 = -1.
    assert_eq!(pp.runtime_queryable_properties, [0, 1, -1, 5, -1, 9, -1, -1]);
}

// =====================================================================
// rmsh — assault_rifle. Cover: ForceSinglePass flag, Anisotropic2Expensive
// filter mode, color params with various alpha handling, bool_constants
// non-zero, no dynamic overlays despite full rmsh.
// =====================================================================
#[test]
fn assault_rifle_bake_matches_tagtool() {
    let Some(tags) = tags_root() else { return };
    let rmsh = bake_for(
        &tags,
        "objects/weapons/rifle/assault_rifle/shaders/assault_rifle.shader",
        "shaders/shader_templates/_0_2_0_1_1_2_1_0_0_1_0_0_0.render_method_template",
    );
    let pp = rmsh.postprocess_definition.as_ref().unwrap();

    assert_eq!(pp.blend_mode, 0, "blend_mode = Opaque");

    // No dynamic overlays (assault_rifle has no animated_parameters in
    // the rmsh) → entry_points/passes/routing empty.
    assert!(pp.overlays.is_empty());
    assert!(pp.entry_points.is_empty(),
        "ForceSinglePass shader has no per-entry-point overlay routing");
    assert!(pp.passes.is_empty());
    assert!(pp.routing_info.is_empty());

    // bool_constants packed bit (single bool set per tagtool).
    assert_eq!(pp.bool_constants, 1, "BooleanConstants = 1");

    // 6 textures all Wrap, all Anisotropic2Expensive except material_texture
    // (which is Trilinear — gray_50_percent placeholder).
    assert_eq!(pp.textures.len(), 6);
    for (i, t) in pp.textures.iter().enumerate() {
        assert_eq!(t.address_mode_x, BitmapAddressMode::Wrap, "tex[{i}].addr_x");
        assert_eq!(t.address_mode_y, BitmapAddressMode::Wrap, "tex[{i}].addr_y");
    }
    assert_eq!(pp.textures[0].filter_mode, BitmapFilterMode::Anisotropic2Expensive, "base_map filter");
    assert_eq!(pp.textures[1].filter_mode, BitmapFilterMode::Anisotropic2Expensive, "detail_map filter");
    assert_eq!(pp.textures[4].filter_mode, BitmapFilterMode::Trilinear,             "material_texture filter");
    assert_eq!(pp.textures[5].filter_mode, BitmapFilterMode::Anisotropic2Expensive, "self_illum_map filter");

    // 22 real_constants — check a few key ones.
    assert_eq!(pp.real_constants.len(), 22);

    // base_map xform — no animated overrides, identity 1,1,0,0.
    assert_eq!(pp.real_constants[0], [1.0, 1.0, 0.0, 0.0]);
    // detail_map = 5 5 0 0 (static animated ScaleUniform=5).
    assert_eq!(pp.real_constants[1], [5.0, 5.0, 0.0, 0.0]);
    // albedo_color = (1, 1, 1, 1) — Color forces alpha=1.
    assert_eq!(pp.real_constants[2], [1.0, 1.0, 1.0, 1.0]);
    // specular_tint = (0.537, 0.541, 0.494, 1) — Color forces alpha=1.
    let st = pp.real_constants[8];
    assert!((st[0] - 0.5372_5490).abs() < 1e-5, "specular_tint R");
    assert!((st[1] - 0.5411_7650).abs() < 1e-5, "specular_tint G");
    assert!((st[2] - 0.4941_1768).abs() < 1e-5, "specular_tint B");
    assert_eq!(st[3], 1.0,                       "specular_tint alpha=1 (Color)");
    // self_illum_color = (0, 1, 0, 1) — green.
    assert_eq!(pp.real_constants[21], [3.0, 3.0, 3.0, 3.0], "self_illum_intensity=3");

    // queryable_properties — tagtool: [-1, 0, -1, -1, 2, -1, -1, -1]
    assert_eq!(pp.runtime_queryable_properties, [-1, 0, -1, -1, 2, -1, -1, -1]);
}

#[test]
fn bake_is_idempotent() {
    let Some(tags) = tags_root() else { return };

    let rmsh_path = tags.join("levels/multi/shrine/sky/shaders/shrine_clouds_sandstorm.shader");
    let mut rmsh = RenderMethod::from_tag(&load_tag(&rmsh_path)).unwrap();
    let rmdf = RenderMethodDefinition::from_tag(&load_tag(
        &tags.join("shaders/shader.render_method_definition"),
    ))
    .unwrap();
    let rmt2 = RenderMethodTemplate::from_tag(&load_tag(&tags.join(
        "shaders/shader_templates/_6_0_0_0_0_0_0_3_0_0_0_0_0.render_method_template",
    )))
    .unwrap();

    rmsh.bake(&rmdf, &rmt2, |p| load_rmop(&tags, p), 0.0).unwrap();
    let first = rmsh.postprocess_definition.clone().unwrap();

    // Second bake call should be a no-op (idempotency guard).
    rmsh.bake(&rmdf, &rmt2, |p| load_rmop(&tags, p), 0.0).unwrap();
    let second = rmsh.postprocess_definition.clone().unwrap();

    assert_eq!(first.real_constants, second.real_constants);
    assert_eq!(first.textures.len(), second.textures.len());
    assert_eq!(first.overlays.len(), second.overlays.len());
}
