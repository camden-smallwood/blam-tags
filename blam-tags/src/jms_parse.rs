//! JMS text parsing — the inverse of [`JmsFile::write`].
//!
//! # The contract
//!
//! This reader is *value-neutral*. It returns exactly the numbers the
//! file contains, in JMS space: positions in JMS centimetres, `v`
//! unflipped, quaternions as authored. It applies none of the
//! conversions `tool.exe` performs while building a tag (`×0.01` on
//! positions, `v := 1 − v`, the `≤8204` quaternion negation), because
//! [`JmsFile::write`] is the exact mirror of this function and those
//! conversions already happened on the tag→JMS side. `parse` ∘ `write`
//! is the identity on every field this struct models.
//!
//! # Why the parse is positional
//!
//! `tool.exe`'s reader is a bare positional token stream. Section
//! header comments (`;### NODES ###`) are *comments* — it never reads
//! them, and they cannot be trusted. In the shipped H3EK corpus, 181 of
//! 254 files label the CAR WHEEL section `;### HINGES ###`, a
//! copy-paste slip in the exporter; the block underneath still carries
//! the car-wheel fields (`<chassis index>`, `<suspension transform>`,
//! `<gain>`) and sits in the car-wheel slot. A reader that dispatched on
//! the header would mis-parse 71% of the corpus. So: count sections by
//! position, ignore every comment.
//!
//! # Tokenisation
//!
//! Delimiters are **TAB, CR and LF only — never space**. This is not a
//! stylistic choice; it is what the engine does, and the corpus proves
//! it: across all 254 shipped JMS files there are 14,597,684
//! tab-separated numeric lines and zero space-separated ones. Space is
//! an ordinary content byte, which is what lets a material definition
//! line like `(4) base shield` arrive as a single token, and what makes
//! `1.0 2.0 3.0` parse as one value with the other two silently
//! discarded. A token beginning `;`, `/`, `{` or `}` comments out the
//! rest of its line.
//!
//! # Version scope
//!
//! Implements the modern layout, **8205–8213**, which is every version
//! `tool.exe` treats as current and the only one that ships (all 254
//! H3EK source files are 8213). Version gates within that range:
//! vertex colour at ≥8211, the CAR WHEEL / POINT TO POINT / PRISMATIC
//! trio at ≥8210, SKYLIGHT at ≥8212.
//!
//! Versions 8197–8204 are a structurally different format — child /
//! sibling node links, a dedicated REGIONS section, two-influence
//! vertices, per-triangle region indices — and are rejected with
//! [`JmsParseError::UnsupportedVersion`] rather than guessed at. No
//! such file exists in the corpus to test against.
//!
//! # Failure policy
//!
//! Loud, never silent. `tool.exe` repairs bad input in place — it
//! rewrites invalid normals, discards malformed triangles, and rebinds
//! orphaned vertices to node 0 — so a "successful" import there proves
//! nothing. This reader instead reports the section, token and line of
//! the first thing it cannot represent, including any section that is
//! non-empty but has no home in [`JmsFile`]. Panic-free on malformed
//! input, matching the rest of the read path.

use crate::jms::{
    JmsBox, JmsCapsule, JmsConvex, JmsFile, JmsHinge, JmsMarker, JmsMaterial, JmsNode, JmsRagdoll,
    JmsSphere, JmsTriangle, JmsVertex,
};
use crate::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};

/// Lowest JMS version this reader understands. Below this the format is
/// structurally different, not merely narrower.
pub const MIN_VERSION: u16 = 8205;
/// Highest JMS version `tool.exe` accepts.
pub const MAX_VERSION: u16 = 8213;

