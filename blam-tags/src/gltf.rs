//! glTF 2.0 → JMS.
//!
//! The other half of importing Halo 3 geometry from formats Bungie's
//! tooling never supported. `tool.exe`'s own `fbx-to-jms` verb converts
//! FBX by building the importer's intermediate scene and then
//! **serialising it back out as JMS text** — text is the sanctioned seam,
//! not a workaround — so writing JMS is exactly what a new front end
//! should do. [`crate::jms::JmsFile::write`] is the finished back end;
//! this module is a front end onto it.
//!
//! glTF rather than FBX because `tool.exe` already covers FBX, and glTF
//! needs no SDK: it is JSON plus a binary blob, and the subset that
//! carries rigid and skinned meshes is small enough to read directly.
//!
//! # What is supported
//!
//! * `.glb` (binary container) and `.gltf` (JSON), with buffers inline,
//!   in a sibling `.bin`, or in a `data:` URI.
//! * `TRIANGLES` primitives. Other modes are refused rather than
//!   silently approximated.
//! * `POSITION`, `NORMAL`, `TEXCOORD_0`, `TEXCOORD_1`, `COLOR_0`,
//!   `JOINTS_0`, `WEIGHTS_0`.
//! * Skinning, with the bind pose taken from `inverseBindMatrices` where
//!   present, because that is the pose the skin was authored against —
//!   the node's current transform may be an animation frame.
//! * Nodes named `#something` become JMS **markers** rather than
//!   skeleton nodes, following Halo's own naming convention.
//!
//! Deliberately refused, with a named error rather than a guess: sparse
//! accessors, non-triangle primitives, and more than four influences per
//! vertex.
//!
//! # Two conversions that are not optional
//!
//! **Axes.** glTF is right-handed Y-up; Halo is right-handed Z-up. The
//! conversion is a +90° rotation about X — `(x, y, z) → (x, −z, y)` —
//! applied to positions, normals and node orientations alike. Its
//! determinant is +1, so winding order is preserved and no triangle
//! needs flipping. Skipping it lays every model on its side.
//!
//! **Scale.** JMS units are hundredths of a Halo world unit; the
//! importer multiplies by 0.01 on the way in. What a glTF file's units
//! *mean* is not recorded anywhere in the file, so this module will not
//! guess: [`GltfOptions::scale`] defaults to 1.0, meaning "the glTF is
//! already in JMS units". A model authored in metres against Halo's
//! 3.048 m world unit wants [`METRES_TO_JMS`].
//!
//! # What it cannot know
//!
//! JMS carries region and permutation per *material*, and glTF has no
//! such concept. Everything lands in one region and one permutation,
//! both named by [`GltfOptions`]. Split it afterwards with
//! [`crate::jms_split`], which is the piece that gets a big mesh past
//! `tool.exe`'s per-section limit.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::jms::{JmsFile, JmsMarker, JmsMaterial, JmsNode, JmsTriangle, JmsVertex};
use crate::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};

/// Multiply glTF metres by this to get JMS units, assuming Halo's world
/// unit of 3.048 m (ten feet). `100 / 3.048`.
pub const METRES_TO_JMS: f32 = 32.808_4;

/// JMS node ceiling. Also `tool.exe`'s, and the engine's — u8 node maps.
pub const MAX_NODES: usize = 255;

/// How to interpret the glTF.
#[derive(Debug, Clone)]
pub struct GltfOptions {
    /// Multiplier applied to every position and translation. See the
    /// module docs — 1.0 means the file is already in JMS units.
    pub scale: f32,
    /// Rotate Y-up into Halo's Z-up. Leave on unless the source was
    /// already authored Z-up.
    pub y_up_to_z_up: bool,
    /// Permutation written into every material label.
    pub permutation: String,
    /// Region written into every material label.
    pub region: String,
}

impl Default for GltfOptions {
    fn default() -> Self {
        Self {
            scale: 1.0,
            y_up_to_z_up: true,
            permutation: "default".to_owned(),
            region: "default".to_owned(),
        }
    }
}

/// Why a glTF could not be converted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GltfError {
    /// Not a `.glb` container and not parseable as JSON.
    NotGltf(String),
    /// Structurally valid JSON that is missing something required.
    Malformed(String),
    /// A buffer could not be resolved — an external `.bin` that is not
    /// next to the `.gltf`, or an unsupported URI scheme.
    MissingBuffer(String),
    /// Present in the file, understood, and deliberately not handled.
    Unsupported(String),
    /// The result would not fit JMS or `tool.exe`.
    TooLarge(String),
}

