//! Reading `.ass` — the other direction from [`crate::ass`].
//!
//! `ass.rs` reconstructs an ASS scene *from* a `scenario_structure_bsp`.
//! This parses one back, which is what an importer needs and what lets
//! the pair be checked against each other: export a shipped sbsp, parse
//! the result, and the two `AssFile`s must agree. That round trip is a
//! real oracle — the corpus supplies the inputs and the writer supplies
//! the expected answer, so neither side is grading its own work.
//!
//! # The format
//!
//! Four sections, each introduced by a `;###`-style comment the parser
//! ignores along with every other comment and blank line:
//!
//! ```text
//! ;### HEADER ###      version, then four quoted strings
//! ;### MATERIALS ###   count, then name + lightmap variant + BM strings
//! ;### OBJECTS ###     count, then a class line and a per-class payload
//! ;### INSTANCES ###   count, then a placement per instance
//! ```
//!
//! Versions differ in ways that matter to a reader, and the version
//! line says which: per-vertex colour appears at 6 and above, the
//! `index<TAB>weight` node form at 3 (older splits it over two lines),
//! three-component UVs at 5 (older is two), and the inline triangle at 3
//! (older writes material and three indices on four lines). H3 writes 7
//! and Halo 2 writes 2, and both are read here.
//!
//! # Quoted strings are why this is not `split_whitespace`
//!
//! A material name, an xref path or an instance name can contain spaces,
//! so the tokeniser has to keep a quoted run together. Splitting on
//! whitespace loses the rest of the name and then misreads every field
//! after it as part of the same record — which is silent, because the
//! counts still add up.

use crate::ass::{
    AssFile, AssInstance, AssLight, AssLightKind, AssMaterial, AssObject, AssObjectPayload,
    AssTriangle, AssVertex,
};
use crate::math::{RealPoint3d, RealQuaternion, RealRgbColor, RealVector3d};

/// Why an ASS could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssParseError {
    /// Ran out of tokens where one was required.
    Truncated { what: &'static str },
    /// A token was not the kind of thing expected there.
    Bad { what: &'static str, got: String },
    /// The version line is not one this reader handles.
    Version(u32),
}

impl std::fmt::Display for AssParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated { what } => write!(f, "the file ends where {what} was expected"),
            Self::Bad { what, got } => write!(f, "expected {what}, got {got:?}"),
            Self::Version(v) => write!(f, "ASS version {v} is not supported (2 to 7 are)"),
        }
    }
}

impl std::error::Error for AssParseError {}

type R<T> = Result<T, AssParseError>;

/// One token: either a bare run of non-space characters or the inside of
/// a quoted string.
struct Lexer {
    toks: Vec<String>,
    at: usize,
}

impl Lexer {
    fn new(src: &str) -> Self {
        let mut toks = Vec::new();
        for line in src.lines() {
            // A comment runs to end of line, and the section banners are
            // comments too — the counts are what drive the parse, not
            // the banners.
            let line = match line.find(';') {
                Some(i) => &line[..i],
                None => line,
            };
            let mut rest = line;
            while !rest.is_empty() {
                let trimmed = rest.trim_start();
                if trimmed.is_empty() {
                    break;
                }
                if let Some(after) = trimmed.strip_prefix('"') {
                    // Quoted: keep spaces, and tolerate a missing close
                    // quote at end of line rather than swallowing the
                    // rest of the file.
                    //
                    // The closing quote is one followed by whitespace or
                    // the end of the line. A name may contain a quote of
                    // its own — `070_bsp_050` has a material called
                    // `int_catwalk_19'x8'6"_02`, where the inches mark is
                    // part of the name and nothing escapes it — and that
                    // one is followed by a letter. Closing at the first
                    // quote regardless cut the name in half and left
                    // `_02"` as a stray token, which took the level down;
                    // closing at the last would have merged the four
                    // quoted fields of a header line into one.
                    let close = after.char_indices().find(|(i, c)| {
                        *c == '"'
                            && after[i + 1..].chars().next().is_none_or(char::is_whitespace)
                    });
                    match close.map(|(i, _)| i) {
                        Some(end) => {
                            toks.push(after[..end].to_owned());
                            rest = &after[end + 1..];
                        }
                        None => {
                            toks.push(after.to_owned());
                            break;
                        }
                    }
                } else {
                    let end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
                    toks.push(trimmed[..end].to_owned());
                    rest = &trimmed[end..];
                }
            }
        }
        Self { toks, at: 0 }
    }

