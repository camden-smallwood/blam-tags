//! Foundation-compatible editing surface for `mapping_function`
//! (`c_function_definition_editor`).
//!
//! This is the editable counterpart to the read/eval [`TagFunction`]. It
//! mirrors the native `c_function_definition_editor` (Reach `tag_debug`
//! XEX): a thin editor over the tag-data blob that keeps the compact data
//! and its editor-data trailer consistent, then serializes a complete,
//! valid blob on demand — Baboon never patches compact bytes itself.
//!
//! On-disk layout (see [`TagFunction`] and the Phase-0 findings):
//! `header[32] + compact[compact_size] + editor_trailer`, where the trailer
//! is `graph_count` fixed-size editor structs. For transition/periodic/
//! exponent the editor bytes equal the compact; only multi_part carries a
//! distinct 148-byte editor (control points + corner flags). The full rich
//! curve-segment model lives in the follow-up phase; this module delivers
//! the header/graph structure, range enable/disable, and master-type
//! conversion (including a valid identity `MultiSpline`).

use super::curve::{CurveGraph, CurvePointMode, CurveSegmentType, EDITOR_SIZE};
use super::{
    build_identity_multipart_bytes, ColorGraphType, FunctionFlags, FunctionKind, FunctionType,
    TagFunction, TagFunctionError,
};

/// Foundation's periodic-function option table (numeric index → label).
/// Baboon preserves any higher/unknown serialized index but only offers these.
pub const PERIODIC_FUNCTIONS: [&str; 12] = [
    "one",
    "zero",
    "cosine",
    "cosine (variable period)",
    "diagonal wave",
    "diagonal wave (variable period)",
    "slide",
    "slide (variable period)",
    "noise",
    "jitter",
    "wander",
    "spark",
];

/// Foundation's transition-function option table (numeric index → label).
pub const TRANSITION_FUNCTIONS: [&str; 4] = ["linear", "early", "very early", "late"];

/// Physical header color-slot mapping per color count (the non-contiguous
/// layout `byte_140CDE670`): 1→[0], 2→[0,3], 3→[0,1,3], 4→[0,1,2,3].
pub(crate) fn color_slots(cgt: ColorGraphType) -> &'static [usize] {
    match cgt {
        ColorGraphType::Scalar => &[],
        ColorGraphType::OneColor => &[0],
        ColorGraphType::TwoColor => &[0, 3],
        ColorGraphType::ThreeColor => &[0, 1, 3],
        ColorGraphType::FourColor => &[0, 1, 2, 3],
    }
}

/// Typed parameters of a Periodic graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PeriodicParams {
    pub function_index: u8,
    pub frequency: f32,
    pub phase: f32,
    pub amplitude_min: f32,
    pub amplitude_max: f32,
}

/// Typed parameters of an Exponent graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExponentParams {
    pub exponent: f32,
    pub amplitude_min: f32,
    pub amplitude_max: f32,
}

/// Typed parameters of a Transition graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionParams {
    pub function_index: u8,
    pub amplitude_min: f32,
    pub amplitude_max: f32,
}

impl PeriodicParams {
    fn to_compact(self) -> Vec<u8> {
        let mut b = vec![self.function_index, 0, 0, 0];
        b.extend_from_slice(&self.frequency.to_le_bytes());
        b.extend_from_slice(&self.phase.to_le_bytes());
        b.extend_from_slice(&self.amplitude_min.to_le_bytes());
        b.extend_from_slice(&self.amplitude_max.to_le_bytes());
        b
    }
}

impl ExponentParams {
    fn to_compact(self) -> Vec<u8> {
        let mut b = Vec::with_capacity(12);
        b.extend_from_slice(&self.amplitude_min.to_le_bytes());
        b.extend_from_slice(&self.amplitude_max.to_le_bytes());
        b.extend_from_slice(&self.exponent.to_le_bytes());
        b
    }
}

impl TransitionParams {
    fn to_compact(self) -> Vec<u8> {
        let mut b = vec![self.function_index, 0, 0, 0];
        b.extend_from_slice(&self.amplitude_min.to_le_bytes());
        b.extend_from_slice(&self.amplitude_max.to_le_bytes());
        b
    }
}

/// Foundation's five user-facing "master" function types (the top-level
/// picker). Several on-disk curve forms collapse to `Curve`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundationMasterType {
    Basic,
    Curve,
    Periodic,
    Exponent,
    Transition,
}

impl FoundationMasterType {
    /// The Foundation master type a raw [`FunctionType`] presents as.
    pub fn from_function_type(kind: FunctionType) -> Self {
        match kind {
            FunctionType::Constant => Self::Basic,
            FunctionType::Periodic => Self::Periodic,
            FunctionType::Exponent => Self::Exponent,
            FunctionType::Transition => Self::Transition,
            _ => Self::Curve,
        }
    }

    /// The concrete [`FunctionType`] this master type serializes as.
    pub fn function_type(self) -> FunctionType {
        match self {
            Self::Basic => FunctionType::Constant,
            Self::Curve => FunctionType::MultiSpline,
            Self::Periodic => FunctionType::Periodic,
            Self::Exponent => FunctionType::Exponent,
            Self::Transition => FunctionType::Transition,
        }
    }
}