impl std::fmt::Display for GltfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotGltf(m) => write!(f, "not a glTF file: {m}"),
            Self::Malformed(m) => write!(f, "malformed glTF: {m}"),
            Self::MissingBuffer(m) => write!(f, "cannot resolve buffer: {m}"),
            Self::Unsupported(m) => write!(f, "unsupported glTF feature: {m}"),
            Self::TooLarge(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for GltfError {}

type R<T> = Result<T, GltfError>;

fn bad(m: impl Into<String>) -> GltfError {
    GltfError::Malformed(m.into())
}

/// Convert a glTF file to a JMS scene.
///
/// `base_dir` is where external `.bin` buffers are looked for; pass the
/// directory holding the `.gltf`. `None` refuses external buffers.
pub fn jms_from_gltf(bytes: &[u8], base_dir: Option<&Path>, opts: &GltfOptions) -> R<JmsFile> {
    let doc = Document::load(bytes, base_dir)?;
    doc.to_jms(opts)
}

/// Read a glTF from disk, resolving external buffers next to it.
pub fn jms_from_gltf_path(path: &Path, opts: &GltfOptions) -> R<JmsFile> {
    let bytes = std::fs::read(path)
        .map_err(|e| GltfError::MissingBuffer(format!("{}: {e}", path.display())))?;
    jms_from_gltf(&bytes, path.parent(), opts)
}

// ---------------------------------------------------------------- document

struct Document {
    json: Value,
    buffers: Vec<Vec<u8>>,
}

impl Document {
    fn load(bytes: &[u8], base_dir: Option<&Path>) -> R<Self> {
        // GLB: 'glTF', version, total length, then length-tagged chunks.
        let (json_bytes, glb_bin) = if bytes.len() >= 12 && &bytes[0..4] == b"glTF" {
            let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            if version != 2 {
                return Err(GltfError::Unsupported(format!("GLB version {version}, expected 2")));
            }
            let mut json = None;
            let mut bin = None;
            let mut at = 12usize;
            while at + 8 <= bytes.len() {
                let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
                let kind = &bytes[at + 4..at + 8];
                let start = at + 8;
                let end = start.checked_add(len).ok_or_else(|| bad("GLB chunk overflows"))?;
                if end > bytes.len() {
                    return Err(bad("GLB chunk runs past end of file"));
                }
                match kind {
                    b"JSON" => json = Some(bytes[start..end].to_vec()),
                    b"BIN\0" => bin = Some(bytes[start..end].to_vec()),
                    _ => {} // unknown chunks are skippable by spec
                }
                at = end + (4 - end % 4) % 4;
            }
            (json.ok_or_else(|| bad("GLB has no JSON chunk"))?, bin)
        } else {
            (bytes.to_vec(), None)
        };

        let json: Value = serde_json::from_slice(&json_bytes)
            .map_err(|e| GltfError::NotGltf(e.to_string()))?;
        if !json.is_object() {
            return Err(GltfError::NotGltf("top level is not an object".into()));
        }

        // Resolve every buffer up front — a dangling one is a hard error,
        // not a surprise halfway through geometry.
        let mut buffers = Vec::new();
        for (i, buf) in arr(&json, "buffers").iter().enumerate() {
            let len = buf.get("byteLength").and_then(Value::as_u64).unwrap_or(0) as usize;
            let data = match buf.get("uri").and_then(Value::as_str) {
                None => glb_bin
                    .clone()
                    .ok_or_else(|| bad(format!("buffer {i} has no uri and there is no BIN chunk")))?,
                Some(uri) if uri.starts_with("data:") => {
                    let payload = uri
                        .split_once(";base64,")
                        .map(|(_, b)| b)
                        .ok_or_else(|| GltfError::Unsupported(
                            "data: URI that is not ;base64,".into(),
                        ))?;
                    decode_base64(payload)
                        .ok_or_else(|| bad(format!("buffer {i} has invalid base64")))?
                }
                Some(uri) => {
                    let dir = base_dir.ok_or_else(|| {
                        GltfError::MissingBuffer(format!(
                            "buffer {i} is external ({uri}) but no base directory was given"
                        ))
                    })?;
                    let path = dir.join(percent_decode(uri));
                    std::fs::read(&path).map_err(|e| {
                        GltfError::MissingBuffer(format!("{}: {e}", path.display()))
                    })?
                }
            };
            if data.len() < len {
                return Err(bad(format!(
                    "buffer {i} declares {len} bytes but only {} are available",
                    data.len()
                )));
            }
            buffers.push(data);
        }

        Ok(Self { json, buffers })
    }

    fn accessor(&self, index: usize) -> R<&Value> {
        arr(&self.json, "accessors")
            .get(index)
            .ok_or_else(|| bad(format!("accessor {index} does not exist")))
    }

    /// Read an accessor as rows of up to four floats, applying the
    /// `normalized` rule for integer component types.
    fn read_floats(&self, index: usize) -> R<Vec<[f32; 4]>> {
        let acc = self.accessor(index)?;
        if acc.get("sparse").is_some() {
            return Err(GltfError::Unsupported("sparse accessors".into()));
        }
        let count = acc.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let comps = type_components(acc)?;
        let ctype = acc
            .get("componentType")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("accessor has no componentType"))? as u32;
        let normalized = acc.get("normalized").and_then(Value::as_bool).unwrap_or(false);
        let size = component_size(ctype)?;

        let mut out = vec![[0.0f32; 4]; count];
        // An accessor with no bufferView reads as zeros, by spec.
        let Some(view_index) = acc.get("bufferView").and_then(Value::as_u64) else {
            return Ok(out);
        };
        let (data, stride) = self.view(view_index as usize, size * comps)?;
        let base = acc.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;

        for (i, row) in out.iter_mut().enumerate() {
            let at = base + i * stride;
            for (c, slot) in row.iter_mut().enumerate().take(comps) {
                let o = at + c * size;
                let raw = data
                    .get(o..o + size)
                    .ok_or_else(|| bad(format!("accessor {index} reads past its buffer view")))?;
                *slot = decode_component(ctype, raw, normalized)?;
            }
        }
        Ok(out)
    }

    /// Read an accessor as unsigned integers — indices and joint ids.
    fn read_uints(&self, index: usize) -> R<Vec<u32>> {
        let acc = self.accessor(index)?;
        if acc.get("sparse").is_some() {
            return Err(GltfError::Unsupported("sparse accessors".into()));
        }
        let count = acc.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let comps = type_components(acc)?;
        let ctype = acc
            .get("componentType")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("accessor has no componentType"))? as u32;
        let size = component_size(ctype)?;

        let mut out = vec![0u32; count * comps];
        let Some(view_index) = acc.get("bufferView").and_then(Value::as_u64) else {
            return Ok(out);
        };
        let (data, stride) = self.view(view_index as usize, size * comps)?;
        let base = acc.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;

        for i in 0..count {
            let at = base + i * stride;
            for c in 0..comps {
                let o = at + c * size;
                let raw = data
                    .get(o..o + size)
                    .ok_or_else(|| bad(format!("accessor {index} reads past its buffer view")))?;
                out[i * comps + c] = match ctype {
                    5120 => i8::from_le_bytes([raw[0]]).max(0) as u32,
                    5121 => raw[0] as u32,
                    5122 => i16::from_le_bytes([raw[0], raw[1]]).max(0) as u32,
                    5123 => u16::from_le_bytes([raw[0], raw[1]]) as u32,
                    5125 => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
                    other => return Err(bad(format!("componentType {other} is not an integer"))),
                };
            }
        }
        Ok(out)
    }

    /// A buffer view's bytes, and the stride to walk it with.
    fn view(&self, index: usize, element: usize) -> R<(&[u8], usize)> {
        let view = arr(&self.json, "bufferViews")
            .get(index)
            .ok_or_else(|| bad(format!("bufferView {index} does not exist")))?;
        let buffer = view.get("buffer").and_then(Value::as_u64).unwrap_or(0) as usize;
        let offset = view.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let length = view.get("byteLength").and_then(Value::as_u64).unwrap_or(0) as usize;
        let stride = view
            .get("byteStride")
            .and_then(Value::as_u64)
            .map(|s| s as usize)
            .unwrap_or(element)
            .max(1);
        let data = self
            .buffers
            .get(buffer)
            .ok_or_else(|| bad(format!("bufferView {index} points at missing buffer {buffer}")))?;
        let end = offset
            .checked_add(length)
            .filter(|e| *e <= data.len())
            .ok_or_else(|| bad(format!("bufferView {index} runs past its buffer")))?;
        Ok((&data[offset..end], stride))
    }
}

