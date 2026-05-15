//! Round-trip a Halo 3 MCC `rm**` (render_method-family) tag through
//! `blam_tags::render_method` and print the parsed structure. Used as
//! a smoke test that the type definitions and parsers match the
//! schema.
//!
//! Usage:
//!   cargo run --example dump_render_method -- <path/to/tag>

use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::render_method::{
    ParameterSource, RenderMethod, RenderMethodDefinition, RenderMethodOption,
    RenderMethodTemplate, ResolvedRenderMethod, ResolvedValue,
};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dump_render_method <path/to/tag>");
        std::process::exit(2);
    };

    let tag = TagFile::read(&path).unwrap_or_else(|e| {
        eprintln!("failed to read {path}: {e}");
        std::process::exit(1);
    });

    let group_be = tag.group().tag.to_be_bytes();
    let group = std::str::from_utf8(&group_be).unwrap_or("????");
    println!("group: {group:?}");

    match &group_be {
        b"rmsh" | b"rm  " | b"rmtr" | b"rmw " | b"rmfl" | b"rmd " | b"rmhg"
        | b"rmsk" | b"rmct" | b"rmcs" | b"rmp " | b"rmb " | b"rmco" | b"rmlv" => {
            let rm = RenderMethod::from_tag(&tag).expect("RenderMethod::from_tag failed");
            dump_render_method(&rm);

            // Walker smoke-test: load the rmdf + every active rmop and
            // resolve all parameters to a flat map. Skipped when the
            // rmdf can't be located (missing tags root, etc.).
            if let Some(tag_root) = guess_tag_root(&path) {
                if let Some(rmdf) = load_rmdf(&tag_root, &rm.definition_path) {
                    println!();
                    println!("================ RESOLVED ================");
                    let resolved = ResolvedRenderMethod::resolve(
                        &rm,
                        &rmdf,
                        |opt_path| load_rmop(&tag_root, opt_path),
                    );
                    dump_resolved(&resolved);
                } else {
                    eprintln!("(skipping walker — rmdf {:?} not found under {:?})",
                        rm.definition_path, tag_root);
                }
            }
        }
        b"rmdf" => {
            let rmdf = RenderMethodDefinition::from_tag(&tag)
                .expect("RenderMethodDefinition::from_tag failed");
            dump_rmdf(&rmdf);
        }
        b"rmop" => {
            let rmop = RenderMethodOption::from_tag(&tag).expect("RenderMethodOption::from_tag failed");
            dump_rmop(&rmop);
        }
        b"rmt2" => {
            let rmt2 = RenderMethodTemplate::from_tag(&tag)
                .expect("RenderMethodTemplate::from_tag failed");
            dump_rmt2(&rmt2);
        }
        _ => {
            eprintln!("unsupported tag group: {group:?}");
            std::process::exit(2);
        }
    }
}