/// Everything that can go wrong reading a JMS. Each variant carries
/// enough context to point at the offending byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JmsParseError {
    /// The version token is absent or not an integer.
    BadVersion { token: String, line: usize },
    /// A structurally different JMS generation (8197–8204), or a
    /// version `tool.exe` itself would reject.
    UnsupportedVersion(u32),
    /// The token stream ended while a section still wanted fields.
    UnexpectedEof { section: &'static str, wanted: &'static str },
    /// A token that should have been a number wasn't one.
    NotANumber { section: &'static str, wanted: &'static str, token: String, line: usize },
    /// An element count that is negative, or too large for `usize`. A
    /// count merely larger than the file can satisfy is *not* this — that
    /// is truncation, and surfaces as [`Self::UnexpectedEof`] naming the
    /// section that actually ran out.
    BadCount { section: &'static str, count: i64, line: usize },
    /// A section this reader parses but [`JmsFile`] cannot represent
    /// turned out to be non-empty. Refused rather than dropped.
    UnrepresentableSection { section: &'static str, count: usize, line: usize },
    /// Tokens remained after the last section. Almost always means an
    /// earlier count was misread and the whole parse is off.
    TrailingTokens { remaining: usize, line: usize },
}

impl std::fmt::Display for JmsParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadVersion { token, line } => {
                write!(f, "line {line}: expected a JMS version number, found {token:?}")
            }
            Self::UnsupportedVersion(v) => write!(
                f,
                "JMS version {v} is not supported; this reader handles \
                 {MIN_VERSION}-{MAX_VERSION} (the modern layout). Versions 8197-8204 use a \
                 structurally different format"
            ),
            Self::UnexpectedEof { section, wanted } => {
                write!(f, "unexpected end of file: {section} still wanted {wanted}")
            }
            Self::NotANumber { section, wanted, token, line } => {
                write!(f, "line {line}: {section} {wanted} is not a number: {token:?}")
            }
            Self::BadCount { section, count, line } => {
                write!(f, "line {line}: {section} has an impossible element count {count}")
            }
            Self::UnrepresentableSection { section, count, line } => write!(
                f,
                "line {line}: this file has {count} {section} entries, which JmsFile cannot \
                 represent yet; refusing rather than dropping them"
            ),
            Self::TrailingTokens { remaining, line } => write!(
                f,
                "line {line}: {remaining} tokens left over after the last section - an earlier \
                 count was probably misread"
            ),
        }
    }
}

impl std::error::Error for JmsParseError {}

/// One token plus the 1-based line it started on.
#[derive(Clone, Copy)]
struct Tok<'a> {
    /// Borrowed from the source text, not from the token vector, so a
    /// token can be handed out by value without tying up `Reader`.
    text: &'a str,
    line: usize,
}

/// Split a JMS into tokens exactly as `tool.exe` does: delimiters are
/// TAB / CR / LF, space is content, and a token starting `;`, `/`, `{`
/// or `}` comments out the remainder of its line.
fn tokenize(src: &str) -> Vec<Tok<'_>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut line = 1usize;
    let mut skipping = false;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                line += 1;
                skipping = false;
                i += 1;
            }
            b'\r' => {
                skipping = false;
                i += 1;
            }
            b'\t' => i += 1,
            _ => {
                let start = i;
                while i < b.len() && !matches!(b[i], b'\t' | b'\r' | b'\n') {
                    i += 1;
                }
                if !skipping {
                    // Non-empty by construction: we consumed >= 1 byte.
                    let text = &src[start..i];
                    match b[start] {
                        b';' | b'/' | b'{' | b'}' => skipping = true,
                        _ => out.push(Tok { text, line }),
                    }
                }
            }
        }
    }
    out
}