// ---------------------------------------------------------------- helpers

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn type_components(acc: &Value) -> R<usize> {
    match acc.get("type").and_then(Value::as_str) {
        Some("SCALAR") => Ok(1),
        Some("VEC2") => Ok(2),
        Some("VEC3") => Ok(3),
        Some("VEC4") => Ok(4),
        Some("MAT4") => Ok(16),
        Some(other) => Err(GltfError::Unsupported(format!("accessor type {other}"))),
        None => Err(bad("accessor has no type")),
    }
}

fn component_size(ctype: u32) -> R<usize> {
    Ok(match ctype {
        5120 | 5121 => 1,
        5122 | 5123 => 2,
        5125 | 5126 => 4,
        other => return Err(bad(format!("unknown componentType {other}"))),
    })
}

fn decode_component(ctype: u32, raw: &[u8], normalized: bool) -> R<f32> {
    Ok(match ctype {
        5126 => f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
        5121 => {
            let v = raw[0] as f32;
            if normalized { v / 255.0 } else { v }
        }
        5120 => {
            let v = i8::from_le_bytes([raw[0]]) as f32;
            if normalized { (v / 127.0).max(-1.0) } else { v }
        }
        5123 => {
            let v = u16::from_le_bytes([raw[0], raw[1]]) as f32;
            if normalized { v / 65535.0 } else { v }
        }
        5122 => {
            let v = i16::from_le_bytes([raw[0], raw[1]]) as f32;
            if normalized { (v / 32767.0).max(-1.0) } else { v }
        }
        5125 => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as f32,
        other => return Err(bad(format!("unknown componentType {other}"))),
    })
}

/// Minimal base64, enough for `data:` URIs. Returns `None` on anything
/// that is not valid standard base64.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    fn sextet(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in text.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | sextet(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// `%20` and friends. glTF URIs are percent-encoded.
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ------------------------------------------------------------------ maths

/// Column-major 4x4, glTF's own layout.
type Mat4 = [f32; 16];

const IDENTITY: Mat4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [0.0f32; 16];
    for c in 0..4 {
        for r in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k * 4 + r] * b[c * 4 + k];
            }
            out[c * 4 + r] = sum;
        }
    }
    out
}

fn mat_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]
}

fn mat_dir(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2],
    ]
}

/// Invert an affine 4x4 (the bottom row is assumed `0 0 0 1`, which is
/// true of every transform glTF can express). Returns identity if the
/// upper 3x3 is singular, which only a degenerate file produces.
fn mat_invert_affine(m: &Mat4) -> Mat4 {
    let (a, b, c) = (m[0], m[4], m[8]);
    let (d, e, f) = (m[1], m[5], m[9]);
    let (g, h, i) = (m[2], m[6], m[10]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-20 {
        return IDENTITY;
    }
    let inv = 1.0 / det;
    // Inverse of the upper 3x3, written column-major.
    let r = [
        (e * i - f * h) * inv,
        -(d * i - f * g) * inv,
        (d * h - e * g) * inv,
        -(b * i - c * h) * inv,
        (a * i - c * g) * inv,
        -(a * h - b * g) * inv,
        (b * f - c * e) * inv,
        -(a * f - c * d) * inv,
        (a * e - b * d) * inv,
    ];
    let t = [m[12], m[13], m[14]];
    let mut out = IDENTITY;
    out[0] = r[0];
    out[1] = r[1];
    out[2] = r[2];
    out[4] = r[3];
    out[5] = r[4];
    out[6] = r[5];
    out[8] = r[6];
    out[9] = r[7];
    out[10] = r[8];
    out[12] = -(r[0] * t[0] + r[3] * t[1] + r[6] * t[2]);
    out[13] = -(r[1] * t[0] + r[4] * t[1] + r[7] * t[2]);
    out[14] = -(r[2] * t[0] + r[5] * t[1] + r[8] * t[2]);
    out
}

/// The rotation part of a matrix as a quaternion, with scale divided out.
fn mat_to_quat(m: &Mat4) -> RealQuaternion {
    // Normalise each basis column so a scaled matrix still yields a unit
    // quaternion — Halo's node transforms carry no scale.
    let col = |c: usize| {
        let v = [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]];
        let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if len > 1e-20 { [v[0] / len, v[1] / len, v[2] / len] } else { [0.0, 0.0, 0.0] }
    };
    let (x, y, z) = (col(0), col(1), col(2));
    let (m00, m01, m02) = (x[0], y[0], z[0]);
    let (m10, m11, m12) = (x[1], y[1], z[1]);
    let (m20, m21, m22) = (x[2], y[2], z[2]);

    let trace = m00 + m11 + m22;
    let (i, j, k, w) = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        ((m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s)
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        (0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s)
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        ((m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s)
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        ((m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s)
    };
    let len = (i * i + j * j + k * k + w * w).sqrt();
    if len > 1e-20 {
        RealQuaternion { i: i / len, j: j / len, k: k / len, w: w / len }
    } else {
        RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 }
    }
}

/// Y-up → Z-up: a +90° rotation about X. `(x, y, z) → (x, −z, y)`.
const Y_UP_TO_Z_UP: Mat4 = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, -1.0, 0.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

fn node_local_matrix(node: &Value) -> Mat4 {
    if let Some(m) = node.get("matrix").and_then(Value::as_array) {
        let mut out = IDENTITY;
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = m.get(i).and_then(Value::as_f64).unwrap_or(0.0) as f32;
        }
        return out;
    }
    let t = node
        .get("translation")
        .and_then(Value::as_array)
        .map(|a| {
            [
                a.first().and_then(Value::as_f64).unwrap_or(0.0) as f32,
                a.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32,
                a.get(2).and_then(Value::as_f64).unwrap_or(0.0) as f32,
            ]
        })
        .unwrap_or([0.0; 3]);
    let r = node
        .get("rotation")
        .and_then(Value::as_array)
        .map(|a| {
            [
                a.first().and_then(Value::as_f64).unwrap_or(0.0) as f32,
                a.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32,
                a.get(2).and_then(Value::as_f64).unwrap_or(0.0) as f32,
                a.get(3).and_then(Value::as_f64).unwrap_or(1.0) as f32,
            ]
        })
        .unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let s = node
        .get("scale")
        .and_then(Value::as_array)
        .map(|a| {
            [
                a.first().and_then(Value::as_f64).unwrap_or(1.0) as f32,
                a.get(1).and_then(Value::as_f64).unwrap_or(1.0) as f32,
                a.get(2).and_then(Value::as_f64).unwrap_or(1.0) as f32,
            ]
        })
        .unwrap_or([1.0; 3]);

    let (x, y, z, w) = (r[0], r[1], r[2], r[3]);
    let mut m = IDENTITY;
    m[0] = (1.0 - 2.0 * (y * y + z * z)) * s[0];
    m[1] = (2.0 * (x * y + z * w)) * s[0];
    m[2] = (2.0 * (x * z - y * w)) * s[0];
    m[4] = (2.0 * (x * y - z * w)) * s[1];
    m[5] = (1.0 - 2.0 * (x * x + z * z)) * s[1];
    m[6] = (2.0 * (y * z + x * w)) * s[1];
    m[8] = (2.0 * (x * z + y * w)) * s[2];
    m[9] = (2.0 * (y * z - x * w)) * s[2];
    m[10] = (1.0 - 2.0 * (x * x + y * y)) * s[2];
    m[12] = t[0];
    m[13] = t[1];
    m[14] = t[2];
    m
}