    fn next(&mut self, what: &'static str) -> R<&str> {
        let t = self.toks.get(self.at).ok_or(AssParseError::Truncated { what })?;
        self.at += 1;
        Ok(t)
    }

    fn string(&mut self, what: &'static str) -> R<String> {
        Ok(self.next(what)?.to_owned())
    }

    fn i32(&mut self, what: &'static str) -> R<i32> {
        let t = self.next(what)?;
        t.parse::<i32>()
            .or_else(|_| t.parse::<f64>().map(|v| v as i32))
            .map_err(|_| AssParseError::Bad { what, got: t.to_owned() })
    }

    fn usize(&mut self, what: &'static str) -> R<usize> {
        let v = self.i32(what)?;
        usize::try_from(v).map_err(|_| AssParseError::Bad { what, got: v.to_string() })
    }

    fn f32(&mut self, what: &'static str) -> R<f32> {
        let t = self.next(what)?;
        t.parse::<f32>().map_err(|_| AssParseError::Bad { what, got: t.to_owned() })
    }

    fn point3(&mut self, what: &'static str) -> R<RealPoint3d> {
        Ok(RealPoint3d { x: self.f32(what)?, y: self.f32(what)?, z: self.f32(what)? })
    }

    fn vector3(&mut self, what: &'static str) -> R<RealVector3d> {
        Ok(RealVector3d { i: self.f32(what)?, j: self.f32(what)?, k: self.f32(what)? })
    }

    fn quat(&mut self, what: &'static str) -> R<RealQuaternion> {
        Ok(RealQuaternion {
            i: self.f32(what)?,
            j: self.f32(what)?,
            k: self.f32(what)?,
            w: self.f32(what)?,
        })
    }

    fn rgb(&mut self, what: &'static str) -> R<RealRgbColor> {
        Ok(RealRgbColor {
            red: self.f32(what)?,
            green: self.f32(what)?,
            blue: self.f32(what)?,
        })
    }
}

