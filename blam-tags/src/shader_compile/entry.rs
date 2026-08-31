//! The rasterizer's shared name tables, read verbatim out of `tool.exe`'s
//! `.rdata` (the pointer arrays `off_14185F460` … `off_14185F940`). These are
//! the exact strings the engine hands to `D3DCompile` and injects as `#define`
//! values, so a byte-matching compile must use them unchanged rather than
//! recompute a name from a base.
//!
//! Contract reference: h3lm `docs/SHADER_COMPILE.md` §5.

/// Shader stage. The numeric value is the engine's own index into the profile
/// and stage-macro tables (`sub_140C574F0` arg `a3`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Vertex = 0,
    Pixel = 1,
    Compute = 2,
}

impl Stage {
    pub fn index(self) -> usize {
        self as usize
    }
    /// `off_14185F6C0` — the `D3DCompile` target profile. Always Shader Model 5.
    pub fn profile(self) -> &'static str {
        ["vs_5_0", "ps_5_0", "cs_5_0"][self.index()]
    }
    /// `off_14185F940` — the stage-select `#define` (set to `"1"`).
    pub fn stage_macro(self) -> &'static str {
        ["VERTEX_SHADER", "PIXEL_SHADER", "COMPUTE_SHADER"][self.index()]
    }
    /// The entry-point name table for this stage.
    fn name_table(self) -> &'static [&'static str; K_NUMBER_OF_ENTRY_POINTS] {
        match self {
            Stage::Vertex => &VS_ENTRY_NAMES,
            Stage::Pixel => &PS_ENTRY_NAMES,
            Stage::Compute => &CS_ENTRY_NAMES,
        }
    }
    /// The entry-point function name passed to `D3DCompile` for `(stage, entry)`.
    /// `None` if `entry` is out of range.
    pub fn entry_name(self, entry: usize) -> Option<&'static str> {
        self.name_table().get(entry).copied()
    }
}

pub const K_NUMBER_OF_ENTRY_POINTS: usize = 18;
pub const K_NUMBER_OF_VERTEX_TYPES: usize = 22;

/// `entry_points_flags` — the 18-entry enum, index = entry-point ordinal. These
/// are the `long_flags` bit names in the `pixel_shader`/`vertex_shader` tags and
/// the ordering the driver iterates.
pub const ENTRY_POINTS: [&str; K_NUMBER_OF_ENTRY_POINTS] = [
    "default",
    "albedo",
    "static_default",
    "static_per_pixel",
    "static_per_vertex",
    "static_sh",
    "static_prt_ambient",
    "static_prt_linear",
    "static_prt_quadratic",
    "dynamic_light",
    "shadow_generate",
    "shadow_apply",
    "active_camo",
    "lightmap_debug_mode",
    "static_per_vertex_color",
    "water_tessellation",
    "water_shading",
    "dynamic_light_cinematic",
];

/// `off_14185F750` — the value of the `entry_point` `#define`. Differs from the
/// `D3DCompile` entry name (e.g. entry 17 is `dynamic_light_cinematic` here but
/// `dynamic_light_cine_*` as a function name).
pub const ENTRY_POINT_MACRO: [&str; K_NUMBER_OF_ENTRY_POINTS] = ENTRY_POINTS;

/// `off_14185F460` — pixel entry-point function names. Note the three PRT
/// entries all share `static_prt_ps`.
pub const PS_ENTRY_NAMES: [&str; K_NUMBER_OF_ENTRY_POINTS] = [
    "default_ps",
    "albedo_ps",
    "static_default_ps",
    "static_per_pixel_ps",
    "static_per_vertex_ps",
    "static_sh_ps",
    "static_prt_ps",
    "static_prt_ps",
    "static_prt_ps",
    "dynamic_light_ps",
    "shadow_generate_ps",
    "shadow_apply_ps",
    "active_camo_ps",
    "lightmap_debug_mode_ps",
    "static_per_vertex_color_ps",
    "water_tessellation_ps",
    "water_shading_ps",
    "dynamic_light_cine_ps",
];