// -------------------------------------------------------------- conversion

impl Document {
    fn to_jms(&self, opts: &GltfOptions) -> R<JmsFile> {
        let nodes = arr(&self.json, "nodes");
        let world = self.world_transforms(nodes)?;

        // A node whose name starts with '#' is a marker, following the
        // convention Halo's own exporters use, and is kept out of the
        // skeleton.
        let is_marker = |i: usize| {
            nodes[i].get("name").and_then(Value::as_str).is_some_and(|n| n.starts_with('#'))
        };

        // Map glTF node index -> JMS node index, skipping markers.
        let mut jms_index = vec![usize::MAX; nodes.len()];
        let mut skeleton: Vec<usize> = Vec::new();
        for (i, slot) in jms_index.iter_mut().enumerate() {
            if !is_marker(i) {
                *slot = skeleton.len();
                skeleton.push(i);
            }
        }
        if skeleton.len() > MAX_NODES {
            return Err(GltfError::TooLarge(format!(
                "{} nodes, but JMS and the engine allow {MAX_NODES}",
                skeleton.len()
            )));
        }

        // Parent pointers, derived from glTF's children lists.
        let mut parent = vec![-1i32; nodes.len()];
        for (i, node) in nodes.iter().enumerate() {
            for c in arr(node, "children") {
                if let Some(c) = c.as_u64() {
                    let c = c as usize;
                    if c < parent.len() {
                        parent[c] = i as i32;
                    }
                }
            }
        }

        // Bind-pose overrides from inverseBindMatrices: the pose the skin
        // was authored against, which the node's own transform may no
        // longer be.
        let mut bind = BTreeMap::<usize, Mat4>::new();
        for skin in arr(&self.json, "skins") {
            let Some(ibm) = skin.get("inverseBindMatrices").and_then(Value::as_u64) else {
                continue;
            };
            let rows = self.read_mat4s(ibm as usize)?;
            for (slot, joint) in arr(skin, "joints").iter().enumerate() {
                let Some(joint) = joint.as_u64().map(|j| j as usize) else { continue };
                if let Some(m) = rows.get(slot) {
                    bind.insert(joint, mat_invert_affine(m));
                }
            }
        }

        let convert = |m: &Mat4| -> Mat4 {
            if opts.y_up_to_z_up {
                // Change of basis: B * M * B^-1, so the transform means
                // the same thing expressed in Halo's axes.
                mat_mul(&mat_mul(&Y_UP_TO_Z_UP, m), &mat_invert_affine(&Y_UP_TO_Z_UP))
            } else {
                *m
            }
        };
        let convert_point = |p: [f32; 3]| -> [f32; 3] {
            let p = if opts.y_up_to_z_up { mat_point(&Y_UP_TO_Z_UP, p) } else { p };
            [p[0] * opts.scale, p[1] * opts.scale, p[2] * opts.scale]
        };
        let convert_dir = |d: [f32; 3]| -> [f32; 3] {
            if opts.y_up_to_z_up { mat_dir(&Y_UP_TO_Z_UP, d) } else { d }
        };

        let mut jms = JmsFile::default();

        for (slot, &n) in skeleton.iter().enumerate() {
            let m = bind.get(&n).copied().unwrap_or(world[n]);
            let converted = convert(&m);
            let t = [converted[12], converted[13], converted[14]];
            // Parent must point at a JMS index; a marker parent is
            // skipped over to the nearest real ancestor.
            let mut p = parent[n];
            while p >= 0 && jms_index[p as usize] == usize::MAX {
                p = parent[p as usize];
            }
            jms.nodes.push(JmsNode {
                name: node_name(&nodes[n], n),
                parent: if p < 0 { -1 } else { jms_index[p as usize] as i16 },
                rotation: mat_to_quat(&converted),
                translation: RealPoint3d {
                    x: t[0] * opts.scale,
                    y: t[1] * opts.scale,
                    z: t[2] * opts.scale,
                },
            });
            let _ = slot;
        }
        if jms.nodes.is_empty() {
            // JMS needs a skeleton. A rigid model with no nodes gets one
            // root at the origin, which is what every vertex binds to.
            jms.nodes.push(JmsNode {
                name: "frame".into(),
                parent: -1,
                rotation: RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 },
                translation: RealPoint3d::default(),
            });
        }