#[derive(Debug)]
pub enum FunctionEditError {
    /// A rebuilt blob failed to re-parse (should not happen; indicates a bug
    /// in the byte construction).
    Serialize(TagFunctionError),
    /// The operation isn't valid for this graph slot / function type.
    InvalidOperation(&'static str),
}

impl std::fmt::Display for FunctionEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(e) => write!(f, "function editor serialize error: {e}"),
            Self::InvalidOperation(m) => write!(f, "invalid function edit: {m}"),
        }
    }
}

impl std::error::Error for FunctionEditError {}

/// Editable `mapping_function`. Wraps a [`TagFunction`]; structural edits
/// rebuild a complete, valid blob (compact + editor trailer) and re-parse,
/// so [`Self::to_bytes`] always yields a round-trippable result.
#[derive(Debug, Clone)]
pub struct TagFunctionEditor {
    func: TagFunction,
}

impl TagFunctionEditor {
    pub fn from_function(func: TagFunction) -> Self {
        Self { func }
    }

    pub fn parse(data: &[u8]) -> Result<Self, TagFunctionError> {
        Ok(Self::from_function(TagFunction::parse(data)?))
    }

    /// Borrow the underlying function (for evaluation / read-only queries).
    pub fn function(&self) -> &TagFunction {
        &self.func
    }

    pub fn into_function(self) -> TagFunction {
        self.func
    }

