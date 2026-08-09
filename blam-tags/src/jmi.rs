//! JMI — the multi-object manifest Halo's `import particle model`
//! consumes.
//!
//! A JMI carries no geometry of its own. It is a short plain-text
//! index naming N sibling object directories; the actual meshes live
//! in `<jmi_dir>/<object>/render/<object>.jms`. Tool's `fbx to jmi`
//! writer emits exactly:
//!
//! ```text
//! ;### VERSION NUMBER ###
//! 8213
//!
//! ;### TOTAL OBJECTS ###
//! 2
//!
//! can_1
//! can_2
//! ```
//!
//! with CRLF line endings throughout. Lines beginning with `;` are
//! comments; the reader consumes whitespace-delimited records and uses
//! the label text only for diagnostics.
//!
//! Importing one JMI produces ONE `particle_model` tag: every listed
//! object is merged into a single mesh, and the per-object index
//! ranges are recorded in the tag's `m_gpu_data/m_variants` (gen3) or
//! `models[]` (Halo 2). See [`crate::particle_model`] for the reverse
//! direction.
//!
//! Reference: `tool.exe` (Halo 3 MCC) — writer at `sub_140081030`
//! (`**** Writing JMI`), reader at `sub_1401AD920`
//! (`unknown JMI version (%i)`), driven by
//! `c:\mcc\release\h3\source\tool\import_particle_model.cpp`.

use std::io::{self, Write};

/// The version Tool's JMI writer emits.
pub const JMI_VERSION: u16 = 8213;

/// Lowest version Tool's JMI reader accepts. The reader rejects
/// anything `<= 8207` with `unknown JMI version (%i)`.
pub const JMI_MIN_VERSION: u16 = 8208;

#[derive(Debug)]
pub enum JmiError {
    Io(io::Error),
    /// Version record missing or not an integer.
    MissingVersion,
    /// Version present but below what Tool's reader accepts.
    UnknownVersion(i64),
    /// Object-count record missing or not an integer.
    MissingCount,
    /// Header declared `expected` objects but only `found` names followed.
    CountMismatch { expected: usize, found: usize },
}

impl std::fmt::Display for JmiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::MissingVersion => write!(f, "JMI has no VERSION NUMBER record"),
            Self::UnknownVersion(v) => write!(
                f, "unknown JMI version ({v}) — Tool accepts {JMI_MIN_VERSION} and above",
            ),
            Self::MissingCount => write!(f, "JMI has no TOTAL OBJECTS record"),
            Self::CountMismatch { expected, found } => write!(
                f, "JMI declares {expected} objects but lists {found}",
            ),
        }
    }
}

impl std::error::Error for JmiError {}

impl From<io::Error> for JmiError {
    fn from(e: io::Error) -> Self { Self::Io(e) }
}

/// A parsed / buildable JMI manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmiFile {
    pub version: u16,
    /// Object directory names, in listed order. Each names a sibling
    /// directory of the JMI holding `render/<name>.jms`.
    pub objects: Vec<String>,
}

impl JmiFile {
    /// Build a manifest at the version Tool writes.
    pub fn new(objects: Vec<String>) -> Self {
        Self { version: JMI_VERSION, objects }
    }

    /// Relative path (forward-slashed) of the JMS backing object `i`,
    /// as resolved against the directory holding the JMI.
    pub fn object_jms_path(&self, index: usize) -> Option<String> {
        let name = self.objects.get(index)?;
        Some(format!("{name}/render/{name}.jms"))
    }

