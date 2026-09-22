//! `import <source>` -- build a tag from a JMS or ASS source file.
//!
//! The other direction from `export`: a source file in, a tag out.
//! Which importer runs is decided the way the kit itself decides it, by
//! the folder the source sits in -- `render/`, `collision/`, `physics/`
//! -- with an `.ass` file always a `scenario_structure_bsp`. `--kind`
//! overrides that when a file is somewhere unusual.
//!
//! The tag is serialised and read back before anything reaches disk. A
//! tag that will not parse is cheaper to find out about here than when
//! the kit refuses it.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::context::CliContext;

/// What a source file builds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Render,
    Collision,
    Physics,
    Structure,
}

impl Kind {
    fn group(self) -> &'static str {
        match self {
            Self::Render => "render_model",
            Self::Collision => "collision_model",
            Self::Physics => "physics_model",
            Self::Structure => "scenario_structure_bsp",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "render" | "render_model" | "mode" => Some(Self::Render),
            "collision" | "collision_model" | "coll" => Some(Self::Collision),
            "physics" | "physics_model" | "phmo" => Some(Self::Physics),
            "structure" | "sbsp" | "scenario_structure_bsp" => Some(Self::Structure),
            _ => None,
        }
    }

    /// The kind a path implies: an ASS is always a structure, and a JMS
    /// takes the name of the folder it is filed under.
    fn of_path(path: &Path) -> Option<Self> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.to_ascii_lowercase())
            .unwrap_or_default();
        if name.ends_with(".ass") {
            return Some(Self::Structure);
        }
        for part in path.components() {
            match part.as_os_str().to_string_lossy().to_ascii_lowercase().as_str() {
                "render" => return Some(Self::Render),
                "collision" => return Some(Self::Collision),
                "physics" => return Some(Self::Physics),
                _ => {}
            }
        }
        None
    }
}


pub fn run(
    ctx: &CliContext,
    source: &str,
    output: Option<&str>,
    kind: Option<&str>,
    prt_samples: Option<usize>,
    instanced_geometry: bool,
    force: bool,
) -> Result<()> {
    let game = ctx.require_game("import")?;
    let src = PathBuf::from(source);
    if !src.exists() {
        return Err(anyhow!("no such source file: {}", src.display()));
    }

    let kind = match kind {
        Some(k) => Kind::parse(k).ok_or_else(|| {
            anyhow!("unknown kind {k:?}: expected render, collision, physics or structure")
        })?,
        None => Kind::of_path(&src).ok_or_else(|| {
            anyhow!(
                "cannot tell what {} builds: it is not an .ass and sits in no                  render/collision/physics folder. Say which with --kind.",
                src.display()
            )
        })?,
    };

    let schema = PathBuf::from("definitions").join(game).join(format!("{}.json", kind.group()));
    if !schema.exists() {
        return Err(anyhow!(
            "schema not found: {} (is `definitions/{game}/` present?)",
            schema.display()
        ));
    }

    let out: PathBuf = match output {
        Some(o) => PathBuf::from(o),
        None => src.with_extension(kind.group()),
    };
    if out.exists() && !force {
        return Err(anyhow!("refusing to overwrite {} (pass --force)", out.display()));
    }

    let text = std::fs::read_to_string(&src)
        .with_context(|| format!("failed to read {}", src.display()))?;

    let tag = match kind {
        Kind::Structure => {
            let (ass, version) = blam_tags::ass_parse::parse(&text)
                .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            let opts = blam_tags::sbsp_import::SbspOptions { instanced_geometry };
            let (tag, report) =
                blam_tags::sbsp_import::structure_bsp_from_ass_with(&ass, &schema, opts)
                    .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            println!(
                "ASS v{version}: {} vertices, {} triangles, {} portals, {} collision surfaces",
                report.vertices,
                report.triangles,
                report.portals_written,
                report.collision_surfaces
            );
            for missing in &report.not_written {
                println!("  not written: {missing}");
            }
            tag
        }
        Kind::Render => {
            let (jms, version) = blam_tags::jms::JmsFile::parse(&text)
                .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            let mut opts = blam_tags::render_import::RenderOptions::default();
            if let Some(n) = prt_samples {
                opts.prt_samples = (n > 0).then_some(n);
            }
            let (tag, report) =
                blam_tags::render_import::render_model_from_jms(&jms, &schema, &opts)
                    .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            println!(
                "JMS v{version}: {} meshes, {} regions, {} materials, {} nodes, PRT over {} vertices",
                report.meshes, report.regions, report.materials, report.nodes, report.prt_vertices
            );
            tag
        }
        Kind::Collision => {
            let (jms, version) = blam_tags::jms::JmsFile::parse(&text)
                .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            let (tag, report) = blam_tags::collision_import::collision_model_from_jms(
                &jms,
                &schema,
                &blam_tags::collision_import::CollisionOptions::default(),
            )
            .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            println!(
                "JMS v{version}: {} regions, {} materials, {} surfaces dropped",
                report.regions, report.materials, report.dropped_surfaces
            );
            tag
        }
        Kind::Physics => {
            let (jms, version) = blam_tags::jms::JmsFile::parse(&text)
                .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            let (tag, report) = blam_tags::physics_import::physics_model_from_jms(
                &jms,
                &schema,
                &blam_tags::physics_import::PhysicsOptions::default(),
            )
            .map_err(|e| anyhow!("{}: {e}", src.display()))?;
            println!(
                "JMS v{version}: {} rigid bodies, {} materials, {} regions",
                report.rigid_bodies, report.materials, report.regions
            );
            for skipped in &report.skipped {
                println!("  not written: {skipped}");
            }
            tag
        }
    };

    let bytes = tag
        .write_to_bytes()
        .map_err(|e| anyhow!("the built tag would not serialise: {e}"))?;
    blam_tags::TagFile::read_from_bytes(&bytes)
        .map_err(|e| anyhow!("the built tag would not parse back: {e}"))?;

    if let Some(parent) = out.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    std::fs::write(&out, &bytes)
        .with_context(|| format!("failed to write {}", out.display()))?;
    println!("wrote {} ({} bytes)", out.display(), bytes.len());
    Ok(())
}