/// `off_14185F4F0` — vertex entry-point function names.
pub const VS_ENTRY_NAMES: [&str; K_NUMBER_OF_ENTRY_POINTS] = [
    "default_vs",
    "albedo_vs",
    "static_default_vs",
    "static_per_pixel_vs",
    "static_per_vertex_vs",
    "static_sh_vs",
    "static_prt_ambient_vs",
    "static_prt_linear_vs",
    "static_prt_quadratic_vs",
    "dynamic_light_vs",
    "shadow_generate_vs",
    "shadow_apply_vs",
    "active_camo_vs",
    "lightmap_debug_mode_vs",
    "static_per_vertex_color_vs",
    "water_tessellation_vs",
    "water_shading_vs",
    "dynamic_light_cine_vs",
];

/// `off_14185F580` — compute entry-point function names.
pub const CS_ENTRY_NAMES: [&str; K_NUMBER_OF_ENTRY_POINTS] = [
    "default_cs",
    "albedo_cs",
    "static_default_cs",
    "static_per_pixel_cs",
    "static_per_vertex_cs",
    "static_sh_cs",
    "static_prt_ambient_cs",
    "static_prt_linear_cs",
    "static_prt_quadratic_cs",
    "dynamic_light_cs",
    "shadow_generate_cs",
    "shadow_apply_cs",
    "active_camo_cs",
    "lightmap_debug_mode_cs",
    "static_per_vertex_color_cs",
    "water_tessellation_cs",
    "water_shading_cs",
    "dynamic_light_cine_cs",
];

/// `off_14185F7E0` — the value of the `vertex_type` `#define`, index = vertex
/// type ordinal.
pub const VERTEX_TYPE_MACRO: [&str; K_NUMBER_OF_VERTEX_TYPES] = [
    "s_world_vertex",
    "s_rigid_vertex",
    "s_skinned_vertex",
    "s_particle_model_vertex",
    "s_flat_world_vertex",
    "s_flat_rigid_vertex",
    "s_flat_skinned_vertex",
    "s_screen_vertex",
    "s_debug_vertex",
    "s_transparent_vertex",
    "s_particle_vertex",
    "s_contrail_vertex",
    "s_light_volume_vertex",
    "s_chud_vertex_simple",
    "s_chud_vertex_fancy",
    "s_decorator_vertex",
    "s_tiny_position_vertex",
    "s_patchy_fog_vertex",
    "s_water_vertex",
    "s_ripple_vertex",
    "s_implicit_vertex",
    "s_beam_vertex",
];

/// `off_14185F890` — the value of the `deform` `#define`, index = vertex type.
/// Note index 19 (`s_ripple_vertex`) maps to `deform_vertex`, not
/// `deform_ripple` — read verbatim from the table.
pub const DEFORM_MACRO: [&str; K_NUMBER_OF_VERTEX_TYPES] = [
    "deform_world",
    "deform_rigid",
    "deform_skinned",
    "deform_particle_model",
    "deform_flat_world",
    "deform_flat_rigid",
    "deform_flat_skinned",
    "deform_screen",
    "deform_debug",
    "deform_transparent",
    "deform_particle",
    "deform_contrail",
    "deform_light_volume",
    "deform_chud_simple",
    "deform_chud_fancy",
    "deform_decorator",
    "deform_tiny_position",
    "deform_patchy_fog",
    "deform_water",
    "deform_vertex",
    "deform_implicit",
    "deform_beam",
];

/// Look up a vertex type ordinal by its `s_*_vertex` name (used to resolve a
/// raw-`.hlsl` `@generate <name>` directive). Accepts either the full
/// `s_world_vertex` or the short `world` form.
pub fn vertex_type_by_name(name: &str) -> Option<usize> {
    let want = name.trim();
    VERTEX_TYPE_MACRO.iter().position(|v| {
        *v == want || v.strip_prefix("s_").and_then(|s| s.strip_suffix("_vertex")) == Some(want)
    })
}

/// Look up an entry-point ordinal by its base name (`@entry <name>` directive,
/// or a render_method_definition entry).
pub fn entry_by_name(name: &str) -> Option<usize> {
    let want = name.trim();
    ENTRY_POINTS.iter().position(|e| *e == want)
}