        for (i, node) in nodes.iter().enumerate() {
            if !is_marker(i) {
                continue;
            }
            let converted = convert(&world[i]);
            let mut p = parent[i];
            while p >= 0 && jms_index[p as usize] == usize::MAX {
                p = parent[p as usize];
            }
            jms.markers.push(JmsMarker {
                name: node_name(node, i).trim_start_matches('#').to_owned(),
                node_index: if p < 0 { 0 } else { jms_index[p as usize] as i16 },
                rotation: mat_to_quat(&converted),
                translation: RealPoint3d {
                    x: converted[12] * opts.scale,
                    y: converted[13] * opts.scale,
                    z: converted[14] * opts.scale,
                },
                radius: 0.0,
            });
        }

        // One JMS material per glTF material actually used, in first-use
        // order so the output is stable.
        let mut material_slot: BTreeMap<i64, i32> = BTreeMap::new();
        let materials = arr(&self.json, "materials");

        for (node_index, node) in nodes.iter().enumerate() {
            let Some(mesh_index) = node.get("mesh").and_then(Value::as_u64) else { continue };
            let Some(mesh) = arr(&self.json, "meshes").get(mesh_index as usize) else { continue };
            let skinned = node.get("skin").is_some();
            let skin_joints: Vec<usize> = node
                .get("skin")
                .and_then(Value::as_u64)
                .and_then(|s| arr(&self.json, "skins").get(s as usize).cloned())
                .map(|s| {
                    arr(&s, "joints").iter().filter_map(|j| j.as_u64().map(|j| j as usize)).collect()
                })
                .unwrap_or_default();
            // A skinned mesh's vertices are already in skin space, so the
            // node transform must NOT be applied — glTF says so
            // explicitly, and applying it doubles the transform.
            let place = if skinned { IDENTITY } else { world[node_index] };
            let default_bind = if jms_index[node_index] == usize::MAX {
                0
            } else {
                jms_index[node_index]
            };

            for prim in arr(mesh, "primitives") {
                let mode = prim.get("mode").and_then(Value::as_u64).unwrap_or(4);
                if mode != 4 {
                    return Err(GltfError::Unsupported(format!(
                        "primitive mode {mode}; only TRIANGLES (4) is supported"
                    )));
                }
                let attrs = prim.get("attributes").ok_or_else(|| bad("primitive has no attributes"))?;
                let Some(pos_acc) = attrs.get("POSITION").and_then(Value::as_u64) else {
                    return Err(bad("primitive has no POSITION"));
                };
                let positions = self.read_floats(pos_acc as usize)?;
                let normals = match attrs.get("NORMAL").and_then(Value::as_u64) {
                    Some(a) => Some(self.read_floats(a as usize)?),
                    None => None,
                };
                let uv0 = match attrs.get("TEXCOORD_0").and_then(Value::as_u64) {
                    Some(a) => Some(self.read_floats(a as usize)?),
                    None => None,
                };
                let uv1 = match attrs.get("TEXCOORD_1").and_then(Value::as_u64) {
                    Some(a) => Some(self.read_floats(a as usize)?),
                    None => None,
                };
                let color = match attrs.get("COLOR_0").and_then(Value::as_u64) {
                    Some(a) => Some(self.read_floats(a as usize)?),
                    None => None,
                };
                let joints = match attrs.get("JOINTS_0").and_then(Value::as_u64) {
                    Some(a) => Some(self.read_uints(a as usize)?),
                    None => None,
                };
                let weights = match attrs.get("WEIGHTS_0").and_then(Value::as_u64) {
                    Some(a) => Some(self.read_floats(a as usize)?),
                    None => None,
                };

                let indices = match prim.get("indices").and_then(Value::as_u64) {
                    Some(a) => self.read_uints(a as usize)?,
                    None => (0..positions.len() as u32).collect(),
                };
                if indices.len() % 3 != 0 {
                    return Err(bad("index count is not a multiple of three"));
                }

                // Material: one JMS entry per distinct glTF material.
                let mat_id = prim.get("material").and_then(Value::as_i64).unwrap_or(-1);
                let material = *material_slot.entry(mat_id).or_insert_with(|| {
                    let name = if mat_id >= 0 {
                        materials
                            .get(mat_id as usize)
                            .and_then(|m| m.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("default")
                            .to_owned()
                    } else {
                        "default".to_owned()
                    };
                    let slot = jms.materials.len();
                    jms.materials.push(JmsMaterial {
                        name: sanitise_material(&name),
                        material_name: format!(
                            "({slot}) {} {}",
                            opts.permutation, opts.region
                        ),
                    });
                    slot as i32
                });

                let base = jms.vertices.len() as u32;
                for i in 0..positions.len() {
                    let p = convert_point(mat_point(&place, [
                        positions[i][0],
                        positions[i][1],
                        positions[i][2],
                    ]));
                    let n = normals
                        .as_ref()
                        .map(|n| convert_dir(mat_dir(&place, [n[i][0], n[i][1], n[i][2]])))
                        .unwrap_or([0.0, 0.0, 1.0]);
                    let n = {
                        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
                        if len > 1e-12 {
                            [n[0] / len, n[1] / len, n[2] / len]
                        } else {
                            [0.0, 0.0, 1.0]
                        }
                    };

                    let mut node_sets: Vec<(i16, f32)> = Vec::new();
                    if let (Some(j), Some(w)) = (&joints, &weights) {
                        for (c, &weight) in w[i].iter().enumerate() {
                            if weight <= 0.0 {
                                continue;
                            }
                            let joint = j.get(i * 4 + c).copied().unwrap_or(0) as usize;
                            let target = skin_joints
                                .get(joint)
                                .and_then(|n| jms_index.get(*n))
                                .copied()
                                .filter(|v| *v != usize::MAX)
                                .unwrap_or(default_bind);
                            node_sets.push((target as i16, weight));
                        }
                    }
                    if node_sets.is_empty() {
                        node_sets.push((default_bind as i16, 1.0));
                    }
                    // `tool.exe` renormalises on the way in, but the
                    // corpus shows 191 shipped vertices whose weights do
                    // not sum to 1 — so do it here and leave nothing to
                    // chance.
                    let sum: f32 = node_sets.iter().map(|(_, w)| *w).sum();
                    if sum > 0.0 {
                        for slot in &mut node_sets {
                            slot.1 /= sum;
                        }
                    }
                    if node_sets.len() > 4 {
                        return Err(GltfError::Unsupported(
                            "more than four node influences on a vertex".into(),
                        ));
                    }

                    let mut uvs = Vec::new();
                    if let Some(u) = &uv0 {
                        uvs.push(RealPoint2d { x: u[i][0], y: u[i][1] });
                    }
                    if let Some(u) = &uv1 {
                        if uvs.is_empty() {
                            uvs.push(RealPoint2d::default());
                        }
                        uvs.push(RealPoint2d { x: u[i][0], y: u[i][1] });
                    }

                    jms.vertices.push(JmsVertex {
                        position: RealPoint3d { x: p[0], y: p[1], z: p[2] },
                        normal: RealVector3d { i: n[0], j: n[1], k: n[2] },
                        tangent: None,
                        binormal: None,
                        node_sets,
                        uvs,
                        color: color.as_ref().map(|c| RealPoint3d {
                            x: c[i][0],
                            y: c[i][1],
                            z: c[i][2],
                        }),
                    });
                }

                for tri in indices.chunks_exact(3) {
                    jms.triangles.push(JmsTriangle {
                        material,
                        v: [base + tri[0], base + tri[1], base + tri[2]],
                        region: 0,
                    });
                }
            }
        }

        if jms.materials.is_empty() {
            jms.materials.push(JmsMaterial {
                name: "default".into(),
                material_name: format!("(0) {} {}", opts.permutation, opts.region),
            });
        }
        Ok(jms)
    }