fn dump_render_method(rm: &RenderMethod) {
    println!("definition:                {}", rm.definition_path);
    println!("options:                   {:?}", rm.options);
    println!("flags:                     0x{:04x}", rm.flags);
    println!("sort_layer:                {}", rm.sort_layer);
    println!("runtime_flags:             0x{:02x}", rm.runtime_flags);
    println!("custom_fog_setting_index:  {}", rm.custom_fog_setting_index);
    println!("prediction_atom_index:     {}", rm.prediction_atom_index);
    println!("parameters: [{}]", rm.parameters.len());
    for (i, p) in rm.parameters.iter().enumerate() {
        println!(
            "  [{i}] {:24} type={:?} bitmap={:?} real={} int={} animated=[{}]",
            p.parameter_name,
            p.parameter_type,
            short_path(&p.bitmap_path),
            p.real_parameter,
            p.int_parameter,
            p.animated_parameters.len(),
        );
    }
    match &rm.postprocess_definition {
        None => println!("postprocess_definition: <empty>"),
        Some(pp) => {
            println!("postprocess_definition:");
            println!("  template:           {}", pp.template_path);
            println!("  textures:           [{}]", pp.textures.len());
            for (i, t) in pp.textures.iter().enumerate() {
                println!(
                    "    [{i}] bitmap={:?} idx={} addr={} filter={} extern={}",
                    short_path(&t.bitmap_path),
                    t.bitmap_index,
                    t.address_mode,
                    t.filter_mode,
                    t.extern_texture_mode,
                );
            }
            println!("  real_constants:     [{}]", pp.real_constants.len());
            for (i, v) in pp.real_constants.iter().enumerate() {
                println!("    [{i}] {:?}", v);
            }
            println!("  int_constants:      [{}]", pp.int_constants.len());
            println!("  bool_constants:     0x{:08x}", pp.bool_constants);
            println!("  entry_points:       [{}]", pp.entry_points.len());
            for (i, ep) in pp.entry_points.iter().enumerate() {
                println!("    [{i}] start={} count={}", ep.start(), ep.count());
            }
            println!("  passes:             [{}]", pp.passes.len());
            for (i, ps) in pp.passes.iter().enumerate() {
                println!(
                    "    [{i}] bitmaps={}/{} vs_real={}/{} ps_real={}/{}",
                    ps.bitmaps.start(), ps.bitmaps.count(),
                    ps.vertex_real_constants.start(), ps.vertex_real_constants.count(),
                    ps.pixel_real_constants.start(), ps.pixel_real_constants.count(),
                );
            }
            println!("  routing_info:       [{}]", pp.routing_info.len());
            for (i, r) in pp.routing_info.iter().enumerate() {
                println!(
                    "    [{i}] dst={} src={} flags=0x{:02x}",
                    r.destination_index, r.source_index, r.type_specific,
                );
            }
            println!("  overlays:           [{}]", pp.overlays.len());
            println!("  blend_mode:         {}", pp.blend_mode);
            println!("  flags:              0x{:08x}", pp.flags);
        }
    }
}

fn dump_rmdf(rmdf: &RenderMethodDefinition) {
    println!("global_options:        {}", rmdf.global_options_path);
    println!("shared_pixel_shaders:  {}", rmdf.shared_pixel_shaders_path);
    println!("shared_vertex_shaders: {}", rmdf.shared_vertex_shaders_path);
    println!("flags: 0x{:08x}  version: {}", rmdf.flags, rmdf.version);
    println!("categories: [{}]", rmdf.categories.len());
    for (ci, c) in rmdf.categories.iter().enumerate() {
        println!(
            "  [{ci}] {:20} ps={:?} vs={:?} options=[{}]",
            c.category_name, c.pixel_function, c.vertex_function, c.options.len()
        );
        for (oi, o) in c.options.iter().enumerate() {
            println!(
                "    [{oi}] {:30} -> {}",
                o.option_name, o.option_path
            );
        }
    }
}

fn dump_rmop(rmop: &RenderMethodOption) {
    println!("parameters: [{}]", rmop.parameters.len());
    for (i, p) in rmop.parameters.iter().enumerate() {
        println!(
            "  [{i}] {:30} type={:?} extern={:?} default_real={} default_int={} default_color=0x{:08x} bitmap={:?}",
            p.parameter_name,
            p.parameter_type,
            p.source_extern,
            p.default_real_value,
            p.default_int_bool_value,
            p.default_color.0,
            short_path(&p.default_bitmap_path),
        );
    }
}

