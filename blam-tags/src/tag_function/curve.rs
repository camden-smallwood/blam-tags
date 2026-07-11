//! The editable curve (`MultiSpline` / `c_multi_part_function_editor`) model.
//!
//! A curve graph is 1..=4 segments; each segment is a Linear (2 control
//! points), Spline, or Spline2 (4 control points) piece. Adjacent segments
//! share their join point. This mirrors the native 148-byte editor struct
//! `[count:i32][4 × 36-byte parts]`, part = `[type, corner_l, corner_r, pad]
//! [4 × real_point2d]`, control points at `part + 8`.
//!
//! `postprocess` compiles the control-point model to the on-disk compact
//! (`MultiPartCompact`: cubic coefficients + `ending_x`), matching the native
//! `c_multi_part_function_editor::postprocess @0x82e8f0f8` and the per-type
//! sub-editor `postprocess` routines (linear @0x82e8df00, spline @0x82e8e018,
//! spline2 @0x82e8e778).

/// Foundation segment types (`FunctionEditorSegmentType`). The compact stores
/// these as the function-type bytes Linear=4, Spline=7, Spline2=10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurveSegmentType {
    Linear,
    Spline,
    Spline2,
}

impl CurveSegmentType {
    pub(crate) fn from_type_byte(b: u8) -> Option<Self> {
        match b {
            4 => Some(Self::Linear),
            7 => Some(Self::Spline),
            10 => Some(Self::Spline2),
            _ => None,
        }
    }
    pub(crate) fn type_byte(self) -> u8 {
        match self {
            Self::Linear => 4,
            Self::Spline => 7,
            Self::Spline2 => 10,
        }
    }
    /// Editor control points for this segment kind (2 for Linear, else 4).
    pub(crate) fn point_count(self) -> usize {
        match self {
            Self::Linear => 2,
            Self::Spline | Self::Spline2 => 4,
        }
    }
    /// On-disk compact part size (header 8 + body).
    pub(crate) fn compact_part_size(self) -> usize {
        match self {
            Self::Linear => 16,
            Self::Spline => 24,
            Self::Spline2 => 36,
        }
    }
}

/// Corner (`FunctionEditorSegmentCornerType`): whether a join is a hard corner
/// or a smooth (tangent-continuous) transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurvePointMode {
    Corner,
    Smooth,
}

const EDITOR_PART_STRIDE: usize = 36;
pub(crate) const EDITOR_SIZE: usize = 4 + 4 * EDITOR_PART_STRIDE; // 148

#[derive(Debug, Clone)]
pub(crate) struct CurveSegment {
    pub seg_type: CurveSegmentType,
    /// `true` = smooth join, `false` = corner. Native editor byte at part+5/+6
    /// (1 = smooth). Left/right refer to the join on each side of the segment.
    pub smooth_left: bool,
    pub smooth_right: bool,
    /// Up to 4 control points (x, y). Linear uses `[0, 1]`; Spline/Spline2 use
    /// all four (0 = start, 1/2 = tangent handles, 3 = end).
    pub points: [(f32, f32); 4],
}

impl CurveSegment {
    fn point_count(&self) -> usize {
        self.seg_type.point_count()
    }
}

/// A single curve graph (one function graph slot).
#[derive(Debug, Clone)]
pub(crate) struct CurveGraph {
    pub segments: Vec<CurveSegment>,
}