    /// Write the manifest in Tool's exact byte layout — CRLF endings,
    /// `;###`-delimited section labels, blank line after each record.
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), JmiError> {
        write!(w, ";### VERSION NUMBER ###\r\n")?;
        write!(w, "{}\r\n", self.version)?;
        write!(w, "\r\n")?;
        write!(w, ";### TOTAL OBJECTS ###\r\n")?;
        write!(w, "{}\r\n", self.objects.len())?;
        write!(w, "\r\n")?;
        for name in &self.objects {
            write!(w, "{name}\r\n")?;
        }
        Ok(())
    }

    /// Parse a JMI, applying the same acceptance rule as Tool's reader
    /// (version must be >= [`JMI_MIN_VERSION`]).
    ///
    /// Comment lines (`;`-prefixed) and blank lines are skipped. The
    /// first two surviving lines are the version and object count; the
    /// next `count` lines are object names. Trailing lines beyond the
    /// declared count are ignored, matching the reader's fixed-trip loop.
    pub fn parse(text: &str) -> Result<Self, JmiError> {
        let mut records = text
            .lines()
            .map(|l| l.trim_end_matches('\r').trim())
            .filter(|l| !l.is_empty() && !l.starts_with(';'));

        let version: i64 = records
            .next()
            .and_then(|l| l.parse().ok())
            .ok_or(JmiError::MissingVersion)?;
        if version < JMI_MIN_VERSION as i64 {
            return Err(JmiError::UnknownVersion(version));
        }

        let count: usize = records
            .next()
            .and_then(|l| l.parse().ok())
            .ok_or(JmiError::MissingCount)?;

        let objects: Vec<String> = records.take(count).map(str::to_owned).collect();
        if objects.len() != count {
            return Err(JmiError::CountMismatch { expected: count, found: objects.len() });
        }
        Ok(Self { version: version as u16, objects })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes Tool's writer produces, down to CRLF and the
    /// blank line after each record.
    #[test]
    fn writes_tool_byte_layout() {
        let jmi = JmiFile::new(vec!["can_1".into(), "can_2".into()]);
        let mut out = Vec::new();
        jmi.write(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            ";### VERSION NUMBER ###\r\n8213\r\n\r\n\
             ;### TOTAL OBJECTS ###\r\n2\r\n\r\n\
             can_1\r\ncan_2\r\n",
        );
    }

    #[test]
    fn round_trips() {
        let jmi = JmiFile::new(vec!["alder_1".into(), "alder_2".into(), "alder_3".into()]);
        let mut out = Vec::new();
        jmi.write(&mut out).unwrap();
        let back = JmiFile::parse(&String::from_utf8(out).unwrap()).unwrap();
        assert_eq!(back, jmi);
    }

    #[test]
    fn empty_manifest_round_trips() {
        let jmi = JmiFile::new(Vec::new());
        let mut out = Vec::new();
        jmi.write(&mut out).unwrap();
        assert_eq!(JmiFile::parse(&String::from_utf8(out).unwrap()).unwrap(), jmi);
    }

    /// Tool rejects <= 8207; 8208 is the first accepted version.
    #[test]
    fn rejects_versions_tool_rejects() {
        let below = ";### VERSION NUMBER ###\r\n8207\r\n\r\n1\r\n\r\nfoo\r\n";
        assert!(matches!(JmiFile::parse(below), Err(JmiError::UnknownVersion(8207))));
        let at_floor = ";### VERSION NUMBER ###\r\n8208\r\n\r\n1\r\n\r\nfoo\r\n";
        assert_eq!(JmiFile::parse(at_floor).unwrap().objects, vec!["foo".to_owned()]);
    }

    /// A truncated list must fail rather than silently import fewer
    /// objects than the header promises.
    #[test]
    fn detects_truncated_object_list() {
        let text = "8213\r\n3\r\nfoo\r\nbar\r\n";
        assert!(matches!(
            JmiFile::parse(text),
            Err(JmiError::CountMismatch { expected: 3, found: 2 }),
        ));
    }

    /// LF-only input and interleaved comments still parse — the reader
    /// is whitespace/comment tolerant even though the writer is strict.
    #[test]
    fn tolerates_lf_and_stray_comments() {
        let text = ";header\n8213\n;count follows\n2\n\nfirst\n;mid-list comment\nsecond\n";
        let jmi = JmiFile::parse(text).unwrap();
        assert_eq!(jmi.objects, vec!["first".to_owned(), "second".to_owned()]);
    }

    #[test]
    fn object_jms_path_matches_tool_layout() {
        let jmi = JmiFile::new(vec!["can_1".into()]);
        assert_eq!(jmi.object_jms_path(0).unwrap(), "can_1/render/can_1.jms");
        assert!(jmi.object_jms_path(1).is_none());
    }
}
