//! `particle_model` geometry extraction: split the tag's merged mesh
//! back into per-object JMS files and emit the JMI manifest that ties
//! them together.
//!
//! Output mirrors the layout Tool's `fbx to jmi` writer produces and
//! its `import particle model` reader consumes, so pointing Tool back
//! at the emitted `.jmi` rebuilds the tag:
//!
//! ```text
//! <out>/<stem>/<stem>.jmi
//! <out>/<stem>/<object>/render/<object>.JMS
//! ```
//!
//! `flat` drops the `<stem>/` level, putting the manifest and the
//! object directories directly under `<out>`. The object directories
//! themselves are never flattened — the JMI resolves each line as
//! `<jmi_dir>/<object>/render/<object>.jms`, so collapsing them would
//! break re-import.
//!
//! The manifest extension is lowercase `.jmi` deliberately. Tool
//! decides between "this is a manifest" and "this is a directory of
//! JMS files" with a **case-sensitive** `strncmp(ext, ".jmi", 5)`
//! against the argument string, so a `.JMI` file is silently treated
//! as a directory — which then derives the wrong tag name (Tool
//! appends the leaf again for directory input) and finds no geometry.
//!
//! See [`crate::particle_model`] for the tag-side reconstruction rules.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::particle_model::read_particle_model;
use crate::TagFile;

use super::ExtractError;

/// One file written by [`particle_model_geometry`].
#[derive(Debug)]
pub struct EmittedParticleFile {
    /// Path the file was written to.
    pub path: PathBuf,
    /// Object name for a JMS, `None` for the manifest itself.
    pub object: Option<String>,
    /// Human-readable content stats (e.g. "36 tris, 108 verts").
    pub summary: String,
}

/// Outcome of a particle_model export.
#[derive(Debug, Default)]
pub struct ParticleModelSummary {
    /// Files written — the manifest first, then one JMS per object.
    pub emitted: Vec<EmittedParticleFile>,
    /// Non-fatal notes (empty objects skipped, names synthesized, …).
    pub warnings: Vec<String>,
    /// `false` when object names had to be invented because the tag
    /// stores none (every gen3 `pmdf` tag).
    pub names_are_authentic: bool,
}

/// Extract `tag`'s particle geometry into the nested source-tree
/// layout `<out_root>/<stem>/`.
pub fn particle_model_to_dir(
    tag: &TagFile,
    out_root: &Path,
    stem: &str,
) -> Result<ParticleModelSummary, ExtractError> {
    particle_model_geometry(tag, out_root, stem, false)
}

/// Extract `tag`'s particle geometry. `flat` drops the `<stem>/`
/// nesting level (see module docs).
pub fn particle_model_geometry(
    tag: &TagFile,
    out_root: &Path,
    stem: &str,
    flat: bool,
) -> Result<ParticleModelSummary, ExtractError> {
    let source = read_particle_model(tag, stem)?;

    let mut summary = ParticleModelSummary {
        names_are_authentic: source.names_are_authentic(),
        ..Default::default()
    };
    if source.objects.is_empty() {
        return Err(ExtractError::msg(
            "particle_model has no reconstructable objects — no index ranges and no geometry",
        ));
    }
    if !summary.names_are_authentic {
        summary.warnings.push(format!(
            "this engine stores no object names — {} name(s) synthesized from the tag stem",
            source.objects.len(),
        ));
    }

    let root = if flat { out_root.to_path_buf() } else { out_root.join(stem) };

    // Manifest first: it names the directories written below it, so a
    // reader landing on a partial extraction still sees what was meant.
    let jmi_path = root.join(format!("{stem}.jmi"));
    write_to(&jmi_path, |w| Ok(source.jmi.write(w)?))?;
    summary.emitted.push(EmittedParticleFile {
        path: jmi_path,
        object: None,
        summary: format!("JMI v{}, {} objects", source.jmi.version, source.jmi.objects.len()),
    });

    let jms_version = crate::game::Game::of(tag).jms_version();
    for object in &source.objects {
        if object.jms.triangles.is_empty() {
            summary
                .warnings
                .push(format!("object `{}` decoded to zero triangles", object.name));
        }
        let path = root
            .join(&object.name)
            .join("render")
            .join(format!("{}.JMS", object.name));
        write_to(&path, |w| Ok(object.jms.write(w, jms_version)?))?;
        summary.emitted.push(EmittedParticleFile {
            path,
            object: Some(object.name.clone()),
            summary: format!(
                "{} tris, {} verts",
                object.jms.triangles.len(),
                object.jms.vertices.len(),
            ),
        });
    }

    Ok(summary)
}

fn write_to<F>(path: &Path, f: F) -> Result<(), ExtractError>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<(), Box<dyn std::error::Error>>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut writer = BufWriter::new(File::create(path)?);
    f(&mut writer).map_err(|e| ExtractError::msg(e.to_string()))?;
    Ok(())
}