    /// Serialize to a complete `mapping_function` `data` blob.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.func.to_bytes()
    }

    // -- Queries (delegate to TagFunction / header). --

    pub fn master_type(&self) -> FoundationMasterType {
        FoundationMasterType::from_function_type(self.func.function_type())
    }

    pub fn function_type(&self) -> FunctionType {
        self.func.function_type()
    }

    pub fn color_graph_type(&self) -> ColorGraphType {
        self.func.color_graph_type()
    }

    pub fn is_ranged(&self) -> bool {
        self.func.is_ranged()
    }

    pub fn graph_count(&self) -> usize {
        self.func.graph_count()
    }

    pub fn is_clamped(&self) -> bool {
        self.func.is_clamped()
    }

    pub fn is_cyclic(&self) -> bool {
        self.func.is_cyclic()
    }

    pub fn is_exclusion(&self) -> bool {
        self.func.is_exclusion()
    }

    pub fn color_count(&self) -> usize {
        self.func.color_count()
    }

    // -- Structural edits. --

    /// Enable or disable the ranged second graph.
    ///
    /// Enabling a non-constant function creates a valid second compact that
    /// duplicates the primary graph (Foundation's `add_function`); disabling
    /// removes it. Constants only carry the RANGE flag (no second compact),
    /// matching `c_function_definition::get_ranged` behaviour.
    pub fn set_ranged(&mut self, ranged: bool) -> Result<(), FunctionEditError> {
        if self.is_ranged() == ranged {
            return Ok(());
        }
        let ftype = self.func.function_type();
        if matches!(ftype, FunctionType::Constant | FunctionType::Identity) {
            // No compact/editor to duplicate — just flip the flag.
            let mut func = self.func.clone();
            func.set_flag(FunctionFlags::RANGE, ranged);
            self.func = func;
            return Ok(());
        }
        self.rebuild(self.master_type(), ranged)
    }

    /// Convert the whole function to a Foundation master type, preserving the
    /// common header (flags, color graph type, colors, clamp range,
    /// exclusion) and the ranged state where representable. Converting to
    /// `Curve` produces a valid one-segment identity `MultiSpline`
    /// (`f(x) = x`), not the empty compact the old path produced.
    pub fn set_master_type(&mut self, target: FoundationMasterType) -> Result<(), FunctionEditError> {
        if self.master_type() == target {
            return Ok(());
        }
        let ranged = self.is_ranged() && target != FoundationMasterType::Basic;
        self.rebuild(target, ranged)
    }

    // -- Option tables + color slots (doc item 5 + 6). --

    pub fn periodic_function_count(&self) -> usize {
        PERIODIC_FUNCTIONS.len()
    }

    pub fn periodic_function_text(&self, index: usize) -> Option<&'static str> {
        PERIODIC_FUNCTIONS.get(index).copied()
    }

    pub fn transition_function_count(&self) -> usize {
        TRANSITION_FUNCTIONS.len()
    }

    pub fn transition_function_text(&self, index: usize) -> Option<&'static str> {
        TRANSITION_FUNCTIONS.get(index).copied()
    }

    /// The ARGB color at logical color index `index` (0..`color_count`),
    /// resolving the non-contiguous physical slot. Ports
    /// `c_function_definition::get_color @0x82e8c978`.
    pub fn get_color(&self, index: usize) -> Option<u32> {
        let slot = *color_slots(self.color_graph_type()).get(index)?;
        Some(self.func.header().colors[slot])
    }

    /// Set the color at logical index `index`, preserving all untouched color
    /// slots and unrelated header bytes.
    pub fn set_color(&mut self, index: usize, argb: u32) -> Result<(), FunctionEditError> {
        let slot = *color_slots(self.color_graph_type())
            .get(index)
            .ok_or(FunctionEditError::InvalidOperation("color index out of range"))?;
        self.func.set_color(slot, argb);
        Ok(())
    }

    /// Change the color-graph type (scalar / N-color).
    pub fn set_color_graph_type(&mut self, cgt: ColorGraphType) {
        self.func.set_color_graph_type(cgt);
    }

    // -- Typed compact params per graph (doc item 3). --

    /// Periodic parameters for graph `slot` (0 = primary, 1 = ranged second).
    pub fn periodic_params(&self, slot: usize) -> Option<PeriodicParams> {
        match self.func.graph(slot)? {
            FunctionKind::Periodic { compact, .. } => Some(PeriodicParams {
                function_index: compact.function_index,
                frequency: compact.frequency,
                phase: compact.phase,
                amplitude_min: compact.amplitude_min,
                amplitude_max: compact.amplitude_max,
            }),
            _ => None,
        }
    }

    pub fn set_periodic_params(
        &mut self,
        slot: usize,
        params: PeriodicParams,
    ) -> Result<(), FunctionEditError> {
        self.set_typed_graph(slot, FunctionType::Periodic, params.to_compact())
    }

    pub fn exponent_params(&self, slot: usize) -> Option<ExponentParams> {
        match self.func.graph(slot)? {
            FunctionKind::Exponent { compact, .. } => Some(ExponentParams {
                exponent: compact.exponent,
                amplitude_min: compact.amplitude_min,
                amplitude_max: compact.amplitude_max,
            }),
            _ => None,
        }
    }

    pub fn set_exponent_params(
        &mut self,
        slot: usize,
        params: ExponentParams,
    ) -> Result<(), FunctionEditError> {
        self.set_typed_graph(slot, FunctionType::Exponent, params.to_compact())
    }

    pub fn transition_params(&self, slot: usize) -> Option<TransitionParams> {
        match self.func.graph(slot)? {
            FunctionKind::Transition { compact, .. } => Some(TransitionParams {
                function_index: compact.function_index,
                amplitude_min: compact.amplitude_min,
                amplitude_max: compact.amplitude_max,
            }),
            _ => None,
        }
    }

    pub fn set_transition_params(
        &mut self,
        slot: usize,
        params: TransitionParams,
    ) -> Result<(), FunctionEditError> {
        self.set_typed_graph(slot, FunctionType::Transition, params.to_compact())
    }

    /// Replace graph `slot`'s compact bytes (and its mirrored editor) with
    /// `new_compact`, preserving the other graph, then rebuild + re-parse.
    /// Valid only for the fixed-size mirror types (periodic/exponent/
    /// transition) where the editor trailer equals the compact.
    fn set_typed_graph(
        &mut self,
        slot: usize,
        expected: FunctionType,
        new_compact: Vec<u8>,
    ) -> Result<(), FunctionEditError> {
        if self.func.function_type() != expected {
            return Err(FunctionEditError::InvalidOperation("wrong function type for params"));
        }
        let graphs = self.graph_count();
        if slot >= graphs {
            return Err(FunctionEditError::InvalidOperation("graph slot out of range"));
        }
        let mut compacts: Vec<Vec<u8>> = (0..graphs)
            .map(|g| self.func.graph_compact_bytes(g).unwrap_or_default())
            .collect();
        compacts[slot] = new_compact;

        let mut header = self.func.header_bytes();
        let mut flags = FunctionFlags(header[1]);
        flags.0 &= !FunctionFlags::OPTIMIZED;
        header[1] = flags.0;
        let compact_total: usize = compacts.iter().map(Vec::len).sum();
        header[28..32].copy_from_slice(&(compact_total as i32).to_le_bytes());

        let mut blob = Vec::with_capacity(32 + 2 * compact_total);
        blob.extend_from_slice(&header);
        for c in &compacts {
            blob.extend_from_slice(c);
        }
        // Editor trailer mirrors the compacts for these types.
        for c in &compacts {
            blob.extend_from_slice(c);
        }
        self.func = TagFunction::parse(&blob).map_err(FunctionEditError::Serialize)?;
        Ok(())
    }

    // -- Curve (MultiSpline) segment editing (doc item 2). --

    /// Parse the per-graph curve models from the editor-data trailer. `None`
    /// unless this is a `MultiSpline`.
    fn curve_graphs(&self) -> Option<Vec<CurveGraph>> {
        if self.func.function_type() != FunctionType::MultiSpline {
            return None;
        }
        let data = self.func.editor_data();
        let graphs = self.graph_count();
        let mut out = Vec::with_capacity(graphs);
        for g in 0..graphs {
            let s = g * EDITOR_SIZE;
            let slice = data.get(s..s + EDITOR_SIZE)?;
            out.push(CurveGraph::from_editor_bytes(slice)?);
        }
        Some(out)
    }

    /// Rebuild the whole blob from post-processed curve graphs (compile each to
    /// its compact + editor), preserving the header. Re-parses into `self.func`.
    fn apply_curve_graphs(&mut self, mut graphs: Vec<CurveGraph>) -> Result<(), FunctionEditError> {
        let mut compact_region = Vec::new();
        let mut editor_region = Vec::new();
        for g in &mut graphs {
            // postprocess mutates the graph (corner flags, clamp, monotonic x),
            // so compile the compact first, then serialize the settled editor.
            let compact = g.postprocess_to_compact();
            compact_region.extend_from_slice(&compact);
            editor_region.extend_from_slice(&g.to_editor_bytes());
        }
        let mut header = self.func.header_bytes();
        header[0] = FunctionType::MultiSpline as u8;
        let mut flags = FunctionFlags(header[1]);
        flags.0 &= !FunctionFlags::OPTIMIZED;
        header[1] = flags.0;
        header[28..32].copy_from_slice(&(compact_region.len() as i32).to_le_bytes());
        let mut blob = Vec::with_capacity(32 + compact_region.len() + editor_region.len());
        blob.extend_from_slice(&header);
        blob.extend_from_slice(&compact_region);
        blob.extend_from_slice(&editor_region);
        self.func = TagFunction::parse(&blob).map_err(FunctionEditError::Serialize)?;
        Ok(())
    }

    fn graph(&self, graph: usize) -> Result<CurveGraph, FunctionEditError> {
        self.curve_graphs()
            .and_then(|g| g.into_iter().nth(graph))
            .ok_or(FunctionEditError::InvalidOperation("not a curve graph"))
    }

    /// Number of segments in curve `graph` (1..=4), or `None` if not a curve.
    pub fn curve_segment_count(&self, graph: usize) -> Option<usize> {
        Some(self.graph(graph).ok()?.segment_count())
    }

    pub fn curve_max_segment_count(&self, _graph: usize) -> usize {
        4
    }

    pub fn curve_segment_type(&self, graph: usize, segment: usize) -> Option<CurveSegmentType> {
        Some(self.graph(graph).ok()?.segments.get(segment)?.seg_type)
    }

    pub fn curve_control_point_count(&self, graph: usize) -> Option<usize> {
        Some(self.graph(graph).ok()?.control_point_count())
    }

    pub fn curve_control_point(&self, graph: usize, point: usize) -> Option<(f32, f32)> {
        self.graph(graph).ok()?.get_control_point(point)
    }

    pub fn curve_is_graph_point(&self, graph: usize, point: usize) -> Option<bool> {
        self.graph(graph).ok()?.is_graph_point(point)
    }

    /// Move a control point. `graph` slot, global `point` index (see
    /// [`Self::curve_control_point_count`]); shared joins stay consistent.
    pub fn set_curve_control_point(
        &mut self,
        graph: usize,
        point: usize,
        value: (f32, f32),
    ) -> Result<(), FunctionEditError> {
        let mut graphs = self
            .curve_graphs()
            .ok_or(FunctionEditError::InvalidOperation("not a curve"))?;
        let g = graphs
            .get_mut(graph)
            .ok_or(FunctionEditError::InvalidOperation("graph slot out of range"))?;
        g.set_control_point(point, value.0, value.1);
        self.apply_curve_graphs(graphs)
    }

    /// Convert a segment to Linear/Spline/Spline2.
    pub fn set_curve_segment_type(
        &mut self,
        graph: usize,
        segment: usize,
        kind: CurveSegmentType,
    ) -> Result<(), FunctionEditError> {
        let mut graphs = self
            .curve_graphs()
            .ok_or(FunctionEditError::InvalidOperation("not a curve"))?;
        let g = graphs
            .get_mut(graph)
            .ok_or(FunctionEditError::InvalidOperation("graph slot out of range"))?;
        g.set_segment_type(segment, kind);
        self.apply_curve_graphs(graphs)
    }

    /// Insert a graph point at normalized `x` (splits a segment; max 4).
    pub fn insert_curve_point(&mut self, graph: usize, x: f32) -> Result<(), FunctionEditError> {
        let mut graphs = self
            .curve_graphs()
            .ok_or(FunctionEditError::InvalidOperation("not a curve"))?;
        let g = graphs
            .get_mut(graph)
            .ok_or(FunctionEditError::InvalidOperation("graph slot out of range"))?;
        if !g.insert_point(x) {
            return Err(FunctionEditError::InvalidOperation(
                "cannot insert point (max segments or out of range)",
            ));
        }
        self.apply_curve_graphs(graphs)
    }

    /// Delete the graph point `point` (merges adjacent segments; min 1).
    pub fn delete_curve_point(&mut self, graph: usize, point: usize) -> Result<(), FunctionEditError> {
        let mut graphs = self
            .curve_graphs()
            .ok_or(FunctionEditError::InvalidOperation("not a curve"))?;
        let g = graphs
            .get_mut(graph)
            .ok_or(FunctionEditError::InvalidOperation("graph slot out of range"))?;
        if !g.delete_point(point) {
            return Err(FunctionEditError::InvalidOperation(
                "cannot delete point (endpoint or last segment)",
            ));
        }
        self.apply_curve_graphs(graphs)
    }

    /// Corner/smooth mode of the join to the left of `segment`.
    pub fn curve_join_mode(&self, graph: usize, segment: usize) -> Option<CurvePointMode> {
        self.graph(graph).ok()?.segment_left_mode(segment)
    }

    /// Set the corner/smooth mode of the join between `segment-1` and
    /// `segment`.
    pub fn set_curve_join_mode(
        &mut self,
        graph: usize,
        segment: usize,
        mode: CurvePointMode,
    ) -> Result<(), FunctionEditError> {
        let mut graphs = self
            .curve_graphs()
            .ok_or(FunctionEditError::InvalidOperation("not a curve"))?;
        let g = graphs
            .get_mut(graph)
            .ok_or(FunctionEditError::InvalidOperation("graph slot out of range"))?;
        g.set_join_mode(segment, mode);
        self.apply_curve_graphs(graphs)
    }

    /// Rebuild the blob for `target` with `ranged` graph count, preserving the
    /// current header fields, then re-parse into `self.func`.
    fn rebuild(
        &mut self,
        target: FoundationMasterType,
        ranged: bool,
    ) -> Result<(), FunctionEditError> {
        let ftype = target.function_type();
        let graphs = if ranged { 2 } else { 1 };

        // Per-graph compact + editor bytes for the target type.
        let (compact, editor): (Vec<u8>, Vec<u8>) = default_graph_bytes(ftype);

        let mut compact_region = Vec::new();
        let mut editor_region = Vec::new();
        for _ in 0..graphs {
            compact_region.extend_from_slice(&compact);
            editor_region.extend_from_slice(&editor);
        }

        // Header: preserve everything, override type/flags/compact_size.
        let mut header = self.func.header_bytes(); // 32-byte snapshot
        header[0] = ftype as u8;
        // RANGE flag reflects graph count; OPTIMIZED cleared (we write a
        // trailer), matching c_function_definition_editor::postprocess.
        let mut flags = FunctionFlags(header[1]);
        if ranged {
            flags.0 |= FunctionFlags::RANGE;
        } else {
            flags.0 &= !FunctionFlags::RANGE;
        }
        flags.0 &= !FunctionFlags::OPTIMIZED;
        header[1] = flags.0;
        // Constant carries no color-graph gradient conversion here; keep byte 2.
        let compact_total = compact_region.len() as i32;
        header[28..32].copy_from_slice(&compact_total.to_le_bytes());

        let mut blob = Vec::with_capacity(32 + compact_region.len() + editor_region.len());
        blob.extend_from_slice(&header);
        blob.extend_from_slice(&compact_region);
        blob.extend_from_slice(&editor_region);

        self.func = TagFunction::parse(&blob).map_err(FunctionEditError::Serialize)?;
        Ok(())
    }
}