impl CurveGraph {
    /// Parse a 148-byte editor struct into the segment model.
    pub(crate) fn from_editor_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < EDITOR_SIZE {
            return None;
        }
        let count = i32::from_le_bytes(b[0..4].try_into().ok()?);
        if !(1..=4).contains(&count) {
            return None;
        }
        let mut segments = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            let base = 4 + i * EDITOR_PART_STRIDE;
            let seg_type = CurveSegmentType::from_type_byte(b[base])?;
            let smooth_left = b[base + 1] != 0;
            let smooth_right = b[base + 2] != 0;
            let mut points = [(0.0f32, 0.0f32); 4];
            for (p, pt) in points.iter_mut().enumerate() {
                let o = base + 4 + p * 8;
                *pt = (
                    f32::from_le_bytes(b[o..o + 4].try_into().ok()?),
                    f32::from_le_bytes(b[o + 4..o + 8].try_into().ok()?),
                );
            }
            segments.push(CurveSegment {
                seg_type,
                smooth_left,
                smooth_right,
                points,
            });
        }
        Some(Self { segments })
    }

    /// Serialize the model back to a 148-byte editor struct.
    pub(crate) fn to_editor_bytes(&self) -> [u8; EDITOR_SIZE] {
        let mut b = [0u8; EDITOR_SIZE];
        let count = self.segments.len().min(4) as i32;
        b[0..4].copy_from_slice(&count.to_le_bytes());
        for (i, seg) in self.segments.iter().take(4).enumerate() {
            let base = 4 + i * EDITOR_PART_STRIDE;
            b[base] = seg.seg_type.type_byte();
            b[base + 1] = seg.smooth_left as u8;
            b[base + 2] = seg.smooth_right as u8;
            for (p, &(x, y)) in seg.points.iter().enumerate() {
                let o = base + 4 + p * 8;
                b[o..o + 4].copy_from_slice(&x.to_le_bytes());
                b[o + 4..o + 8].copy_from_slice(&y.to_le_bytes());
            }
        }
        b
    }

    // -- Navigation (ports `c_multi_part_function_editor`). --

    pub(crate) fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Total control-point count across the graph. Linear contributes 1 index,
    /// Spline/Spline2 contribute 3, plus 1 for the final endpoint.
    pub(crate) fn control_point_count(&self) -> usize {
        self.point_index_from_segment(self.segments.len()) + 1
    }

    /// The starting global control-point index of `segment`. Ports
    /// `point_index_from_segment @0x82e95ae0`.
    pub(crate) fn point_index_from_segment(&self, segment: usize) -> usize {
        let mut idx = 0usize;
        for seg in self.segments.iter().take(segment) {
            idx += match seg.seg_type {
                CurveSegmentType::Linear => 1,
                CurveSegmentType::Spline | CurveSegmentType::Spline2 => 3,
            };
        }
        idx
    }

    /// The (x, y) of global control point `index`. Ports
    /// `get_control_point @0x82e95798`: walk segments, Linear consumes 1 index
    /// (points 0/1), Spline/Spline2 consume 3 (points 0..3), sharing joins.
    pub(crate) fn get_control_point(&self, index: usize) -> Option<(f32, f32)> {
        let mut v = index as isize;
        for seg in &self.segments {
            match seg.seg_type {
                CurveSegmentType::Linear => {
                    if (0..2).contains(&v) {
                        return Some(seg.points[v as usize]);
                    }
                    v -= 1;
                }
                CurveSegmentType::Spline | CurveSegmentType::Spline2 => {
                    if (0..4).contains(&v) {
                        return Some(seg.points[v as usize]);
                    }
                    v -= 3;
                }
            }
        }
        None
    }

    /// Whether global control point `index` is a graph point (on-curve join)
    /// vs a spline tangent handle. Ports `is_graph_point @0x82e90978`.
    pub(crate) fn is_graph_point(&self, index: usize) -> Option<bool> {
        let mut v = index as isize;
        for seg in &self.segments {
            match seg.seg_type {
                CurveSegmentType::Linear => {
                    if (0..2).contains(&v) {
                        return Some(true);
                    }
                    v -= 1;
                }
                CurveSegmentType::Spline | CurveSegmentType::Spline2 => {
                    if (0..4).contains(&v) {
                        return Some(v == 0 || v == 3);
                    }
                    v -= 3;
                }
            }
        }
        None
    }

    /// All `(segment, local_point)` locations of global control point `index`.
    /// A graph point at a segment join lives in two segments (end of one,
    /// start of the next); setting it must update every location.
    fn point_locations(&self, index: usize) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (i, seg) in self.segments.iter().enumerate() {
            let base = self.point_index_from_segment(i);
            for p in 0..seg.point_count() {
                if base + p == index {
                    out.push((i, p));
                }
            }
        }
        out
    }

    /// Set the global control point `index` to `(x, y)` across all its
    /// locations (keeps shared joins consistent). Ports the shared-join
    /// behaviour of `set_control_point_x/y @0x82e8ffa8 / 0x82e964d8`.
    pub(crate) fn set_control_point(&mut self, index: usize, x: f32, y: f32) {
        for (i, p) in self.point_locations(index) {
            self.segments[i].points[p] = (x, y);
        }
    }

    /// Change a segment's type, adapting its control points: growing Linear →
    /// Spline/Spline2 synthesizes interior handles at 1/3 and 2/3; shrinking to
    /// Linear keeps the endpoints.
    pub(crate) fn set_segment_type(&mut self, seg: usize, new_type: CurveSegmentType) {
        let Some(s) = self.segments.get_mut(seg) else { return };
        if s.seg_type == new_type {
            return;
        }
        let old_last = s.seg_type.point_count() - 1;
        let (x0, y0) = s.points[0];
        let (x1, y1) = s.points[old_last];
        s.seg_type = new_type;
        if new_type == CurveSegmentType::Linear {
            s.points[1] = (x1, y1);
        } else {
            // Endpoints at 0 and 3; interior handles evenly spaced.
            s.points[0] = (x0, y0);
            s.points[1] = (x0 + (x1 - x0) / 3.0, y0 + (y1 - y0) / 3.0);
            s.points[2] = (x0 + 2.0 * (x1 - x0) / 3.0, y0 + 2.0 * (y1 - y0) / 3.0);
            s.points[3] = (x1, y1);
        }
    }

    /// Corner/smooth mode of the join *before* segment `seg` (its left join).
    pub(crate) fn segment_left_mode(&self, seg: usize) -> Option<CurvePointMode> {
        self.segments.get(seg).map(|s| {
            if s.smooth_left {
                CurvePointMode::Smooth
            } else {
                CurvePointMode::Corner
            }
        })
    }

    /// Set the corner/smooth mode of the join between segment `seg-1` and
    /// `seg` (both sides of the shared join are updated).
    pub(crate) fn set_join_mode(&mut self, seg: usize, mode: CurvePointMode) {
        let smooth = mode == CurvePointMode::Smooth;
        if seg < self.segments.len() {
            self.segments[seg].smooth_left = smooth;
        }
        if seg > 0 {
            if let Some(prev) = self.segments.get_mut(seg - 1) {
                prev.smooth_right = smooth;
            }
        }
    }

    /// Insert a graph point at `x`, splitting the segment that contains it
    /// into two Linear pieces (max 4 segments). Returns whether a point was
    /// added. Ports the intent of `add_point @0x82e94008` (a linear split;
    /// spline subdivision is a later refinement).
    pub(crate) fn insert_point(&mut self, x: f32) -> bool {
        if self.segments.len() >= 4 {
            return false;
        }
        let x = x.clamp(0.0, 1.0);
        for i in 0..self.segments.len() {
            let last = self.segments[i].seg_type.point_count() - 1;
            let (sx, sy) = self.segments[i].points[0];
            let (ex, ey) = self.segments[i].points[last];
            if x > sx && x < ex {
                let t = (x - sx) / (ex - sx);
                let y = sy + (ey - sy) * t;
                // Left half keeps the original start; right half the end.
                let left = CurveSegment {
                    seg_type: CurveSegmentType::Linear,
                    smooth_left: self.segments[i].smooth_left,
                    smooth_right: false,
                    points: [(sx, sy), (x, y), (0.0, 0.0), (0.0, 0.0)],
                };
                let right = CurveSegment {
                    seg_type: CurveSegmentType::Linear,
                    smooth_left: false,
                    smooth_right: self.segments[i].smooth_right,
                    points: [(x, y), (ex, ey), (0.0, 0.0), (0.0, 0.0)],
                };
                self.segments[i] = left;
                self.segments.insert(i + 1, right);
                return true;
            }
        }
        false
    }

    /// Delete the graph point `index` by merging the two segments that meet at
    /// it (keeps a minimum of one segment). Ports the intent of
    /// `delete_point @0x82e94c20`.
    pub(crate) fn delete_point(&mut self, index: usize) -> bool {
        if self.segments.len() <= 1 {
            return false;
        }
        // Find the interior join (end of segment i, index == start of i+1).
        for i in 0..self.segments.len().saturating_sub(1) {
            let join_index = self.point_index_from_segment(i + 1);
            if join_index == index {
                let last = self.segments[i + 1].seg_type.point_count() - 1;
                let end = self.segments[i + 1].points[last];
                let smooth_right = self.segments[i + 1].smooth_right;
                // Collapse segment i+1 into i as a Linear span.
                let start = self.segments[i].points[0];
                self.segments[i] = CurveSegment {
                    seg_type: CurveSegmentType::Linear,
                    smooth_left: self.segments[i].smooth_left,
                    smooth_right,
                    points: [start, end, (0.0, 0.0), (0.0, 0.0)],
                };
                self.segments.remove(i + 1);
                return true;
            }
        }
        false
    }

    // -- Compile: model → on-disk MultiPart compact. --

    /// Run the native postprocess sequence and produce the compact bytes:
    /// derive corner flags, clamp endpoints to x∈[0,1], enforce monotonic x,
    /// then per-segment compute coefficients and `ending_x` (last = FLT_MAX).
    /// Mirrors `c_multi_part_function_editor::postprocess @0x82e8f0f8`.
    pub(crate) fn postprocess_to_compact(&mut self) -> Vec<u8> {
        let n = self.segments.len();
        // 1. Corner-flag normalization. The native postprocess
        //    (@0x82e8f0f8) force-clears the smooth flag at the graph ends and
        //    at any join touching a Linear segment (those genuinely cannot be
        //    smooth). We diverge from its `else if (!flag) flag = 1`
        //    auto-promotion, which would silently override an explicit Corner
        //    the user just set — instead we PRESERVE the stored flag at
        //    interior non-linear joins so Corner/Smooth is a real toggle.
        for i in 0..n {
            let this_linear = self.segments[i].seg_type == CurveSegmentType::Linear;
            let next_linear = i + 1 < n && self.segments[i + 1].seg_type == CurveSegmentType::Linear;
            let prev_linear = i > 0 && self.segments[i - 1].seg_type == CurveSegmentType::Linear;
            let seg = &mut self.segments[i];
            if i + 1 >= n || next_linear || this_linear {
                seg.smooth_right = false;
            }
            if i == 0 || prev_linear || this_linear {
                seg.smooth_left = false;
            }
        }
        // 2. Clamp the graph endpoints to x = 0 and x = 1.
        if let Some(first) = self.segments.first_mut() {
            first.points[0].0 = 0.0;
        }
        if let Some(last) = self.segments.last_mut() {
            let lp = last.point_count() - 1;
            last.points[lp].0 = 1.0;
        }
        // 3. Monotonic-x + per-segment compile.
        let mut out = Vec::new();
        out.extend_from_slice(&(n as i32).to_le_bytes());
        let mut running_x = 0.0f32;
        for i in 0..n {
            let seg_type = self.segments[i].seg_type;
            let last_pt = seg_type.point_count() - 1;
            // Bump start / end x to keep the segment strictly increasing.
            if self.segments[i].points[0].0 < running_x {
                self.segments[i].points[0].0 = running_x;
            }
            if self.segments[i].points[last_pt].0 < running_x {
                self.segments[i].points[last_pt].0 = running_x + 0.001;
            }
            let body = match seg_type {
                CurveSegmentType::Linear => linear_compact(self.segments[i].points),
                CurveSegmentType::Spline => spline_compact(self.segments[i].points),
                CurveSegmentType::Spline2 => spline2_compact(self.segments[i].points),
            };
            running_x = self.segments[i].points[last_pt].0;
            let ending_x = if i == n - 1 { f32::MAX } else { running_x };
            out.push(seg_type.type_byte());
            out.extend_from_slice(&[0, 0, 0]);
            out.extend_from_slice(&ending_x.to_le_bytes());
            out.extend_from_slice(&body);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Per-segment coefficient math (ports the sub-editor postprocess routines).
// ---------------------------------------------------------------------------

/// Linear: `slope = (y1 - y0)/(x1 - x0)`, `offset = y0 - slope·x0`.
/// Ports `c_linear_function_editor::postprocess @0x82e8df00`.
fn linear_compact(p: [(f32, f32); 4]) -> Vec<u8> {
    let (x0, y0) = p[0];
    let (x1, y1) = p[1];
    let dx = x1 - x0;
    let (slope, offset) = if dx.abs() >= 1e-4 {
        let slope = (y1 - y0) / dx;
        (slope, y0 - slope * x0)
    } else {
        (0.0, y0)
    };
    let mut b = Vec::with_capacity(8);
    b.extend_from_slice(&slope.to_le_bytes());
    b.extend_from_slice(&offset.to_le_bytes());
    b
}

/// Spline: a global-x cubic `i·x³ + j·x² + k·x + l` through the four control
/// points. Endpoints `p0, p3`; tangent handles `p1, p2`. Equivalent to the
/// native `c_spline_function_editor::postprocess @0x82e8e018` (unit-domain
/// Hermite + global renorm), computed here by exact affine composition.
fn spline_compact(p: [(f32, f32); 4]) -> Vec<u8> {
    let (i, j, k, l) = spline_global_cubic(p);
    let mut b = Vec::with_capacity(16);
    b.extend_from_slice(&i.to_le_bytes());
    b.extend_from_slice(&j.to_le_bytes());
    b.extend_from_slice(&k.to_le_bytes());
    b.extend_from_slice(&l.to_le_bytes());
    b
}

/// The cubic power-basis coefficients (global x) for a spline segment.
fn spline_global_cubic(p: [(f32, f32); 4]) -> (f32, f32, f32, f32) {
    let (x0, y0) = p[0];
    let (x1p, y1p) = p[1];
    let (x2p, y2p) = p[2];
    let (x3, y3) = p[3];
    let w = x3 - x0;
    if w.abs() < 1e-6 {
        return (0.0, 0.0, 0.0, y0);
    }
    // Global tangents from the Bezier-style handles.
    let m0 = if (x1p - x0).abs() > 1e-6 { (y1p - y0) / (x1p - x0) } else { 0.0 };
    let m1 = if (x3 - x2p).abs() > 1e-6 { (y3 - y2p) / (x3 - x2p) } else { 0.0 };
    // Unit-domain (t = (x-x0)/w) Hermite cubic with tangents scaled by width.
    let cap_m0 = m0 * w;
    let cap_m1 = m1 * w;
    let a_t = 2.0 * y0 - 2.0 * y3 + cap_m0 + cap_m1;
    let b_t = -3.0 * y0 + 3.0 * y3 - 2.0 * cap_m0 - cap_m1;
    let c_t = cap_m0;
    let d_t = y0;
    // Compose t = s·x + r with s = 1/w, r = -x0/w → global power basis.
    let s = 1.0 / w;
    let r = -x0 / w;
    let i = a_t * s * s * s;
    let j = 3.0 * a_t * s * s * r + b_t * s * s;
    let k = 3.0 * a_t * s * r * r + 2.0 * b_t * s * r + c_t * s;
    let l = a_t * r * r * r + b_t * r * r + c_t * r + d_t;
    (i, j, k, l)
}

/// Spline2: a unit-domain Hermite cubic plus `left_x`, `width`, `bias`, where
/// the compact remaps input internally. Ports
/// `c_spline2_function_editor::postprocess @0x82e8e778`.
fn spline2_compact(p: [(f32, f32); 4]) -> Vec<u8> {
    let (x0, y0) = p[0];
    let (x1p, y1p) = p[1];
    let (x2p, y2p) = p[2];
    let (x3, y3) = p[3];
    let w = x3 - x0;
    // Left tangent (unit-domain), right tangent (unit-domain).
    let m0 = if w.abs() >= 1e-4 {
        let run = (x1p - x0) / w;
        if run.abs() >= 1e-4 { (y1p - y0) / run } else { 0.0 }
    } else {
        0.0
    };
    let m1 = if w.abs() >= 1e-4 {
        let run = (x2p - x3) / w;
        if run.abs() >= 1e-4 { (y2p - y3) / run } else { 0.0 }
    } else {
        0.0
    };
    let i = 2.0 * y0 - 2.0 * y3 + m0 + m1;
    let j = -3.0 * y0 + 3.0 * y3 - 2.0 * m0 - m1;
    let k = m0;
    let l = y0;
    // bias = sqrt(dist²(p2,p3) / dist²(p0,p1)).
    let d_right = dist_sq((x2p, y2p), (x3, y3));
    let d_left = dist_sq((x0, y0), (x1p, y1p));
    let bias = if d_right.abs() >= 1e-4 { (d_left / d_right).sqrt() } else { 0.0 };
    let left_x = x0;
    let width = x3 - x0;
    let mut b = Vec::with_capacity(28);
    for v in [i, j, k, l, left_x, width, bias] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b
}

fn dist_sq(a: (f32, f32), b: (f32, f32)) -> f32 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tag_function::build_identity_multipart_bytes;

    fn eval_cubic(c: (f32, f32, f32, f32), x: f32) -> f32 {
        c.0 * x * x * x + c.1 * x * x + c.2 * x + c.3
    }

    #[test]
    fn identity_editor_parses_to_one_linear_segment() {
        let (_compact, editor) = build_identity_multipart_bytes();
        let g = CurveGraph::from_editor_bytes(&editor).unwrap();
        assert_eq!(g.segment_count(), 1);
        assert_eq!(g.segments[0].seg_type, CurveSegmentType::Linear);
        assert_eq!(g.segments[0].points[0], (0.0, 0.0));
        assert_eq!(g.segments[0].points[1], (1.0, 1.0));
        assert_eq!(g.control_point_count(), 2);
        assert_eq!(g.get_control_point(0), Some((0.0, 0.0)));
        assert_eq!(g.get_control_point(1), Some((1.0, 1.0)));
        assert_eq!(g.is_graph_point(0), Some(true));
        assert_eq!(g.is_graph_point(1), Some(true));
    }

    #[test]
    fn identity_postprocess_matches_reference_compact() {
        let (compact, editor) = build_identity_multipart_bytes();
        let mut g = CurveGraph::from_editor_bytes(&editor).unwrap();
        assert_eq!(g.postprocess_to_compact(), compact);
    }

    #[test]
    fn editor_bytes_roundtrip() {
        let (_c, editor) = build_identity_multipart_bytes();
        let g = CurveGraph::from_editor_bytes(&editor).unwrap();
        assert_eq!(&g.to_editor_bytes()[..], &editor[..]);
    }

    #[test]
    fn spline_cubic_passes_through_endpoints() {
        let pts = [(0.2f32, 0.1f32), (0.4, 0.5), (0.6, 0.9), (0.8, 0.7)];
        let c = spline_global_cubic(pts);
        assert!((eval_cubic(c, 0.2) - 0.1).abs() < 1e-4, "f(x0) wrong: {}", eval_cubic(c, 0.2));
        assert!((eval_cubic(c, 0.8) - 0.7).abs() < 1e-4, "f(x3) wrong: {}", eval_cubic(c, 0.8));
    }

    #[test]
    fn two_linear_segments_navigation() {
        let seg = |a: (f32, f32), b: (f32, f32)| CurveSegment {
            seg_type: CurveSegmentType::Linear,
            smooth_left: false,
            smooth_right: false,
            points: [a, b, (0.0, 0.0), (0.0, 0.0)],
        };
        let g = CurveGraph {
            segments: vec![seg((0.0, 0.0), (0.5, 0.5)), seg((0.5, 0.5), (1.0, 1.0))],
        };
        assert_eq!(g.control_point_count(), 3);
        assert_eq!(g.point_index_from_segment(1), 1);
        assert_eq!(g.get_control_point(0), Some((0.0, 0.0)));
        assert_eq!(g.get_control_point(2), Some((1.0, 1.0)));
    }
}