    fn read_mat4s(&self, accessor: usize) -> R<Vec<Mat4>> {
        let rows = self.read_floats_wide(accessor, 16)?;
        Ok(rows
            .chunks_exact(16)
            .map(|c| {
                let mut m = IDENTITY;
                m.copy_from_slice(c);
                m
            })
            .collect())
    }

    /// Like [`Document::read_floats`] but for wide types (MAT4), returned flat.
    fn read_floats_wide(&self, index: usize, comps: usize) -> R<Vec<f32>> {
        let acc = self.accessor(index)?;
        let count = acc.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
        let ctype = acc
            .get("componentType")
            .and_then(Value::as_u64)
            .ok_or_else(|| bad("accessor has no componentType"))? as u32;
        let size = component_size(ctype)?;
        let mut out = vec![0.0f32; count * comps];
        let Some(view_index) = acc.get("bufferView").and_then(Value::as_u64) else {
            return Ok(out);
        };
        let (data, stride) = self.view(view_index as usize, size * comps)?;
        let base = acc.get("byteOffset").and_then(Value::as_u64).unwrap_or(0) as usize;
        for i in 0..count {
            let at = base + i * stride;
            for c in 0..comps {
                let o = at + c * size;
                let raw = data
                    .get(o..o + size)
                    .ok_or_else(|| bad(format!("accessor {index} reads past its buffer view")))?;
                out[i * comps + c] = decode_component(ctype, raw, false)?;
            }
        }
        Ok(out)
    }

    /// World transform per node, from the scene roots down.
    fn world_transforms(&self, nodes: &[Value]) -> R<Vec<Mat4>> {
        let mut out = vec![IDENTITY; nodes.len()];
        let mut seen = vec![false; nodes.len()];
        let mut stack: Vec<(usize, Mat4)> = Vec::new();

        // Scene roots, or every node that is nobody's child.
        let scene = self.json.get("scene").and_then(Value::as_u64).unwrap_or(0) as usize;
        let roots: Vec<usize> = arr(&self.json, "scenes")
            .get(scene)
            .map(|s| arr(s, "nodes").iter().filter_map(|n| n.as_u64().map(|n| n as usize)).collect())
            .unwrap_or_default();
        let roots = if roots.is_empty() {
            let mut child = vec![false; nodes.len()];
            for node in nodes {
                for c in arr(node, "children") {
                    if let Some(c) = c.as_u64().map(|c| c as usize).filter(|c| *c < child.len()) {
                        child[c] = true;
                    }
                }
            }
            (0..nodes.len()).filter(|i| !child[*i]).collect()
        } else {
            roots
        };

        for r in roots {
            stack.push((r, IDENTITY));
        }
        while let Some((i, parent)) = stack.pop() {
            let Some(node) = nodes.get(i) else { continue };
            if seen[i] {
                // A cycle, or a node reachable twice. Take the first
                // placement and move on rather than looping forever.
                continue;
            }
            seen[i] = true;
            let m = mat_mul(&parent, &node_local_matrix(node));
            out[i] = m;
            for c in arr(node, "children") {
                if let Some(c) = c.as_u64() {
                    stack.push((c as usize, m));
                }
            }
        }
        Ok(out)
    }
}

fn node_name(node: &Value, index: usize) -> String {
    node.get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
        .map(|n| n.trim().to_owned())
        .unwrap_or_else(|| format!("node{index}"))
}

