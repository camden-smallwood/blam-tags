//! Filesystem helpers for tag-reference resolution. Halo stores
//! cross-tag links as backslash-separated relative paths
//! (`objects\characters\masterchief`); turning one into a real
//! `PathBuf` needs (a) the `tags/` ancestor of the input as a root
//! and (b) the target group's extension appended on the end.

use std::path::{Path, PathBuf};

use crate::fields::TagFieldData;
use crate::TagStruct;

/// Find the `tags/` ancestor of `path` and return everything up to
/// and including it. Returns `None` if `path` doesn't canonicalize
/// or no `tags/` component is found.
pub fn derive_tags_root(path: &Path) -> Option<PathBuf> {
    let abs = path.canonicalize().ok()?;
    let mut acc = PathBuf::new();
    let mut found = None;
    for component in abs.components() {
        acc.push(component);
        if matches!(component, std::path::Component::Normal(s) if s == "tags") {
            found = Some(acc.clone());
        }
    }
    found
}

/// Extract a tag file's stem (filename without extension) for
/// output-path construction. Falls back to `default` for paths
/// without a usable stem.
pub fn tag_stem(path: &Path, default: &str) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or(default).to_owned()
}

/// Read a `tag_reference` field's relative path, dropping null/empty
/// references. Accepts a `/`-separated path so it transparently
/// handles tag-reference fields nested inside inherited parent
/// structs (e.g. `object/model` on a `.crate` whose `obje` parent is
/// inlined as a named substruct).
pub fn tag_ref_path(s: &TagStruct<'_>, path: &str) -> Option<String> {
    let v = s.field_path(path)?.value()?;
    let TagFieldData::TagReference(r) = v else { return None; };
    let (_, p) = r.group_tag_and_name?;
    if p.is_empty() { None } else { Some(p) }
}

/// Resolve a Halo-style relative tag path (`objects\foo\bar`) against
/// an absolute `tags_root` and stamp on the target group's extension.
///
/// Always APPENDS `.{ext}` to the filename — never replaces an existing
/// dot-suffix. Authoring names can contain literal dots (e.g.
/// `decal_road_stripe_short_1.bitmap`) and the engine's tag-ref
/// resolver appends the group extension without stripping, producing
/// on-disk files like `..._1.bitmap.decal_system`. Using
/// `Path::set_extension` here would strip the `.bitmap` portion and
/// miss those files.
pub fn resolve_tag_path(tags_root: &Path, rel: &str, ext: &str) -> PathBuf {
    let rel_path: PathBuf = rel.split('\\').collect();
    let p = tags_root.join(&rel_path);
    let mut s = p.into_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// Map a tag group FOURCC (big-endian u32 such as `rmtr`) to the
/// matching on-disk file extension. Returns `None` for unknown
/// groups — caller falls back to a sensible default (typically
/// `"shader"` for the render-method family).
///
/// Halo's tag-ref payload stores the group FOURCC alongside the
/// path, but the path itself is extension-less. Composing the right
/// extension is essential for non-`rmsh` shader chains (terrain,
/// water, foliage, decal, etc.). Source: MCC tag dump filenames.
pub fn group_tag_to_extension(group: u32) -> Option<&'static str> {
    let fourcc = group.to_be_bytes();
    Some(match &fourcc {
        // render-method family
        b"rmsh" => "shader",
        b"rmtr" => "shader_terrain",
        b"rmw " => "shader_water",
        b"rmfl" => "shader_foliage",
        b"rmd " => "shader_decal",
        b"rmhg" => "shader_halogram",
        b"rmsk" => "shader_skin",
        b"rmct" => "shader_cortana",
        b"rmcs" => "shader_custom",
        b"rmp " => "shader_particle",
        b"rmb " => "shader_beam",
        b"rmco" => "shader_contrail",
        b"rmlv" => "shader_light_volume",
        // render-method definitions
        b"rmdf" => "render_method_definition",
        b"rmop" => "render_method_option",
        b"rmt2" => "render_method_template",
        // common other groups (extend as needed)
        b"bitm" => "bitmap",
        b"mode" => "render_model",
        b"hlmt" => "model",
        b"jmad" => "model_animation_graph",
        b"sbsp" => "scenario_structure_bsp",
        b"scnr" => "scenario",
        b"Lbsp" => "scenario_lightmap_bsp_data",
        b"skya" => "sky_atm_parameters",
        b"decs" => "decal_system",
        b"cfxs" => "camera_fx_settings",
        b"rasg" => "rasterizer_globals",
        b"obje" => "object",
        b"bipd" => "biped",
        b"vehi" => "vehicle",
        b"weap" => "weapon",
        b"eqip" => "equipment",
        b"ssce" => "sound_scenery",
        b"scen" => "scenery",
        // `.crate` extension uses two FOURCCs across Halo versions:
        //   `bloc` — H3 MCC (verified via `definitions/halo3_mcc/crate.json:3`)
        //   `crat` — Reach / older builds
        // Keep both so either resolves to the right on-disk extension.
        b"bloc" => "crate",
        b"crat" => "crate",
        b"mach" => "device_machine",
        b"ctrl" => "device_control",
        b"term" => "device_terminal",
        b"proj" => "projectile",
        b"crea" => "creature",
        b"gint" => "giant",
        b"efsc" => "effect_scenery",
        b"effe" => "effect",
        _ => return None,
    })
}