/// Cursor over the token stream with typed, section-labelled reads.
struct Reader<'a> {
    toks: Vec<Tok<'a>>,
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(src: &'a str) -> Self {
        Self { toks: tokenize(src), pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.toks.len().saturating_sub(self.pos)
    }

    /// Line of the token last consumed, for error messages after the fact.
    fn line(&self) -> usize {
        let idx = self.pos.saturating_sub(1).min(self.toks.len().saturating_sub(1));
        self.toks.get(idx).map_or(0, |t| t.line)
    }

    /// Take the next token. Returned by value — `Tok` is two `Copy`
    /// fields borrowed from the source text — so nothing keeps `Reader`
    /// borrowed and the caller can go straight on to the next read.
    fn next(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<Tok<'a>, JmsParseError> {
        let t = *self
            .toks
            .get(self.pos)
            .ok_or(JmsParseError::UnexpectedEof { section, wanted })?;
        self.pos += 1;
        Ok(t)
    }

    fn string(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<String, JmsParseError> {
        Ok(self.next(section, wanted)?.text.to_owned())
    }

    fn i32(&mut self, section: &'static str, wanted: &'static str) -> Result<i32, JmsParseError> {
        let t = self.next(section, wanted)?;
        t.text.trim().parse::<i32>().map_err(|_| JmsParseError::NotANumber {
            section,
            wanted,
            token: t.text.to_owned(),
            line: t.line,
        })
    }

    fn i16(&mut self, section: &'static str, wanted: &'static str) -> Result<i16, JmsParseError> {
        // JMS writes indices as plain integers; the tag side narrows them.
        // Clamp rather than fail so an out-of-range index reaches the
        // caller's validation instead of dying in the lexer.
        Ok(self.i32(section, wanted)?.clamp(i16::MIN as i32, i16::MAX as i32) as i16)
    }

    fn f32(&mut self, section: &'static str, wanted: &'static str) -> Result<f32, JmsParseError> {
        let t = self.next(section, wanted)?;
        t.text.trim().parse::<f32>().map_err(|_| JmsParseError::NotANumber {
            section,
            wanted,
            token: t.text.to_owned(),
            line: t.line,
        })
    }

    /// Read an element count. Rejects negatives, which are always
    /// corruption. Does **not** reject a count larger than the stream can
    /// satisfy: that is truncation, and reporting it as
    /// [`JmsParseError::UnexpectedEof`] from the section that actually ran
    /// out is a far better diagnostic than blaming the count. The loop
    /// bails on the first missing token, so an absurd count costs one
    /// iteration, not `n`.
    fn count(&mut self, section: &'static str) -> Result<usize, JmsParseError> {
        let t = self.next(section, "element count")?;
        let line = t.line;
        let raw = t.text.trim().parse::<i64>().map_err(|_| JmsParseError::NotANumber {
            section,
            wanted: "element count",
            token: t.text.to_owned(),
            line,
        })?;
        if raw < 0 {
            return Err(JmsParseError::BadCount { section, count: raw, line });
        }
        usize::try_from(raw).map_err(|_| JmsParseError::BadCount { section, count: raw, line })
    }

    /// How much to pre-allocate for `n` elements of at least
    /// `min_tokens_each` tokens. Bounded by what is actually left in the
    /// stream, so a corrupt count cannot turn into a huge allocation.
    fn hint(&self, n: usize, min_tokens_each: usize) -> usize {
        n.min(self.remaining() / min_tokens_each.max(1))
    }

    fn quat(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<RealQuaternion, JmsParseError> {
        Ok(RealQuaternion {
            i: self.f32(section, wanted)?,
            j: self.f32(section, wanted)?,
            k: self.f32(section, wanted)?,
            w: self.f32(section, wanted)?,
        })
    }

    fn point3(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<RealPoint3d, JmsParseError> {
        Ok(RealPoint3d {
            x: self.f32(section, wanted)?,
            y: self.f32(section, wanted)?,
            z: self.f32(section, wanted)?,
        })
    }

    fn vector3(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<RealVector3d, JmsParseError> {
        Ok(RealVector3d {
            i: self.f32(section, wanted)?,
            j: self.f32(section, wanted)?,
            k: self.f32(section, wanted)?,
        })
    }

    fn point2(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<RealPoint2d, JmsParseError> {
        Ok(RealPoint2d {
            x: self.f32(section, wanted)?,
            y: self.f32(section, wanted)?,
        })
    }

    /// Read `<rotation ijkw> <translation xyz>` — the 7-float transform
    /// every constraint and primitive shares.
    fn transform(
        &mut self,
        section: &'static str,
        wanted: &'static str,
    ) -> Result<(RealQuaternion, RealPoint3d), JmsParseError> {
        let r = self.quat(section, wanted)?;
        let t = self.point3(section, wanted)?;
        Ok((r, t))
    }

    /// Consume a section this reader parses positionally but cannot
    /// store, refusing if it turns out to carry data. Every one of these
    /// is empty in all 254 shipped H3EK files, so refusing costs nothing
    /// today and prevents silent loss tomorrow.
    fn skip_unrepresentable(&mut self, section: &'static str) -> Result<(), JmsParseError> {
        let n = self.count(section)?;
        if n != 0 {
            return Err(JmsParseError::UnrepresentableSection {
                section,
                count: n,
                line: self.line(),
            });
        }
        Ok(())
    }
}

impl JmsFile {
    /// Parse JMS text. Returns the scene and the format version it
    /// declared.
    ///
    /// Values come back in JMS space, unconverted — see the module docs.
    ///
    /// ```no_run
    /// # use blam_tags::jms::JmsFile;
    /// let text = std::fs::read_to_string("model.JMS").unwrap();
    /// let (jms, version) = JmsFile::parse(&text).unwrap();
    /// assert_eq!(version, 8213);
    /// println!("{} vertices, {} triangles", jms.vertices.len(), jms.triangles.len());
    /// ```
    pub fn parse(text: &str) -> Result<(Self, u16), JmsParseError> {
        let mut r = Reader::new(text);

        // ### VERSION ###
        let vt = r.next("VERSION", "version number")?;
        let version = vt.text.trim().parse::<u32>().map_err(|_| JmsParseError::BadVersion {
            token: vt.text.to_owned(),
            line: vt.line,
        })?;
        if !(MIN_VERSION as u32..=MAX_VERSION as u32).contains(&version) {
            return Err(JmsParseError::UnsupportedVersion(version));
        }
        let version = version as u16;
        let has_vertex_color = version >= 8211;
        let has_extra_constraints = version >= 8210;
        let has_skylight = version >= 8212;

        let mut out = Self::default();

        // ### NODES ###  name, parent, rotation(4), translation(3)
        let n = r.count("NODES")?;
        out.nodes.reserve(r.hint(n, 9));
        for _ in 0..n {
            let name = r.string("NODES", "name")?;
            let parent = r.i16("NODES", "parent node index")?;
            let (rotation, translation) = r.transform("NODES", "default transform")?;
            out.nodes.push(JmsNode { name, parent, rotation, translation });
        }

        // ### MATERIALS ###  name, "(slot) [lod] permutation region"
        let n = r.count("MATERIALS")?;
        out.materials.reserve(r.hint(n, 2));
        for _ in 0..n {
            let name = r.string("MATERIALS", "name")?;
            let material_name = r.string("MATERIALS", "material name")?;
            out.materials.push(JmsMaterial { name, material_name });
        }

        // ### MARKERS ###  name, node, rotation(4), translation(3), radius
        let n = r.count("MARKERS")?;
        out.markers.reserve(r.hint(n, 10));
        for _ in 0..n {
            let name = r.string("MARKERS", "name")?;
            let node_index = r.i16("MARKERS", "node index")?;
            let (rotation, translation) = r.transform("MARKERS", "transform")?;
            let radius = r.f32("MARKERS", "radius")?;
            out.markers.push(JmsMarker { name, node_index, rotation, translation, radius });
        }

        // Neither of these has a home in JmsFile, and both are empty in
        // every shipped file.
        r.skip_unrepresentable("INSTANCE XREF PATHS")?;
        r.skip_unrepresentable("INSTANCE MARKERS")?;

        // ### VERTICES ###
        let min_vertex_tokens = 3 + 3 + 1 + 1 + if has_vertex_color { 3 } else { 0 };
        let n = r.count("VERTICES")?;
        out.vertices.reserve(r.hint(n, min_vertex_tokens));
        for _ in 0..n {
            let position = r.point3("VERTICES", "position")?;
            let normal = r.vector3("VERTICES", "normal")?;

            let influences = r.count("VERTICES")?;
            let mut node_sets = Vec::with_capacity(r.hint(influences, 2));
            for _ in 0..influences {
                let idx = r.i16("VERTICES", "node influence index")?;
                let weight = r.f32("VERTICES", "node influence weight")?;
                node_sets.push((idx, weight));
            }

            let uv_count = r.count("VERTICES")?;
            let mut uvs = Vec::with_capacity(r.hint(uv_count, 2));
            for _ in 0..uv_count {
                uvs.push(r.point2("VERTICES", "texture coordinate")?);
            }

            let color = if has_vertex_color {
                Some(r.point3("VERTICES", "vertex color")?)
            } else {
                None
            };

            out.vertices.push(JmsVertex {
                position,
                normal,
                tangent: None,
                binormal: None,
                node_sets,
                uvs,
                color,
            });
        }

        // ### TRIANGLES ###  material, v0 v1 v2. The modern format folds
        // region into the material label, so `region` stays 0 — same
        // convention the writer uses.
        let n = r.count("TRIANGLES")?;
        out.triangles.reserve(r.hint(n, 4));
        for _ in 0..n {
            let material = r.i32("TRIANGLES", "material index")?;
            let v = [
                r.i32("TRIANGLES", "vertex index")? as u32,
                r.i32("TRIANGLES", "vertex index")? as u32,
                r.i32("TRIANGLES", "vertex index")? as u32,
            ];
            out.triangles.push(JmsTriangle { material, v, region: 0 });
        }

        // ### SPHERES ###
        let n = r.count("SPHERES")?;
        out.spheres.reserve(r.hint(n, 11));
        for _ in 0..n {
            let name = r.string("SPHERES", "name")?;
            let parent = r.i32("SPHERES", "parent")?;
            let material = r.i32("SPHERES", "material")?;
            let (rotation, translation) = r.transform("SPHERES", "transform")?;
            let radius = r.f32("SPHERES", "radius")?;
            out.spheres.push(JmsSphere { name, parent, material, rotation, translation, radius });
        }

        // ### BOXES ###  extents are FULL, not half.
        let n = r.count("BOXES")?;
        out.boxes.reserve(r.hint(n, 13));
        for _ in 0..n {
            let name = r.string("BOXES", "name")?;
            let parent = r.i32("BOXES", "parent")?;
            let material = r.i32("BOXES", "material")?;
            let (rotation, translation) = r.transform("BOXES", "transform")?;
            let width = r.f32("BOXES", "width")?;
            let length = r.f32("BOXES", "length")?;
            let height = r.f32("BOXES", "height")?;
            out.boxes.push(JmsBox {
                name, parent, material, rotation, translation, width, length, height,
            });
        }

        // ### CAPSULES ###  Halo calls these pills.
        let n = r.count("CAPSULES")?;
        out.capsules.reserve(r.hint(n, 12));
        for _ in 0..n {
            let name = r.string("CAPSULES", "name")?;
            let parent = r.i32("CAPSULES", "parent")?;
            let material = r.i32("CAPSULES", "material")?;
            let (rotation, translation) = r.transform("CAPSULES", "transform")?;
            let height = r.f32("CAPSULES", "height")?;
            let radius = r.f32("CAPSULES", "radius")?;
            out.capsules.push(JmsCapsule {
                name, parent, material, rotation, translation, height, radius,
            });
        }

        // ### CONVEX SHAPES ###
        let n = r.count("CONVEX SHAPES")?;
        out.convex_shapes.reserve(r.hint(n, 10));
        for _ in 0..n {
            let name = r.string("CONVEX SHAPES", "name")?;
            let parent = r.i32("CONVEX SHAPES", "parent")?;
            let material = r.i32("CONVEX SHAPES", "material")?;
            let (rotation, translation) = r.transform("CONVEX SHAPES", "transform")?;
            let vc = r.count("CONVEX SHAPES")?;
            let mut vertices = Vec::with_capacity(r.hint(vc, 3));
            for _ in 0..vc {
                vertices.push(r.point3("CONVEX SHAPES", "vertex")?);
            }
            out.convex_shapes.push(JmsConvex {
                name, parent, material, rotation, translation, vertices,
            });
        }

        // ### RAGDOLLS ###
        let n = r.count("RAGDOLLS")?;
        out.ragdolls.reserve(r.hint(n, 24));
        for _ in 0..n {
            let name = r.string("RAGDOLLS", "name")?;
            let attached = r.i32("RAGDOLLS", "attached index")?;
            let referenced = r.i32("RAGDOLLS", "referenced index")?;
            let (attached_rotation, attached_translation) =
                r.transform("RAGDOLLS", "attached transform")?;
            let (referenced_rotation, referenced_translation) =
                r.transform("RAGDOLLS", "reference transform")?;
            out.ragdolls.push(JmsRagdoll {
                name,
                attached,
                referenced,
                attached_rotation,
                attached_translation,
                referenced_rotation,
                referenced_translation,
                min_twist: r.f32("RAGDOLLS", "min twist")?,
                max_twist: r.f32("RAGDOLLS", "max twist")?,
                min_cone: r.f32("RAGDOLLS", "min cone")?,
                max_cone: r.f32("RAGDOLLS", "max cone")?,
                min_plane: r.f32("RAGDOLLS", "min plane")?,
                max_plane: r.f32("RAGDOLLS", "max plane")?,
                friction_limit: r.f32("RAGDOLLS", "friction limit")?,
            });
        }

        // ### HINGES ###
        let n = r.count("HINGES")?;
        out.hinges.reserve(r.hint(n, 21));
        for _ in 0..n {
            let name = r.string("HINGES", "name")?;
            let body_a = r.i32("HINGES", "body A index")?;
            let body_b = r.i32("HINGES", "body B index")?;
            let (a_rotation, a_translation) = r.transform("HINGES", "body A transform")?;
            let (b_rotation, b_translation) = r.transform("HINGES", "body B transform")?;
            out.hinges.push(JmsHinge {
                name,
                body_a,
                body_b,
                a_rotation,
                a_translation,
                b_rotation,
                b_translation,
                is_limited: r.i32("HINGES", "is limited")?,
                friction_limit: r.f32("HINGES", "friction limit")?,
                min_angle: r.f32("HINGES", "min angle")?,
                max_angle: r.f32("HINGES", "max angle")?,
            });
        }

        if has_extra_constraints {
            // The slot 181 of 254 shipped files mislabel as a second
            // HINGES header. Position is what counts.
            r.skip_unrepresentable("CAR WHEEL")?;
            r.skip_unrepresentable("POINT TO POINT")?;
            r.skip_unrepresentable("PRISMATIC")?;
        }

        r.skip_unrepresentable("BOUNDING SPHERE")?;

        if has_skylight {
            r.skip_unrepresentable("SKYLIGHT")?;
        }

        if r.remaining() != 0 {
            return Err(JmsParseError::TrailingTokens {
                remaining: r.remaining(),
                line: r.toks.get(r.pos).map_or(0, |t| t.line),
            });
        }

        Ok((out, version))
    }

    /// Parse JMS from raw bytes, replacing any invalid UTF-8. JMS is
    /// nominally ASCII but artist-authored names occasionally are not,
    /// and a name byte should never fail a whole import.
    pub fn parse_bytes(bytes: &[u8]) -> Result<(Self, u16), JmsParseError> {
        Self::parse(&String::from_utf8_lossy(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest legal 8213 file: every section present, all empty.
    fn empty_8213() -> String {
        let mut s = String::from(";### VERSION ###\n8213\n\n");
        for section in [
            "NODES", "MATERIALS", "MARKERS", "INSTANCE XREF PATHS", "INSTANCE MARKERS",
            "VERTICES", "TRIANGLES", "SPHERES", "BOXES", "CAPSULES", "CONVEX SHAPES",
            "RAGDOLLS", "HINGES", "CAR WHEEL", "POINT TO POINT", "PRISMATIC",
            "BOUNDING SPHERE", "SKYLIGHT",
        ] {
            s.push_str(&format!(";### {section} ###\n0\n;\t<comment is ignored>\n\n"));
        }
        s
    }

    #[test]
    fn parses_an_empty_file() {
        let (jms, v) = JmsFile::parse(&empty_8213()).expect("empty 8213 should parse");
        assert_eq!(v, 8213);
        assert!(jms.nodes.is_empty() && jms.vertices.is_empty());
    }

    #[test]
    fn tabs_separate_fields_but_spaces_do_not() {
        // The material definition line keeps its spaces as one token;
        // the node transform splits on tabs.
        let src = empty_8213().replace(
            ";### MATERIALS ###\n0\n",
            ";### MATERIALS ###\n1\nsentinel_shield\n(4) base shield\n",
        );
        let (jms, _) = JmsFile::parse(&src).unwrap();
        assert_eq!(jms.materials.len(), 1);
        assert_eq!(jms.materials[0].name, "sentinel_shield");
        assert_eq!(jms.materials[0].material_name, "(4) base shield");
    }

    #[test]
    fn a_space_separated_transform_is_one_token_and_fails_loudly() {
        // tool.exe would silently take the first value and drop the rest.
        // We refuse instead, because the failure is otherwise invisible.
        let src = empty_8213().replace(
            ";### NODES ###\n0\n",
            ";### NODES ###\n1\nb_pelvis\n-1\n0.0 0.0 0.0 1.0\n0.0\t0.0\t0.0\n",
        );
        let err = JmsFile::parse(&src).unwrap_err();
        assert!(matches!(err, JmsParseError::NotANumber { .. }), "got {err:?}");
    }

    #[test]
    fn semicolon_comments_run_to_end_of_line() {
        let src = empty_8213().replace(
            ";### NODES ###\n0\n",
            ";### NODES ###\n1\n;NODE 0\tthis\tis\tall\tcomment\nb_pelvis\n-1\n\
             0\t0\t0\t1\n0\t0\t0\n",
        );
        let (jms, _) = JmsFile::parse(&src).unwrap();
        assert_eq!(jms.nodes.len(), 1);
        assert_eq!(jms.nodes[0].name, "b_pelvis");
        assert_eq!(jms.nodes[0].parent, -1);
    }

    #[test]
    fn crlf_and_lf_both_work() {
        let lf = empty_8213();
        let crlf = lf.replace('\n', "\r\n");
        let a = JmsFile::parse(&lf).unwrap();
        let b = JmsFile::parse(&crlf).unwrap();
        assert_eq!(a.1, b.1);
        assert_eq!(a.0.nodes.len(), b.0.nodes.len());
    }

    #[test]
    fn old_versions_are_refused_not_guessed() {
        let src = empty_8213().replace("8213", "8200");
        assert_eq!(
            JmsFile::parse(&src).unwrap_err(),
            JmsParseError::UnsupportedVersion(8200)
        );
    }

    #[test]
    fn a_negative_count_does_not_panic() {
        let src = empty_8213().replace(";### NODES ###\n0\n", ";### NODES ###\n-5\n");
        assert!(matches!(
            JmsFile::parse(&src).unwrap_err(),
            JmsParseError::BadCount { .. }
        ));
    }

    #[test]
    fn an_absurd_count_fails_cleanly_without_allocating() {
        // The reserve is bounded by the tokens actually left and the read
        // loop bails on the first missing one, so this costs one iteration
        // rather than a billion — and reports the truthful reason, which is
        // that the file ended, not that the number was malformed.
        let src = empty_8213().replace(";### NODES ###\n0\n", ";### NODES ###\n999999999\n");
        assert!(matches!(
            JmsFile::parse(&src).unwrap_err(),
            JmsParseError::UnexpectedEof { section: "NODES", .. }
        ));
    }

    #[test]
    fn a_populated_unrepresentable_section_is_refused() {
        let src = empty_8213().replace(
            ";### BOUNDING SPHERE ###\n0\n",
            ";### BOUNDING SPHERE ###\n1\n0\t0\t0\n1.0\n",
        );
        assert!(matches!(
            JmsFile::parse(&src).unwrap_err(),
            JmsParseError::UnrepresentableSection { section: "BOUNDING SPHERE", count: 1, .. }
        ));
    }

    #[test]
    fn truncation_reports_the_section_that_ran_out() {
        let src = ";### VERSION ###\n8213\n;### NODES ###\n2\nb_pelvis\n-1\n";
        assert!(matches!(
            JmsFile::parse(src).unwrap_err(),
            JmsParseError::UnexpectedEof { section: "NODES", .. }
        ));
    }

    #[test]
    fn vertex_colour_is_read_at_8211_and_above() {
        let src = empty_8213().replace(
            ";### VERTICES ###\n0\n",
            ";### VERTICES ###\n1\n\
             1\t2\t3\n0\t0\t1\n1\n0\n1.0\n1\n0.25\t0.75\n0.5\t0.25\t0.125\n",
        );
        let (jms, _) = JmsFile::parse(&src).unwrap();
        let v = &jms.vertices[0];
        assert_eq!(v.position, RealPoint3d { x: 1.0, y: 2.0, z: 3.0 });
        assert_eq!(v.node_sets, vec![(0i16, 1.0f32)]);
        assert_eq!(v.uvs, vec![RealPoint2d { x: 0.25, y: 0.75 }]);
        assert_eq!(v.color, Some(RealPoint3d { x: 0.5, y: 0.25, z: 0.125 }));
    }

    #[test]
    fn write_then_parse_round_trips() {
        let mut jms = JmsFile::default();
        jms.nodes.push(JmsNode {
            name: "b_pelvis".into(),
            parent: -1,
            rotation: RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 },
            translation: RealPoint3d { x: 1.5, y: -2.25, z: 94.125 },
        });
        jms.materials.push(JmsMaterial {
            name: "hull".into(),
            material_name: "(0) default hull".into(),
        });
        jms.vertices.push(JmsVertex {
            position: RealPoint3d { x: 1.0, y: 2.0, z: 3.0 },
            normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
            tangent: None,
            binormal: None,
            node_sets: vec![(0, 1.0)],
            uvs: vec![RealPoint2d { x: 0.5, y: 0.25 }],
            color: Some(RealPoint3d { x: 0.0, y: 0.0, z: 0.0 }),
        });
        jms.triangles.push(JmsTriangle { material: 0, v: [0, 0, 0], region: 0 });

        let mut buf = Vec::new();
        jms.write(&mut buf, 8213).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let (back, version) = JmsFile::parse(&text).expect("our own output must parse");

        assert_eq!(version, 8213);
        assert_eq!(back.nodes.len(), 1);
        assert_eq!(back.nodes[0].name, jms.nodes[0].name);
        assert_eq!(back.nodes[0].translation, jms.nodes[0].translation);
        assert_eq!(back.materials[0].material_name, jms.materials[0].material_name);
        assert_eq!(back.vertices[0].position, jms.vertices[0].position);
        assert_eq!(back.vertices[0].uvs, jms.vertices[0].uvs);
        assert_eq!(back.triangles[0].v, jms.triangles[0].v);
    }
}