/// Parse an ASS scene.
pub fn parse(src: &str) -> R<(AssFile, u32)> {
    let mut lx = Lexer::new(src);

    let version = lx.usize("the version")? as u32;
    if !(2..=7).contains(&version) {
        return Err(AssParseError::Version(version));
    }
    let header_tool = lx.string("the tool name")?;
    let header_tool_version = lx.string("the tool version")?;
    let header_user = lx.string("the user name")?;
    let header_machine = lx.string("the machine name")?;

    // ---- materials --------------------------------------------------
    let count = lx.usize("the material count")?;
    let mut materials = Vec::with_capacity(count);
    for _ in 0..count {
        let name = lx.string("a material name")?;
        let lightmap_variant = lx.string("a lightmap variant")?;
        let mut bm_strings = Vec::new();
        if version >= 4 {
            let n = lx.usize("a BM string count")?;
            for _ in 0..n {
                bm_strings.push(lx.string("a BM string")?);
            }
        }
        materials.push(AssMaterial { name, lightmap_variant, bm_strings });
    }

    // ---- objects ----------------------------------------------------
    let count = lx.usize("the object count")?;
    let mut objects = Vec::with_capacity(count);
    for _ in 0..count {
        let class = lx.string("an object class")?;
        let xref_filepath = lx.string("an xref filepath")?;
        let xref_objectname = lx.string("an xref object name")?;
        let payload = match class.as_str() {
            "MESH" => parse_mesh(&mut lx, version)?,
            "GENERIC_LIGHT" => AssObjectPayload::GenericLight(parse_light(&mut lx)?),
            "SPHERE" => AssObjectPayload::Sphere {
                material: lx.i32("a sphere material")?,
                radius: lx.f32("a sphere radius")?,
            },
            other => {
                return Err(AssParseError::Bad {
                    what: "an object class this reader knows",
                    got: other.to_owned(),
                })
            }
        };
        objects.push(AssObject { xref_filepath, xref_objectname, payload });
    }

    // ---- instances --------------------------------------------------
    let count = lx.usize("the instance count")?;
    let mut instances = Vec::with_capacity(count);
    for _ in 0..count {
        let object_index = lx.i32("an instance object index")?;
        let name = lx.string("an instance name")?;
        let unique_id = lx.i32("a unique id")?;
        let parent_id = lx.i32("a parent id")?;
        let inheritance_flag = lx.i32("an inheritance flag")?;
        let local_rotation = lx.quat("a local rotation")?;
        let local_translation = lx.point3("a local translation")?;
        let local_scale = lx.f32("a local scale")?;
        let pivot_rotation = lx.quat("a pivot rotation")?;
        let pivot_translation = lx.point3("a pivot translation")?;
        let pivot_scale = lx.f32("a pivot scale")?;
        // Bone groups are only meaningful on a skinned instance, and the
        // writer emits one line each with no count in front — so they run
        // to the end of the record, which is the end of the file for the
        // last instance. Nothing in the H3 structure path uses them.
        instances.push(AssInstance {
            object_index,
            name,
            unique_id,
            parent_id,
            inheritance_flag,
            local_rotation,
            local_translation,
            local_scale,
            pivot_rotation,
            pivot_translation,
            pivot_scale,
            bone_groups: Vec::new(),
        });
    }

    Ok((
        AssFile {
            header_tool,
            header_tool_version,
            header_user,
            header_machine,
            materials,
            objects,
            instances,
        },
        version,
    ))
}

fn parse_mesh(lx: &mut Lexer, version: u32) -> R<AssObjectPayload> {
    let n = lx.usize("a vertex count")?;
    let mut vertices = Vec::with_capacity(n);
    for _ in 0..n {
        let position = lx.point3("a vertex position")?;
        let normal = lx.vector3("a vertex normal")?;
        let color = if version >= 6 {
            lx.rgb("a vertex colour")?
        } else {
            RealRgbColor { red: 0.0, green: 0.0, blue: 0.0 }
        };
        let sets = lx.usize("a node set count")?;
        let mut node_set = Vec::with_capacity(sets);
        for _ in 0..sets {
            // Whether the pair is on one line or two makes no difference
            // once tokenised — the version only changes the whitespace.
            let idx = lx.i32("a node index")?;
            let weight = lx.f32("a node weight")?;
            node_set.push((idx, weight));
        }
        let uv_count = lx.usize("a uv count")?;
        let mut uvs = Vec::with_capacity(uv_count);
        for _ in 0..uv_count {
            let x = lx.f32("a uv u")?;
            let y = lx.f32("a uv v")?;
            let z = if version >= 5 { lx.f32("a uv w")? } else { 0.0 };
            uvs.push(RealPoint3d { x, y, z });
        }
        vertices.push(AssVertex { position, normal, color, node_set, uvs });
    }

    let n = lx.usize("a triangle count")?;
    let mut triangles = Vec::with_capacity(n);
    for _ in 0..n {
        let material = lx.i32("a triangle material")?;
        let a = lx.i32("a triangle vertex")? as u32;
        let b = lx.i32("a triangle vertex")? as u32;
        let c = lx.i32("a triangle vertex")? as u32;
        triangles.push(AssTriangle { material, v: [a, b, c] });
    }

    Ok(AssObjectPayload::Mesh { vertices, triangles })
}