/// JMS material names are whitespace-delimited tokens inside a
/// tab-delimited file, so a space in a shader name would split it. The
/// corpus does contain spaces in material names, but only in files the
/// importer itself wrote; a converter has no reason to risk it.
fn sanitise_material(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect();
    if cleaned.is_empty() { "default".to_owned() } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A minimal glTF: one triangle, positions in a base64 buffer.
    fn one_triangle(y_up: [[f32; 3]; 3]) -> Vec<u8> {
        let mut bin = Vec::new();
        for p in y_up {
            for c in p {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let b64 = encode_base64(&bin);
        let doc = json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "name": "mesh_node"}],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "mode": 4}]}],
            "accessors": [{
                "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"
            }],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": bin.len()}],
            "buffers": [{"byteLength": bin.len(), "uri": format!("data:application/octet-stream;base64,{b64}")}]
        });
        serde_json::to_vec(&doc).unwrap()
    }

    fn encode_base64(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
        }
        out
    }

    #[test]
    fn base64_round_trips() {
        for case in [&b""[..], b"a", b"ab", b"abc", b"abcd", &[0u8, 255, 128, 1, 2][..]] {
            assert_eq!(decode_base64(&encode_base64(case)).unwrap(), case, "{case:?}");
        }
    }

    #[test]
    fn a_triangle_converts_and_the_axes_change() {
        // A triangle lying in glTF's XZ plane (its "ground"), which after
        // conversion must lie in Halo's XY plane.
        let bytes = one_triangle([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        let jms = jms_from_gltf(&bytes, None, &GltfOptions::default()).unwrap();
        assert_eq!(jms.triangles.len(), 1);
        assert_eq!(jms.vertices.len(), 3);
        // (x, y, z) -> (x, -z, y): the third vertex was +Z, so it becomes -Y.
        let v = &jms.vertices[2].position;
        assert!((v.x - 0.0).abs() < 1e-6, "{v:?}");
        assert!((v.y - -1.0).abs() < 1e-6, "{v:?}");
        assert!((v.z - 0.0).abs() < 1e-6, "{v:?}");
        // Every vertex is bound to a node, and JMS always has a skeleton.
        assert_eq!(jms.nodes.len(), 1);
        assert_eq!(jms.vertices[0].node_sets, vec![(0, 1.0)]);
    }

    #[test]
    fn the_axis_swap_can_be_turned_off() {
        let bytes = one_triangle([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        let opts = GltfOptions { y_up_to_z_up: false, ..Default::default() };
        let jms = jms_from_gltf(&bytes, None, &opts).unwrap();
        let v = &jms.vertices[2].position;
        assert!((v.z - 1.0).abs() < 1e-6, "z should be untouched: {v:?}");
    }

    #[test]
    fn scale_multiplies_positions() {
        let bytes = one_triangle([[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        let opts = GltfOptions { scale: 10.0, ..Default::default() };
        let jms = jms_from_gltf(&bytes, None, &opts).unwrap();
        assert!((jms.vertices[1].position.x - 20.0).abs() < 1e-4);
    }

    #[test]
    fn the_axis_change_preserves_handedness() {
        // determinant of the 3x3 must be +1, or winding flips and every
        // triangle faces inward.
        let m = Y_UP_TO_Z_UP;
        let det = m[0] * (m[5] * m[10] - m[9] * m[6]) - m[4] * (m[1] * m[10] - m[9] * m[2])
            + m[8] * (m[1] * m[6] - m[5] * m[2]);
        assert!((det - 1.0).abs() < 1e-6, "determinant is {det}, not +1");
    }

    #[test]
    fn affine_inverse_is_actually_an_inverse() {
        let m = node_local_matrix(&json!({
            "translation": [1.0, 2.0, 3.0],
            "rotation": [0.2, 0.3, 0.1, 0.927],
            "scale": [2.0, 2.0, 2.0]
        }));
        let product = mat_mul(&m, &mat_invert_affine(&m));
        for (i, expected) in IDENTITY.iter().enumerate() {
            assert!((product[i] - expected).abs() < 1e-4, "slot {i}: {}", product[i]);
        }
    }

    #[test]
    fn a_matrix_round_trips_through_the_quaternion() {
        let m = node_local_matrix(&json!({"rotation": [0.2, 0.3, 0.1, 0.927]}));
        let q = mat_to_quat(&m);
        let back = node_local_matrix(&json!({"rotation": [q.i, q.j, q.k, q.w]}));
        for i in [0, 1, 2, 4, 5, 6, 8, 9, 10] {
            assert!((m[i] - back[i]).abs() < 1e-3, "slot {i}: {} vs {}", m[i], back[i]);
        }
    }

    #[test]
    fn markers_come_from_hash_prefixed_nodes_and_leave_the_skeleton() {
        let doc = json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [
                {"name": "root", "children": [1]},
                {"name": "#primary_trigger", "translation": [0.0, 1.0, 0.0]}
            ],
            "meshes": [], "accessors": [], "bufferViews": [], "buffers": []
        });
        let jms = jms_from_gltf(&serde_json::to_vec(&doc).unwrap(), None, &GltfOptions::default())
            .unwrap();
        assert_eq!(jms.nodes.len(), 1, "the marker must not be a skeleton node");
        assert_eq!(jms.nodes[0].name, "root");
        assert_eq!(jms.markers.len(), 1);
        assert_eq!(jms.markers[0].name, "primary_trigger", "the '#' is stripped");
        assert_eq!(jms.markers[0].node_index, 0, "parented to the nearest real node");
        // +Y in glTF becomes +Z in Halo.
        assert!((jms.markers[0].translation.z - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_non_triangle_primitive_is_refused_not_approximated() {
        let mut doc: Value =
            serde_json::from_slice(&one_triangle([[0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]))
                .unwrap();
        doc["meshes"][0]["primitives"][0]["mode"] = json!(5); // TRIANGLE_STRIP
        let err = jms_from_gltf(&serde_json::to_vec(&doc).unwrap(), None, &GltfOptions::default())
            .unwrap_err();
        assert!(matches!(err, GltfError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn a_sparse_accessor_is_refused_not_silently_wrong() {
        let mut doc: Value =
            serde_json::from_slice(&one_triangle([[0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]))
                .unwrap();
        doc["accessors"][0]["sparse"] = json!({"count": 1});
        let err = jms_from_gltf(&serde_json::to_vec(&doc).unwrap(), None, &GltfOptions::default())
            .unwrap_err();
        assert!(matches!(err, GltfError::Unsupported(_)), "{err:?}");
    }

    #[test]
    fn glb_and_gltf_produce_the_same_scene() {
        let json_bytes = one_triangle([[0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        let from_json = jms_from_gltf(&json_bytes, None, &GltfOptions::default()).unwrap();

        // Wrap the identical JSON in a GLB container, JSON chunk only.
        let mut glb = Vec::new();
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        let padded = json_bytes.len().div_ceil(4) * 4;
        glb.extend_from_slice(&((12 + 8 + padded) as u32).to_le_bytes());
        glb.extend_from_slice(&(padded as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_bytes);
        glb.resize(12 + 8 + padded, b' ');
        let from_glb = jms_from_gltf(&glb, None, &GltfOptions::default()).unwrap();

        assert_eq!(from_glb.vertices.len(), from_json.vertices.len());
        assert_eq!(from_glb.triangles.len(), from_json.triangles.len());
        assert_eq!(
            from_glb.vertices[2].position.y,
            from_json.vertices[2].position.y
        );
    }

    #[test]
    fn material_labels_come_out_in_the_form_the_splitter_expects() {
        let mut doc: Value =
            serde_json::from_slice(&one_triangle([[0.0; 3], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]))
                .unwrap();
        doc["materials"] = json!([{"name": "hull plate"}]);
        doc["meshes"][0]["primitives"][0]["material"] = json!(0);
        let opts = GltfOptions {
            permutation: "base".into(),
            region: "body".into(),
            ..Default::default()
        };
        let jms =
            jms_from_gltf(&serde_json::to_vec(&doc).unwrap(), None, &opts).unwrap();
        assert_eq!(jms.materials.len(), 1);
        // Spaces would split the token, so they are folded.
        assert_eq!(jms.materials[0].name, "hull_plate");
        assert_eq!(jms.materials[0].material_name, "(0) base body");

        let label = crate::jms_split::MaterialLabel::parse(&jms.materials[0].material_name);
        assert_eq!(label.permutation, "base");
        assert_eq!(label.region, "body");
    }

    /// A two-joint skin, with the bind pose supplied by
    /// `inverseBindMatrices` and the joints deliberately *posed*
    /// elsewhere, so the test distinguishes "used the bind pose" from
    /// "used the current transform".
    #[test]
    fn skinning_uses_the_inverse_bind_matrices_not_the_posed_transform() {
        let mut bin: Vec<u8> = Vec::new();
        let push_f32 = |bin: &mut Vec<u8>, v: &[f32]| {
            for f in v {
                bin.extend_from_slice(&f.to_le_bytes());
            }
        };

        // positions: 3 verts
        let pos_off = bin.len();
        push_f32(&mut bin, &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        // joints: 3 x u16x4
        let joint_off = bin.len();
        for j in [[0u16, 0, 0, 0], [1, 0, 0, 0], [0, 1, 0, 0]] {
            for c in j {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        // weights: 3 x f32x4 — the last vertex is split 50/50, and the
        // second is deliberately unnormalised to prove renormalisation.
        let weight_off = bin.len();
        push_f32(&mut bin, &[1.0, 0.0, 0.0, 0.0]);
        push_f32(&mut bin, &[0.5, 0.0, 0.0, 0.0]);
        push_f32(&mut bin, &[0.5, 0.5, 0.0, 0.0]);
        // inverseBindMatrices: joint 0 identity, joint 1 = translate(0,-2,0)
        let ibm_off = bin.len();
        push_f32(&mut bin, &IDENTITY);
        let mut ibm1 = IDENTITY;
        ibm1[13] = -2.0;
        push_f32(&mut bin, &ibm1);

        let b64 = encode_base64(&bin);
        let doc = json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0, 2]}],
            "nodes": [
                // Joint 0, posed far away from its bind pose on purpose.
                {"name": "root", "children": [1], "translation": [99.0, 99.0, 99.0]},
                {"name": "tip"},
                {"name": "skinned_mesh", "mesh": 0, "skin": 0}
            ],
            "skins": [{"joints": [0, 1], "inverseBindMatrices": 3}],
            "meshes": [{"primitives": [{
                "attributes": {"POSITION": 0, "JOINTS_0": 1, "WEIGHTS_0": 2},
                "mode": 4
            }]}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5123, "count": 3, "type": "VEC4"},
                {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4"},
                {"bufferView": 3, "componentType": 5126, "count": 2, "type": "MAT4"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": pos_off,    "byteLength": joint_off - pos_off},
                {"buffer": 0, "byteOffset": joint_off,  "byteLength": weight_off - joint_off},
                {"buffer": 0, "byteOffset": weight_off, "byteLength": ibm_off - weight_off},
                {"buffer": 0, "byteOffset": ibm_off,    "byteLength": bin.len() - ibm_off}
            ],
            "buffers": [{"byteLength": bin.len(),
                         "uri": format!("data:application/octet-stream;base64,{b64}")}]
        });

        let jms =
            jms_from_gltf(&serde_json::to_vec(&doc).unwrap(), None, &GltfOptions::default())
                .unwrap();

        // Three skeleton nodes: two joints plus the mesh node.
        assert_eq!(jms.nodes.len(), 3);
        assert_eq!(jms.nodes[0].name, "root");
        assert_eq!(jms.nodes[1].name, "tip");
        assert_eq!(jms.nodes[1].parent, 0);

        // Joint 0's bind pose is the identity, NOT the (99,99,99) it is
        // posed at. This is the assertion the whole test exists for.
        let root = &jms.nodes[0].translation;
        assert!(
            root.x.abs() < 1e-4 && root.y.abs() < 1e-4 && root.z.abs() < 1e-4,
            "root should sit at its bind pose (origin), got {root:?}"
        );
        // Joint 1's bind pose is glTF (0, 2, 0) -> Halo (0, 0, 2).
        let tip = &jms.nodes[1].translation;
        assert!((tip.z - 2.0).abs() < 1e-4, "tip should be at z=2, got {tip:?}");

        // A skinned mesh ignores its node transform, so vertex 1 stays at
        // x = 1 rather than being displaced.
        assert!((jms.vertices[1].position.x - 1.0).abs() < 1e-5);

        // Influences map through skin.joints to JMS node indices, and
        // unnormalised weights are renormalised to sum to 1.
        assert_eq!(jms.vertices[0].node_sets, vec![(0i16, 1.0f32)]);
        assert_eq!(jms.vertices[1].node_sets, vec![(1i16, 1.0f32)], "0.5 alone must renormalise");
        assert_eq!(jms.vertices[2].node_sets, vec![(0i16, 0.5f32), (1i16, 0.5f32)]);
        for v in &jms.vertices {
            let sum: f32 = v.node_sets.iter().map(|(_, w)| *w).sum();
            assert!((sum - 1.0).abs() < 1e-5, "weights must sum to 1, got {sum}");
        }
    }

    #[test]
    fn a_converted_scene_writes_as_valid_jms_and_reparses() {
        let bytes = one_triangle([[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]]);
        let jms = jms_from_gltf(&bytes, None, &GltfOptions::default()).unwrap();
        let mut buf = Vec::new();
        jms.write(&mut buf, 8213).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let (back, version) = JmsFile::parse(&text).unwrap();
        assert_eq!(version, 8213);
        assert_eq!(back.triangles.len(), 1);
        assert_eq!(back.vertices.len(), 3);
        assert_eq!(back.nodes.len(), 1);
    }
}