fn dump_rmt2(rmt2: &RenderMethodTemplate) {
    println!("vertex_shader: {}", rmt2.vertex_shader_path);
    println!("pixel_shader:  {}", rmt2.pixel_shader_path);
    println!("available_entry_points: 0x{:08x}", rmt2.available_entry_points);
    println!("entry_points: [{}]", rmt2.entry_points.len());
    for (i, ep) in rmt2.entry_points.iter().enumerate() {
        println!("  [{i}] start={} count={}", ep.start(), ep.count());
    }
    println!("passes: [{}]", rmt2.passes.len());
    for (i, p) in rmt2.passes.iter().enumerate() {
        println!(
            "  [{i}] tex={}/{} vs_r={}/{} ps_r={}/{} ps_i={}/{} ps_b={}/{} ext_tex={}/{} ext_ps_r={}/{} ps_size={} vs_size={} blend={}",
            p.bitmaps.start(), p.bitmaps.count(),
            p.vertex_real_constants.start(), p.vertex_real_constants.count(),
            p.pixel_real_constants.start(), p.pixel_real_constants.count(),
            p.pixel_int_constants.start(), p.pixel_int_constants.count(),
            p.pixel_bool_constants.start(), p.pixel_bool_constants.count(),
            p.extern_bitmaps.start(), p.extern_bitmaps.count(),
            p.extern_pixel_real_constants.start(), p.extern_pixel_real_constants.count(),
            p.pixel_parameters_size, p.vertex_parameters_size, p.alpha_blend_mode,
        );
    }
    println!("routing_info: [{}]", rmt2.routing_info.len());
    for (i, r) in rmt2.routing_info.iter().enumerate() {
        println!(
            "  [{i}] dst={} src={} flags=0x{:02x}",
            r.destination_index, r.source_index, r.type_specific
        );
    }
    println!("float_constants: {:?}", rmt2.float_constants);
    println!("int_constants:   {:?}", rmt2.int_constants);
    println!("bool_constants:  {:?}", rmt2.bool_constants);
    println!("textures:        {:?}", rmt2.textures);
}

fn short_path(p: &str) -> &str {
    p.rsplit(['\\', '/']).next().unwrap_or(p)
}

// ---- Walker support: resolve tag-relative paths against a tags root ----

/// Walk up the input path looking for a `tags/` directory ancestor.
/// Returns the path INCLUDING `tags` so callers can join Halo-relative
/// `shaders\foo` strings directly.
fn guess_tag_root(p: &str) -> Option<PathBuf> {
    let mut cur = PathBuf::from(p);
    while cur.pop() {
        if cur.file_name().is_some_and(|n| n == "tags") {
            return Some(cur);
        }
    }
    None
}

fn load_rmdf(tags_root: &PathBuf, halo_path: &str) -> Option<RenderMethodDefinition> {
    let abs = tags_root.join(halo_path.replace('\\', "/"))
        .with_extension("render_method_definition");
    let tag = TagFile::read(&abs).ok()?;
    RenderMethodDefinition::from_tag(&tag).ok()
}

fn load_rmop(tags_root: &PathBuf, halo_path: &str) -> Option<RenderMethodOption> {
    let abs = tags_root.join(halo_path.replace('\\', "/"))
        .with_extension("render_method_option");
    let tag = TagFile::read(&abs).ok()?;
    RenderMethodOption::from_tag(&tag).ok()
}

fn dump_resolved(r: &ResolvedRenderMethod) {
    println!("parameters: [{}]", r.parameters.len());
    for p in &r.parameters {
        match &p.source {
            ParameterSource::Extern(ext) => {
                println!("  {:36} {:?} <- extern {:?}", p.name, p.parameter_type, ext);
            }
            ParameterSource::Inline(v) => match v {
                ResolvedValue::Bitmap(b) => println!(
                    "  {:36} Bitmap   {:?} (filter={:?} addr={:?})",
                    p.name, short_path(&b.bitmap_path), b.filter_mode, b.address_mode,
                ),
                ResolvedValue::Color(c) => println!(
                    "  {:36} Color    [{:.3}, {:.3}, {:.3}, {:.3}]",
                    p.name, c[0], c[1], c[2], c[3],
                ),
                ResolvedValue::Real(f)  => println!("  {:36} Real     {}", p.name, f),
                ResolvedValue::Int(i)   => println!("  {:36} Int      {}", p.name, i),
                ResolvedValue::Bool(b)  => println!("  {:36} Bool     {}", p.name, b),
            },
        }
    }
}