/// Default per-graph `(compact, editor)` byte pair for a target function
/// type. For periodic/exponent/transition the editor mirrors the compact;
/// for multi_part it is the 148-byte identity editor with its derived
/// 20-byte compact; constant/identity carry neither.
fn default_graph_bytes(ftype: FunctionType) -> (Vec<u8>, Vec<u8>) {
    match ftype {
        FunctionType::Constant | FunctionType::Identity => (Vec::new(), Vec::new()),
        FunctionType::Periodic => {
            // idx=0, freq=1, phase=0, amp_min=0, amp_max=1
            let mut c = vec![0u8; 4];
            c.extend_from_slice(&1.0f32.to_le_bytes());
            c.extend_from_slice(&0.0f32.to_le_bytes());
            c.extend_from_slice(&0.0f32.to_le_bytes());
            c.extend_from_slice(&1.0f32.to_le_bytes());
            (c.clone(), c)
        }
        FunctionType::Exponent => {
            // amp_min=0, amp_max=1, exponent=1
            let mut c = Vec::new();
            c.extend_from_slice(&0.0f32.to_le_bytes());
            c.extend_from_slice(&1.0f32.to_le_bytes());
            c.extend_from_slice(&1.0f32.to_le_bytes());
            (c.clone(), c)
        }
        FunctionType::Transition => {
            // idx=0, amp_min=0, amp_max=1
            let mut c = vec![0u8; 4];
            c.extend_from_slice(&0.0f32.to_le_bytes());
            c.extend_from_slice(&1.0f32.to_le_bytes());
            (c.clone(), c)
        }
        FunctionType::MultiSpline => build_identity_multipart_bytes(),
        // Legacy single types are not produced by the Foundation editor.
        _ => (Vec::new(), Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scalar constant with output range [0, 1] and no CLAMPED flag, so a
    /// converted identity curve evaluates to `f(x) = x`.
    fn constant_0_1() -> TagFunctionEditor {
        let mut blob = vec![0u8; 32];
        blob[0] = FunctionType::Constant as u8;
        blob[4..8].copy_from_slice(&0.0f32.to_le_bytes()); // clamp_min (value)
        blob[8..12].copy_from_slice(&1.0f32.to_le_bytes()); // clamp_max
        TagFunctionEditor::parse(&blob).unwrap()
    }

    #[test]
    fn convert_constant_to_curve_is_valid_identity_multispline() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Curve).unwrap();
        assert_eq!(e.function_type(), FunctionType::MultiSpline);
        assert_eq!(e.master_type(), FoundationMasterType::Curve);
        // Valid blob: 32 header + 20 compact + 148 editor.
        let bytes = e.to_bytes();
        assert_eq!(bytes.len(), 200);
        // Re-parses and evaluates as identity (not the old empty-compact 0.0).
        let f = e.function();
        for x in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            assert!(
                (f.evaluate(x, 0.0) - x).abs() < 1e-4,
                "identity curve f({x}) = {} != {x}",
                f.evaluate(x, 0.0)
            );
        }
        // Round-trips through parse.
        assert_eq!(TagFunctionEditor::parse(&bytes).unwrap().function_type(), FunctionType::MultiSpline);
    }

    #[test]
    fn convert_to_periodic_exponent_transition_are_valid() {
        for (mt, ft, compact) in [
            (FoundationMasterType::Periodic, FunctionType::Periodic, 20usize),
            (FoundationMasterType::Exponent, FunctionType::Exponent, 12),
            (FoundationMasterType::Transition, FunctionType::Transition, 12),
        ] {
            let mut e = constant_0_1();
            e.set_master_type(mt).unwrap();
            assert_eq!(e.function_type(), ft);
            let bytes = e.to_bytes();
            // header + compact + editor(mirror) for one graph.
            assert_eq!(bytes.len(), 32 + compact + compact, "{mt:?} blob size");
            assert_eq!(TagFunctionEditor::parse(&bytes).unwrap().function_type(), ft);
        }
    }

    #[test]
    fn set_ranged_adds_and_removes_second_graph() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        assert_eq!(e.graph_count(), 1);
        assert_eq!(e.to_bytes().len(), 72); // 32 + 20 + 20

        e.set_ranged(true).unwrap();
        assert!(e.is_ranged());
        assert_eq!(e.graph_count(), 2);
        // 32 + 2×20 compact + 2×20 editor = 112.
        assert_eq!(e.to_bytes().len(), 112);
        assert!(e.function().ranged_second().is_some());

        e.set_ranged(false).unwrap();
        assert!(!e.is_ranged());
        assert_eq!(e.graph_count(), 1);
        assert_eq!(e.to_bytes().len(), 72);
    }

    #[test]
    fn conversion_preserves_clamp_range_and_colors() {
        let mut blob = vec![0u8; 32];
        blob[0] = FunctionType::Constant as u8;
        blob[2] = ColorGraphType::TwoColor as u8;
        blob[4..8].copy_from_slice(&0.25f32.to_le_bytes()); // colors[0] / clamp_min
        blob[8..12].copy_from_slice(&0.75f32.to_le_bytes()); // colors[1] / clamp_max
        blob[16..20].copy_from_slice(&0xAABBCCDDu32.to_le_bytes()); // colors[3]
        let mut e = TagFunctionEditor::parse(&blob).unwrap();

        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        let h = e.function().header();
        assert_eq!(h.clamp_range_min, 0.25);
        assert_eq!(h.clamp_range_max, 0.75);
        assert_eq!(h.colors[3], 0xAABBCCDD);
        assert_eq!(h.color_graph_type, ColorGraphType::TwoColor);
    }

    fn identity_curve() -> TagFunctionEditor {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Curve).unwrap();
        e
    }

    #[test]
    fn dragging_endpoint_changes_evaluation_and_roundtrips() {
        let mut e = identity_curve();
        assert_eq!(e.curve_segment_count(0), Some(1));
        assert_eq!(e.curve_control_point_count(0), Some(2));
        // Move the end control point (index 1) down to y = 0.5.
        e.set_curve_control_point(0, 1, (1.0, 0.5)).unwrap();
        assert_eq!(e.curve_control_point(0, 1), Some((1.0, 0.5)));
        // f(1) now ≈ 0.5, f(0) ≈ 0 (clamp range [0,1], linear segment).
        let f = e.function();
        assert!((f.evaluate(1.0, 0.0) - 0.5).abs() < 1e-3, "f(1) = {}", f.evaluate(1.0, 0.0));
        assert!((f.evaluate(0.0, 0.0) - 0.0).abs() < 1e-3);
        assert!((f.evaluate(0.5, 0.0) - 0.25).abs() < 1e-3, "midpoint {}", f.evaluate(0.5, 0.0));
        // Round-trips through a full parse.
        let bytes = e.to_bytes();
        let re = TagFunctionEditor::parse(&bytes).unwrap();
        assert_eq!(re.curve_control_point(0, 1), Some((1.0, 0.5)));
    }

    #[test]
    fn changing_segment_to_spline_grows_control_points() {
        let mut e = identity_curve();
        e.set_curve_segment_type(0, 0, CurveSegmentType::Spline).unwrap();
        assert_eq!(e.curve_segment_type(0, 0), Some(CurveSegmentType::Spline));
        // Spline segment exposes 4 control points; endpoints are graph points.
        assert_eq!(e.curve_control_point_count(0), Some(4));
        assert_eq!(e.curve_is_graph_point(0, 0), Some(true));
        assert_eq!(e.curve_is_graph_point(0, 1), Some(false)); // tangent handle
        assert_eq!(e.curve_is_graph_point(0, 3), Some(true));
        // Still evaluates as (near) identity — endpoints (0,0)/(1,1) preserved.
        let f = e.function();
        assert!((f.evaluate(0.0, 0.0) - 0.0).abs() < 1e-3);
        assert!((f.evaluate(1.0, 0.0) - 1.0).abs() < 1e-3);
        assert_eq!(TagFunctionEditor::parse(&e.to_bytes()).unwrap().function_type(), FunctionType::MultiSpline);
    }

    #[test]
    fn insert_and_delete_curve_points() {
        let mut e = identity_curve();
        assert_eq!(e.curve_segment_count(0), Some(1));
        // Insert two graph points → 3 segments, 4 control points.
        e.insert_curve_point(0, 0.5).unwrap();
        assert_eq!(e.curve_segment_count(0), Some(2));
        e.insert_curve_point(0, 0.25).unwrap();
        assert_eq!(e.curve_segment_count(0), Some(3));
        assert_eq!(e.curve_control_point_count(0), Some(4));
        // Still monotonic identity along x.
        let f = e.function();
        assert!((f.evaluate(0.5, 0.0) - 0.5).abs() < 1e-2, "f(0.5)={}", f.evaluate(0.5, 0.0));
        // Delete the middle graph point (index 2 → join) → back to 2 segments.
        e.delete_curve_point(0, 2).unwrap();
        assert_eq!(e.curve_segment_count(0), Some(2));
        // Round-trips.
        assert_eq!(TagFunctionEditor::parse(&e.to_bytes()).unwrap().function_type(), FunctionType::MultiSpline);
    }

    #[test]
    fn insert_curve_point_capped_at_four_segments() {
        let mut e = identity_curve();
        e.insert_curve_point(0, 0.2).unwrap();
        e.insert_curve_point(0, 0.4).unwrap();
        e.insert_curve_point(0, 0.6).unwrap();
        assert_eq!(e.curve_segment_count(0), Some(4));
        // Fifth insert is rejected (max 4 segments).
        assert!(e.insert_curve_point(0, 0.8).is_err());
    }

    #[test]
    fn periodic_params_get_set_both_graphs_roundtrip() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        e.set_ranged(true).unwrap();
        let g0 = PeriodicParams { function_index: 8, frequency: 2.5, phase: 0.25, amplitude_min: -1.0, amplitude_max: 3.0 };
        let g1 = PeriodicParams { function_index: 2, frequency: 0.5, phase: 0.0, amplitude_min: 0.0, amplitude_max: 1.0 };
        e.set_periodic_params(0, g0).unwrap();
        e.set_periodic_params(1, g1).unwrap();
        assert_eq!(e.periodic_params(0), Some(g0));
        assert_eq!(e.periodic_params(1), Some(g1));
        // Survives a full parse round-trip, both graphs independent.
        let re = TagFunctionEditor::parse(&e.to_bytes()).unwrap();
        assert_eq!(re.periodic_params(0), Some(g0));
        assert_eq!(re.periodic_params(1), Some(g1));
        assert!(re.is_ranged());
    }

    #[test]
    fn exponent_and_transition_params_roundtrip() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Exponent).unwrap();
        let ep = ExponentParams { exponent: 2.0, amplitude_min: 0.1, amplitude_max: 0.9 };
        e.set_exponent_params(0, ep).unwrap();
        assert_eq!(e.exponent_params(0), Some(ep));
        assert_eq!(TagFunctionEditor::parse(&e.to_bytes()).unwrap().exponent_params(0), Some(ep));

        let mut t = constant_0_1();
        t.set_master_type(FoundationMasterType::Transition).unwrap();
        let tp = TransitionParams { function_index: 3, amplitude_min: 0.2, amplitude_max: 0.8 };
        t.set_transition_params(0, tp).unwrap();
        assert_eq!(t.transition_params(0), Some(tp));
        assert_eq!(TagFunctionEditor::parse(&t.to_bytes()).unwrap().transition_params(0), Some(tp));
    }

    #[test]
    fn option_tables_match_doc() {
        assert_eq!(PERIODIC_FUNCTIONS.len(), 12);
        assert_eq!(PERIODIC_FUNCTIONS[0], "one");
        assert_eq!(PERIODIC_FUNCTIONS[2], "cosine");
        assert_eq!(PERIODIC_FUNCTIONS[11], "spark");
        assert_eq!(TRANSITION_FUNCTIONS, ["linear", "early", "very early", "late"]);
        let e = constant_0_1();
        assert_eq!(e.periodic_function_count(), 12);
        assert_eq!(e.periodic_function_text(8), Some("noise"));
        assert_eq!(e.transition_function_text(3), Some("late"));
        assert_eq!(e.periodic_function_text(99), None);
    }

    #[test]
    fn color_slots_preserved_on_edit() {
        // A 2-color periodic; set color 1 (physical slot 3), color 0 (slot 0)
        // must be untouched, and slots 1/2 stay zero.
        let mut e = constant_0_1();
        e.set_color_graph_type(ColorGraphType::TwoColor);
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        assert_eq!(e.color_count(), 2);
        // Setting logical color 1 (physical slot 3) must not disturb logical
        // color 0 (physical slot 0). Slots 1/2 are unused for a 2-color graph
        // (the header union there still aliases clamp bits — not a color).
        e.set_color(0, 0x00112233).unwrap();
        e.set_color(1, 0x00AABBCC).unwrap();
        assert_eq!(e.get_color(0), Some(0x00112233));
        assert_eq!(e.get_color(1), Some(0x00AABBCC));
        let h = e.function().header();
        assert_eq!(h.colors[0], 0x00112233); // physical slot 0
        assert_eq!(h.colors[3], 0x00AABBCC); // physical slot 3
        // Round-trips, both logical colors preserved.
        let re = TagFunctionEditor::parse(&e.to_bytes()).unwrap();
        assert_eq!(re.get_color(0), Some(0x00112233));
        assert_eq!(re.get_color(1), Some(0x00AABBCC));
    }

    #[test]
    fn typed_params_reject_wrong_type() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        assert!(e.exponent_params(0).is_none());
        assert!(e.set_exponent_params(0, ExponentParams { exponent: 1.0, amplitude_min: 0.0, amplitude_max: 1.0 }).is_err());
        // slot 1 on an unranged function is out of range.
        assert!(e.set_periodic_params(1, PeriodicParams { function_index: 0, frequency: 1.0, phase: 0.0, amplitude_min: 0.0, amplitude_max: 1.0 }).is_err());
    }

    #[test]
    fn curve_ops_reject_non_curve() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        assert!(e.curve_segment_count(0).is_none());
        assert!(e.set_curve_control_point(0, 0, (0.0, 0.0)).is_err());
    }

    #[test]
    fn corner_smooth_persists_on_spline_joins() {
        // Two spline segments so the join isn't forced to a corner by
        // postprocess (Linear neighbours force corners).
        let mut e = identity_curve();
        e.set_curve_segment_type(0, 0, CurveSegmentType::Spline).unwrap();
        e.insert_curve_point(0, 0.5).unwrap(); // splits into two linears
        e.set_curve_segment_type(0, 0, CurveSegmentType::Spline).unwrap();
        e.set_curve_segment_type(0, 1, CurveSegmentType::Spline).unwrap();
        assert_eq!(e.curve_segment_count(0), Some(2));
        // Corner/Smooth is a real toggle at a spline↔spline join, and both
        // states round-trip (postprocess doesn't override the explicit choice).
        e.set_curve_join_mode(0, 1, CurvePointMode::Smooth).unwrap();
        assert_eq!(e.curve_join_mode(0, 1), Some(CurvePointMode::Smooth));
        assert_eq!(
            TagFunctionEditor::parse(&e.to_bytes()).unwrap().curve_join_mode(0, 1),
            Some(CurvePointMode::Smooth)
        );
        e.set_curve_join_mode(0, 1, CurvePointMode::Corner).unwrap();
        assert_eq!(e.curve_join_mode(0, 1), Some(CurvePointMode::Corner));
        let re = TagFunctionEditor::parse(&e.to_bytes()).unwrap();
        assert_eq!(re.curve_join_mode(0, 1), Some(CurvePointMode::Corner));
    }

    #[test]
    fn all_segment_types_produce_valid_evaluable_compacts() {
        for kind in [CurveSegmentType::Linear, CurveSegmentType::Spline, CurveSegmentType::Spline2] {
            let mut e = identity_curve();
            e.set_curve_segment_type(0, 0, kind).unwrap();
            let f = e.function();
            for x in [0.0f32, 0.3, 0.6, 1.0] {
                let v = f.evaluate(x, 0.0);
                assert!(v.is_finite(), "{kind:?} eval at {x} = {v}");
            }
            // Endpoints stay identity-ish (0→~0, 1→~1).
            assert!((f.evaluate(0.0, 0.0)).abs() < 1e-2, "{kind:?} f(0)");
            assert!((f.evaluate(1.0, 0.0) - 1.0).abs() < 1e-2, "{kind:?} f(1)");
            assert_eq!(TagFunctionEditor::parse(&e.to_bytes()).unwrap().function_type(), FunctionType::MultiSpline);
        }
    }

    #[test]
    fn three_and_four_color_slot_preservation() {
        for (cgt, count, slots) in [
            (ColorGraphType::ThreeColor, 3usize, vec![0usize, 1, 3]),
            (ColorGraphType::FourColor, 4, vec![0, 1, 2, 3]),
        ] {
            let mut e = constant_0_1();
            e.set_color_graph_type(cgt);
            e.set_master_type(FoundationMasterType::Periodic).unwrap();
            assert_eq!(e.color_count(), count);
            let colors: Vec<u32> = (0..count).map(|i| 0x00100000 * (i as u32 + 1)).collect();
            for (i, &c) in colors.iter().enumerate() {
                e.set_color(i, c).unwrap();
            }
            let re = TagFunctionEditor::parse(&e.to_bytes()).unwrap();
            for (i, &c) in colors.iter().enumerate() {
                assert_eq!(re.get_color(i), Some(c), "{cgt:?} logical color {i}");
            }
            // Physical slots line up with the mapping.
            let h = re.function().header();
            for (i, &slot) in slots.iter().enumerate() {
                assert_eq!(h.colors[slot], colors[i]);
            }
        }
    }

    #[test]
    fn conversion_preserves_exclusion_and_flag_bytes() {
        let mut blob = vec![0u8; 32];
        blob[0] = FunctionType::Constant as u8;
        blob[1] = FunctionFlags::CLAMPED | FunctionFlags::CYCLIC; // unrelated flags
        blob[8..12].copy_from_slice(&1.0f32.to_le_bytes());
        blob[20..24].copy_from_slice(&0.2f32.to_le_bytes()); // exclusion_min
        blob[24..28].copy_from_slice(&0.8f32.to_le_bytes()); // exclusion_max
        let mut e = TagFunctionEditor::parse(&blob).unwrap();
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        let h = e.function().header();
        assert_eq!(h.exclusion_min, 0.2);
        assert_eq!(h.exclusion_max, 0.8);
        assert!(h.flags.is_clamped() && h.flags.is_cyclic());
        // Valid compact size in the serialized header.
        let bytes = e.to_bytes();
        let cs = i32::from_le_bytes(bytes[28..32].try_into().unwrap());
        assert_eq!(cs, 20); // one periodic compact
    }

    #[test]
    fn ranged_curve_conversion_makes_two_valid_graphs() {
        let mut e = constant_0_1();
        e.set_master_type(FoundationMasterType::Periodic).unwrap();
        e.set_ranged(true).unwrap();
        // Convert a ranged periodic to a curve — must produce 2 identity graphs.
        e.set_master_type(FoundationMasterType::Curve).unwrap();
        assert_eq!(e.function_type(), FunctionType::MultiSpline);
        assert_eq!(e.graph_count(), 2);
        // 32 + 2×20 compact + 2×148 editor.
        assert_eq!(e.to_bytes().len(), 32 + 40 + 296);
        assert!(e.function().ranged_second().is_some());
    }
}