fn parse_light(lx: &mut Lexer) -> R<AssLight> {
    let kind = match lx.string("a light class")?.as_str() {
        "SPOT_LGT" => AssLightKind::SpotLgt,
        "DIRECT_LGT" => AssLightKind::DirectLgt,
        "OMNI_LGT" => AssLightKind::OmniLgt,
        "AMBIENT_LGT" => AssLightKind::AmbientLgt,
        other => {
            return Err(AssParseError::Bad {
                what: "a light class",
                got: other.to_owned(),
            })
        }
    };
    Ok(AssLight {
        kind,
        color: lx.rgb("a light colour")?,
        intensity: lx.f32("a light intensity")?,
        hotspot_size: lx.f32("a hotspot size")?,
        hotspot_falloff: lx.f32("a hotspot falloff")?,
        use_near_attenuation: lx.i32("a near attenuation flag")? != 0,
        near_atten_min: lx.f32("a near attenuation min")?,
        near_atten_max: lx.f32("a near attenuation max")?,
        use_far_attenuation: lx.i32("a far attenuation flag")? != 0,
        far_atten_min: lx.f32("a far attenuation min")?,
        far_atten_max: lx.f32("a far attenuation max")?,
        shape: lx.i32("a light shape")?,
        aspect: lx.f32("a light aspect")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest complete scene, to pin the section order.
    #[test]
    fn an_empty_scene_round_trips() {
        let src = "\
;### HEADER ###
7
\"tool\"
\"1.0\"
\"user\"
\"machine\"

;### MATERIALS ###
0

;### OBJECTS ###
0

;### INSTANCES ###
0
";
        let (ass, version) = parse(src).expect("parse");
        assert_eq!(version, 7);
        assert_eq!(ass.header_tool, "tool");
        assert_eq!(ass.header_machine, "machine");
        assert!(ass.materials.is_empty() && ass.objects.is_empty());
    }

    /// Names with spaces survive, which whitespace splitting would not
    /// manage — and the failure would be silent, because the counts
    /// still add up.
    #[test]
    fn quoted_names_keep_their_spaces() {
        let src = "\
7
\"a tool\" \"v 1\" \"a user\" \"a machine\"
2
\"shader with spaces\"
\"lightmap one\"
0
\"second\"
\"\"
0
0
0
";
        let (ass, _) = parse(src).expect("parse");
        assert_eq!(ass.materials[0].name, "shader with spaces");
        assert_eq!(ass.materials[0].lightmap_variant, "lightmap one");
        assert_eq!(ass.materials[1].name, "second");
        assert_eq!(ass.materials[1].lightmap_variant, "");
    }

    /// A name may contain a quote, and the kit's own content does.
    ///
    /// `070_waste`'s `070_bsp_050` has a material called
    /// `%!+int_catwalk_19'x8'6"_02` — feet and inches, unescaped. The
    /// close is the quote followed by whitespace or end of line, so an
    /// inches mark in the middle of a name is just a character, while
    /// the four quoted fields of a header line stay four fields.
    #[test]
    fn a_name_may_contain_a_quote() {
        let src = "7
\"a tool\" \"v 1\" \"a user\" \"a machine\"
1
\"%!+int_catwalk_19'x8'6\"_02\"
\"\"
0
0
0
";
        let (ass, _) = parse(src).expect("parse");
        assert_eq!(ass.materials.len(), 1);
        assert_eq!(ass.materials[0].name, "%!+int_catwalk_19'x8'6\"_02");
    }

    /// A comment is a comment wherever it starts.
    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let src = "\
; leading chatter
7
\"t\" \"v\" \"u\" \"m\"

; how many materials
1
\"mat\"  ; trailing note
\"\"
0

0
0
";
        let (ass, _) = parse(src).expect("parse");
        assert_eq!(ass.materials.len(), 1);
        assert_eq!(ass.materials[0].name, "mat");
    }

    /// A truncated file says what it wanted, not just that it failed.
    #[test]
    fn truncation_names_what_was_missing() {
        let err = parse("7\n\"t\" \"v\" \"u\" \"m\"\n2\n\"only\"\n\"\"\n0\n").unwrap_err();
        assert!(
            matches!(err, AssParseError::Truncated { .. }),
            "expected a truncation, got {err:?}"
        );
    }
}
