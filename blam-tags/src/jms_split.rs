//! Split oversized JMS sections across extra regions.
//!
//! # Why this exists
//!
//! `tool.exe` refuses a section over 32,767 vertices and **never splits
//! anything to make it fit** — the import just fails. Shipped content
//! works around that in the source: `lich.render_model` is 165,395
//! triangles carried by 13 regions, three of which are `exterior01`,
//! `exterior02`, `exterior03` — one mesh cut into three so each piece
//! fits. This module does that cut automatically.
//!
//! It needs no change to `tool.exe`, and it cannot produce an invalid
//! tag, because stock `tool.exe` still validates everything afterwards.
//!
//! # What decides a section
//!
//! A JMS has no mesh concept: one flat vertex list, one flat triangle
//! list, and a material index per triangle. Sections come entirely from
//! the material *definition line*, `(slot) [LOD] permutation region`.
//! Every distinct `(lod, permutation, region)` triple becomes one
//! section. Verified against the shipped corpus: grouping this way
//! reproduces the section count of the built tag **exactly** on every
//! matched model — 14/14 on `lich`, 43/43 on `hoverhog`, 31/31 on
//! `heretic_banshee02`.
//!
//! So a section is split by rewriting material labels, and nothing else.
//!
//! # The budget, and why it is a guess with evidence
//!
//! The limit is on *post-weld* vertices, and JMS stores an unshared
//! triangle soup — `tool.exe` welds it, and how much it welds is not
//! cheaply predictable. Exact-match deduplication was tried as a
//! predictor and rejected: measured against 86 shipped models it
//! *under*-estimates on 22 of them (as low as 0.5×), because the
//! importer also *splits* vertices, and it over-estimates by up to 3.35×
//! elsewhere, because the importer's weld uses epsilons where exact
//! matching does not.
//!
//! What is stable is the ratio of built vertices to *source triangles*,
//! and — importantly — **it falls as sections get bigger.** Measured over
//! 102 matched sections from the shipped corpus:
//!
//! | source triangles | sections | min | median | max |
//! |---|---|---|---|---|
//! | under 100 | 11 | 0.90 | 2.00 | **2.21** |
//! | 100 – 1,000 | 39 | 0.57 | 1.09 | 2.00 |
//! | 1,000 – 5,000 | 42 | 0.55 | 1.03 | 1.55 |
//! | 5,000 – 20,000 | 9 | 0.52 | 0.87 | 1.44 |
//! | over 20,000 | 1 | — | 0.73 | 0.73 |
//!
//! The ratios above 1.5 are **exclusively tiny sections**, and they are
//! tiny for a reason: they are double-sided transparent geometry, where
//! the importer emits each triangle once per winding, so a 12-triangle
//! section lands at exactly 24 vertices. Sections big enough for this
//! module to touch behave quite differently — the largest observed ratio
//! at 1,000 triangles or more is **1.553**, and the biggest section in
//! the corpus (25,670 triangles) sits at 0.725 because welding wins at
//! scale.
//!
//! [`SplitBudget::vertices_per_triangle`] therefore defaults to **1.6**,
//! just above the worst case in the size range that matters, rather than
//! above the global worst case. Defaulting to 2.3 would have been
//! "safer" in the abstract and clearly wrong in practice: it would split
//! sections a third the size of ones that import fine today, and regions
//! are the scarce resource here — there are only 16.
//!
//! It is still an estimate, and deliberately conservative: an
//! over-small section imports fine, an over-large one fails. If `tool`
//! still rejects a section, raise the ratio and re-run. A mesh known to
//! be single-sided can go the other way, down to about 1.1.
//!
//! ## What the default does to shipped content
//!
//! At 1.6 — a budget of 20,479 source triangles — **251 of the 254
//! shipped JMS files are left untouched.** The three it would split are
//! `lich22.JMS` (14 → 15 sections, 13 → 14 regions), `skiff16.JMS`
//! (15 → 17, 6 → 7) and `test\Untitled.JMS` (1 → 2).
//!
//! Those three import fine today: the corpus's largest section is 27,613
//! triangles at a real ratio near 1.09, so it fits without help. Cutting
//! it is unnecessary but harmless — an extra region out of 16, and no
//! visual change, since all regions draw together. Tightening the ratio
//! to about 1.2 would leave them alone, and is a reasonable choice for
//! content you know is single-sided. It is not the default because the
//! evidence thins out exactly where it would matter: only ten measured
//! sections exceed 5,000 triangles and only one exceeds 20,000, so the
//! declining-ratio trend is real but lightly evidenced at the top end —
//! and the cost of being wrong in that direction is a failed import
//! after a long build, not a spare region.
//!
//! # The ceiling this does not lift
//!
//! **16 regions per model, and that one is real** — three enforcement
//! sites including engine code. So this buys about 16 × 32,767 ≈ 524,000
//! welded vertices per model, roughly 500k source triangles, and not one
//! triangle more. Past that the answer is a real importer, not a
//! pre-processor. [`split_oversized_sections`] fails loudly rather than
//! emitting a model that cannot import.
//!
//! # What a split changes downstream
//!
//! New regions are new names. All regions of a render_model draw
//! together, so the result is visually identical — but a `model` (hlmt)
//! tag that names regions for damage, permutation selection or
//! attachment will not know about `exterior02`. Check the report and fix
//! the hlmt if the model uses those features.

use std::collections::BTreeMap;

use crate::jms::{JmsFile, JmsMaterial};

/// Sections cap at this many vertices in `tool.exe`'s importer. The tag
/// format itself allows 65,535, but some vertex-index fields are written
/// through a signed-word helper, which is the likeliest reason the
/// importer stops at half that. Treat this as the safe number.
pub const MAX_VERTICES_PER_SECTION: usize = 32_767;

/// Indices a built section may hold. Unlike the vertex cap this one is
/// the format's: the index run is addressed with 16-bit offsets.
///
/// It binds before the vertex cap on dense meshes, which is how a section
/// can be comfortably inside 32,767 vertices and still be refused.
pub const MAX_INDICES_PER_SECTION: usize = 65_535;

/// Regions per model. Unlike the vertex cap this is a genuine engine
/// constant, enforced in three places including the runtime.
pub const MAX_REGIONS: usize = 16;


/// The symbol table a material name's flags are read against.
///
/// `.rdata 0x140FBA568`, 25 entries. Index 23 is a NUL, which
/// `sub_14010FDA0` skips with an `if (v8)` before comparing, so it never
/// matches and index 24 is still reachable.
const MATERIAL_SYMBOLS: &[u8; 25] = b"%#?!@*$^-&=.;)><|~({}['\0]";

/// Bit 13 — `)` — is what becomes `connected_material` flag bit 3, the
/// one the point welder calls *precise*.
const PRECISE_SYMBOL_BIT: u32 = 1 << 13;

/// The flag bits a material name's symbol runs set.
///
/// `sub_14010FDA0`: walk in from each end, skipping whitespace, setting
/// bit *i* for the *i*th table entry, and stop at the first character
/// that is not one. So the symbols must be a leading or trailing run —
/// `flood_fronds)!%` sets three bits, and a `)` in the middle of a word
/// sets none.
pub fn material_symbol_flags(name: &str) -> u32 {
    let index = |c: u8| -> Option<u32> {
        MATERIAL_SYMBOLS.iter().position(|&s| s != 0 && s == c).map(|i| i as u32)
    };
    let is_space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r');

    let bytes = name.as_bytes();
    let mut flags = 0u32;
    for &c in bytes {
        if is_space(c) {
            continue;
        }
        match index(c) {
            Some(i) => flags |= 1 << i,
            None => break,
        }
    }
    for &c in bytes.iter().rev() {
        if is_space(c) {
            continue;
        }
        match index(c) {
            Some(i) => flags |= 1 << i,
            None => break,
        }
    }
    flags
}

/// A material name with its symbol runs taken off.
///
/// The symbols are flags, not part of the name: `flood_fronds)!%` and
/// `flood_fronds` are one shader with different flags set, and Tool emits
/// one material for them. Stripping the same leading and trailing runs
/// [`material_symbol_flags`] reads is what makes two spellings compare
/// equal.
pub fn material_base_name(name: &str) -> &str {
    let symbol = |c: u8| MATERIAL_SYMBOLS.iter().any(|&s| s != 0 && s == c);
    let strip = |c: u8| symbol(c) || c.is_ascii_whitespace();
    let b = name.as_bytes();
    let mut lo = 0usize;
    while lo < b.len() && strip(b[lo]) {
        lo += 1;
    }
    let mut hi = b.len();
    while hi > lo && strip(b[hi - 1]) {
        hi -= 1;
    }
    &name[lo..hi]
}

/// Does this material name carry the *precise* flag?
///
/// The point welder welds a precise point only at the tight tolerance
/// and leaves it out of the coarse stage, so this decides which vertices
/// are allowed to move further.
pub fn material_is_precise(name: &str) -> bool {
    material_symbol_flags(name) & PRECISE_SYMBOL_BIT != 0
}

/// How aggressively to split.
#[derive(Debug, Clone)]
pub struct SplitBudget {
    /// Vertex ceiling a built section must stay under.
    pub max_vertices_per_section: usize,
    /// Assumed built-vertices-per-source-triangle. See the module docs
    /// for the measured distribution by section size; 1.6 sits just above
    /// the worst case (1.553) among sections large enough to ever need
    /// splitting.
    pub vertices_per_triangle: f32,
    /// Index ceiling a built section must stay under.
    pub max_indices_per_section: usize,
    /// Assumed indices-per-source-triangle, after stripping.
    ///
    /// Fitted over 365 meshes from the kit's loose render JMS. Only
    /// sections large enough to need splitting set the figure, and among
    /// those the worst case is 2.736:
    ///
    /// | source triangles | meshes | mean | max |
    /// |---|---|---|---|
    /// | any | 365 | 2.400 | 4.569 |
    /// | >= 1,000 | 179 | 2.313 | 3.557 |
    /// | >= 5,000 | 54 | 2.287 | 3.163 |
    /// | >= 10,000 | 20 | 2.256 | 2.736 |
    ///
    /// 3.0 sits above that with room, and it is the figure an unstripped
    /// triangle list would cost — so a section budgeted at 3.0 fits even
    /// if stripping achieves nothing at all.
    pub indices_per_triangle: f32,
    /// Refuse to produce more than this many distinct regions.
    pub max_regions: usize,
}

impl Default for SplitBudget {
    fn default() -> Self {
        Self {
            max_vertices_per_section: MAX_VERTICES_PER_SECTION,
            vertices_per_triangle: 1.6,
            max_indices_per_section: MAX_INDICES_PER_SECTION,
            indices_per_triangle: 3.0,
            max_regions: MAX_REGIONS,
        }
    }
}

impl SplitBudget {
    /// Largest source-triangle count that should still fit one section.
    ///
    /// Whichever of the two ceilings binds first. A section can sit well
    /// inside the vertex cap and still need more indices than the format
    /// holds, so budgeting only vertices leaves the importer refusing
    /// sections that `--split` was asked to fix.
    pub fn triangles_per_section(&self) -> usize {
        let by = |limit: usize, ratio: f32, fallback: f32| -> usize {
            let r = if ratio.is_finite() && ratio > 0.0 { ratio } else { fallback };
            ((limit as f32) / r).floor().max(1.0) as usize
        };
        let by_vertices = by(self.max_vertices_per_section, self.vertices_per_triangle, 1.6);
        let by_indices = by(self.max_indices_per_section, self.indices_per_triangle, 3.0);
        by_vertices.min(by_indices)
    }
}

/// Why a split could not be performed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitError {
    /// Fitting the geometry needs more regions than the format allows.
    /// The model is past what a source-side split can rescue.
    TooManyRegions { needed: usize, limit: usize },
    /// A generated region name collided with one already in the file.
    /// Refused rather than silently merging two unrelated sections.
    NameCollision { name: String },
    /// A triangle referenced a material index outside `materials`.
    BadMaterialIndex { triangle: usize, material: i32 },
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyRegions { needed, limit } => write!(
                f,
                "splitting needs {needed} regions but the format allows {limit}; this model is \
                 past what a source-side split can fix and needs a real importer"
            ),
            Self::NameCollision { name } => write!(
                f,
                "generated region name {name:?} already exists in this file; refusing to merge \
                 two unrelated sections"
            ),
            Self::BadMaterialIndex { triangle, material } => {
                write!(f, "triangle {triangle} references material {material}, which does not exist")
            }
        }
    }
}

impl std::error::Error for SplitError {}

/// A parsed material definition line, `(slot) [LOD] permutation region`.
///
/// Mirrors `tool.exe`'s own splitter: strip a leading parenthesised
/// group, then an optional `L<digit>` LOD token, then take the next two
/// whitespace-separated tokens as permutation and region in that order.
/// Either missing token becomes `default`.
///
/// Note the delimiter here is *whitespace*, unlike the file-level
/// tokeniser which splits only on tabs — this whole label arrives as one
/// file token precisely because spaces are content there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialLabel {
    /// The artist's Blender material slot. Round-trip metadata only —
    /// the importer strips it and acts on nothing inside it.
    pub slot: Option<String>,
    /// `l1`..`l6` style level-of-detail token, when present.
    pub lod: Option<String>,
    pub permutation: String,
    pub region: String,
}

impl MaterialLabel {
    /// Parse a definition line. Never fails: absent fields default, the
    /// same way the importer's does.
    pub fn parse(text: &str) -> Self {
        let mut rest = text.trim();

        let mut slot = None;
        if let Some((inside, after)) = rest.strip_prefix('(').and_then(|s| s.split_once(')')) {
            slot = Some(inside.to_owned());
            rest = after.trim_start();
        }

        let mut parts = rest.split_whitespace();
        let mut first = parts.next();

        let lod = match first {
            Some(t) if is_lod_token(t) => {
                let lod = t.to_owned();
                first = parts.next();
                Some(lod)
            }
            _ => None,
        };

        let permutation = first.filter(|t| !t.is_empty()).unwrap_or("default").to_owned();
        let region = parts.next().filter(|t| !t.is_empty()).unwrap_or("default").to_owned();

        Self { slot, lod, permutation, region }
    }

    /// Re-emit in the importer's format.
    pub fn to_label(&self) -> String {
        let mut out = String::new();
        if let Some(slot) = &self.slot {
            out.push('(');
            out.push_str(slot);
            out.push_str(") ");
        }
        if let Some(lod) = &self.lod {
            out.push_str(lod);
            out.push(' ');
        }
        out.push_str(&self.permutation);
        out.push(' ');
        out.push_str(&self.region);
        out
    }

    /// The `(lod, permutation, region)` triple that decides which section
    /// a triangle lands in. Case-folded, because the importer resolves
    /// these by name and the corpus is inconsistent about case
    /// (`Default` and `default` both occur).
    pub fn section_key(&self) -> (String, String, String) {
        (
            self.lod.as_deref().unwrap_or("").to_ascii_lowercase(),
            self.permutation.to_ascii_lowercase(),
            self.region.to_ascii_lowercase(),
        )
    }
}

/// `l` or `L` followed by a single digit — the LOD token the importer
/// skips before reading permutation and region.
fn is_lod_token(t: &str) -> bool {
    let b = t.as_bytes();
    b.len() == 2 && (b[0] == b'l' || b[0] == b'L') && b[1].is_ascii_digit()
}

/// One section that was cut up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionSplit {
    /// Section key before the split.
    pub original_region: String,
    pub permutation: String,
    pub lod: Option<String>,
    /// Source triangles the section held.
    pub triangles: usize,
    /// Region names it became, the first being the original.
    pub regions: Vec<String>,
}

/// What [`split_oversized_sections`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SplitReport {
    pub sections_before: usize,
    pub sections_after: usize,
    pub regions_before: usize,
    pub regions_after: usize,
    /// Only the sections that were actually cut.
    pub splits: Vec<SectionSplit>,
    /// Largest source-triangle count in any section afterwards.
    pub largest_section_triangles: usize,
}

impl SplitReport {
    /// Did anything change?
    pub fn changed(&self) -> bool {
        !self.splits.is_empty()
    }
}

/// Cut every section over budget into as many regions as it needs.
///
/// Mutates `jms` in place: adds material entries and repoints triangle
/// material indices. Vertices and triangles themselves are untouched —
/// only which section each triangle belongs to changes.
///
/// Returns without modifying anything if nothing is over budget, or if
/// the split cannot be done within [`SplitBudget::max_regions`].
pub fn split_oversized_sections(
    jms: &mut JmsFile,
    budget: &SplitBudget,
) -> Result<SplitReport, SplitError> {
    let labels: Vec<MaterialLabel> =
        jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();

    // Group triangle indices by section. BTreeMap so the walk is
    // deterministic: the same input must always produce the same regions.
    let mut sections: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (i, tri) in jms.triangles.iter().enumerate() {
        let label = labels.get(usize::try_from(tri.material).map_err(|_| {
            SplitError::BadMaterialIndex { triangle: i, material: tri.material }
        })?);
        let Some(label) = label else {
            return Err(SplitError::BadMaterialIndex { triangle: i, material: tri.material });
        };
        sections.entry(label.section_key()).or_default().push(i);
    }

    let limit = budget.triangles_per_section();
    let existing_regions: std::collections::BTreeSet<String> =
        labels.iter().map(|l| l.region.to_ascii_lowercase()).collect();

    let mut report = SplitReport {
        sections_before: sections.len(),
        regions_before: existing_regions.len(),
        ..Default::default()
    };

    // How many regions will we end up with? Refuse before touching
    // anything if the answer is too many — a half-applied split is worse
    // than none.
    let mut projected: std::collections::BTreeSet<String> = existing_regions.clone();
    let mut plan: Vec<((String, String, String), usize)> = Vec::new();
    for (key, tris) in &sections {
        let chunks = tris.len().div_ceil(limit.max(1));
        if chunks > 1 {
            let region = &key.2;
            for n in 2..=chunks {
                projected.insert(format!("{region}{n:02}"));
            }
            plan.push((key.clone(), chunks));
        }
    }
    if plan.is_empty() {
        report.sections_after = sections.len();
        report.regions_after = existing_regions.len();
        report.largest_section_triangles =
            sections.values().map(|v| v.len()).max().unwrap_or(0);
        return Ok(report);
    }
    if projected.len() > budget.max_regions {
        return Err(SplitError::TooManyRegions {
            needed: projected.len(),
            limit: budget.max_regions,
        });
    }
    for name in projected.difference(&existing_regions) {
        if existing_regions.contains(name) {
            return Err(SplitError::NameCollision { name: name.clone() });
        }
    }

    // Apply. For each over-budget section, order its triangles for
    // spatial locality and deal them into chunks, then give each chunk
    // past the first its own copy of every material the section used,
    // relabelled to the new region.
    for (key, chunks) in plan {
        let tris = &sections[&key];
        let ordered = order_for_locality(jms, tris);
        let per_chunk = ordered.len().div_ceil(chunks);

        let mut regions = vec![key.2.clone()];
        for chunk in 1..chunks {
            let base_region = &key.2;
            let new_region = format!("{base_region}{:02}", chunk + 1);
            regions.push(new_region.clone());

            // One new material per distinct material used in this chunk,
            // so parts and shaders survive the move.
            let mut remap: BTreeMap<i32, i32> = BTreeMap::new();
            let start = chunk * per_chunk;
            let end = ((chunk + 1) * per_chunk).min(ordered.len());
            for &t in &ordered[start..end] {
                let old = jms.triangles[t].material;
                let new = match remap.get(&old) {
                    Some(v) => *v,
                    None => {
                        let src = &jms.materials[old as usize];
                        let mut label = MaterialLabel::parse(&src.material_name);
                        label.region = new_region.clone();
                        let created = JmsMaterial {
                            name: src.name.clone(),
                            material_name: label.to_label(),
                        };
                        jms.materials.push(created);
                        let idx = (jms.materials.len() - 1) as i32;
                        remap.insert(old, idx);
                        idx
                    }
                };
                jms.triangles[t].material = new;
            }
        }

        report.splits.push(SectionSplit {
            original_region: key.2.clone(),
            permutation: key.1.clone(),
            lod: if key.0.is_empty() { None } else { Some(key.0.clone()) },
            triangles: tris.len(),
            regions,
        });
    }

    // Recount from the mutated file rather than predicting, so the report
    // describes what is actually there.
    let labels: Vec<MaterialLabel> =
        jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
    let mut after: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    let mut regions_after: std::collections::BTreeSet<String> = Default::default();
    for tri in &jms.triangles {
        if let Some(label) = labels.get(tri.material as usize) {
            *after.entry(label.section_key()).or_default() += 1;
            regions_after.insert(label.region.to_ascii_lowercase());
        }
    }
    report.sections_after = after.len();
    report.regions_after = regions_after.len();
    report.largest_section_triangles = after.values().copied().max().unwrap_or(0);
    Ok(report)
}

/// Order a section's triangles so that consecutive ones are near each
/// other in space.
///
/// Any partition is *correct* — the split only has to fit the budget. But
/// an arbitrary one scatters each region across the whole model, which
/// makes the engine's per-region culling useless and inflates every
/// region's bounding volume to the size of the original. Sorting by the
/// Morton code of the triangle centroid keeps each chunk compact for the
/// cost of one sort.
fn order_for_locality(jms: &JmsFile, tris: &[usize]) -> Vec<usize> {
    // Bounds over just this section's triangles.
    let (mut lo, mut hi) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
    let centroid = |t: usize| -> [f32; 3] {
        let tri = &jms.triangles[t];
        let mut c = [0.0f32; 3];
        for v in tri.v {
            if let Some(vert) = jms.vertices.get(v as usize) {
                c[0] += vert.position.x;
                c[1] += vert.position.y;
                c[2] += vert.position.z;
            }
        }
        [c[0] / 3.0, c[1] / 3.0, c[2] / 3.0]
    };
    for &t in tris {
        let c = centroid(t);
        for a in 0..3 {
            if c[a].is_finite() {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
        }
    }

    let mut keyed: Vec<(u64, usize)> = tris
        .iter()
        .map(|&t| {
            let c = centroid(t);
            let mut q = [0u32; 3];
            for a in 0..3 {
                let span = hi[a] - lo[a];
                let n = if span > 0.0 && c[a].is_finite() {
                    ((c[a] - lo[a]) / span * 1023.0).clamp(0.0, 1023.0) as u32
                } else {
                    0
                };
                q[a] = n;
            }
            (morton3(q[0], q[1], q[2]), t)
        })
        .collect();
    // Sort by (code, index) so equal codes keep a stable, reproducible
    // order — the same input must always yield the same split.
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, t)| t).collect()
}

/// Interleave three 10-bit values into a 30-bit Morton code.
fn morton3(x: u32, y: u32, z: u32) -> u64 {
    fn spread(v: u32) -> u64 {
        let mut v = u64::from(v & 0x3FF);
        v = (v | (v << 16)) & 0x0000_0000_FF00_00FF;
        v = (v | (v << 8)) & 0x0000_0000_0F00_F00F;
        v = (v | (v << 4)) & 0x0000_0000_C30C_30C3;
        v = (v | (v << 2)) & 0x0000_0000_4924_9249;
        v
    }
    spread(x) | (spread(y) << 1) | (spread(z) << 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jms::{JmsTriangle, JmsVertex};
    use crate::math::{RealPoint2d, RealPoint3d, RealVector3d};

    fn label(s: &str) -> MaterialLabel {
        MaterialLabel::parse(s)
    }

    #[test]
    fn material_labels_parse_the_shapes_the_corpus_actually_uses() {
        // 1406 of 1573 shipped materials are the 3-token form...
        let a = label("(0) Default Default");
        assert_eq!(a.slot.as_deref(), Some("0"));
        assert_eq!(a.lod, None);
        assert_eq!((a.permutation.as_str(), a.region.as_str()), ("Default", "Default"));

        // ...and 167 are the 4-token form with an LOD in the middle.
        let b = label("(9) l5 default hull");
        assert_eq!(b.slot.as_deref(), Some("9"));
        assert_eq!(b.lod.as_deref(), Some("l5"));
        assert_eq!((b.permutation.as_str(), b.region.as_str()), ("default", "hull"));

        // Permutation first, region second — getting this backwards
        // mislabels 89% of the corpus.
        let c = label("(4) base shield");
        assert_eq!((c.permutation.as_str(), c.region.as_str()), ("base", "shield"));
    }

    #[test]
    fn missing_tokens_default_the_way_the_importer_does() {
        assert_eq!(label("").permutation, "default");
        assert_eq!(label("").region, "default");
        assert_eq!(label("(3)").region, "default");
        assert_eq!(label("hull").permutation, "hull");
        assert_eq!(label("hull").region, "default");
    }

    #[test]
    fn labels_round_trip() {
        for s in ["(0) Default Default", "(9) l5 default hull", "base shield", "L2 a b"] {
            let parsed = label(s);
            assert_eq!(MaterialLabel::parse(&parsed.to_label()), parsed, "{s}");
        }
    }

    #[test]
    fn only_a_two_char_l_plus_digit_is_an_lod() {
        assert!(is_lod_token("l5") && is_lod_token("L2"));
        assert!(!is_lod_token("l") && !is_lod_token("l55") && !is_lod_token("left"));
        // A permutation that merely starts with L is not an LOD token.
        assert_eq!(label("(0) left hull").permutation, "left");
    }

    /// Build a model of `n` triangles in one section, laid out along X so
    /// the locality sort has something real to order.
    fn model(n: usize, label: &str) -> JmsFile {
        let mut jms = JmsFile::default();
        jms.materials.push(JmsMaterial {
            name: "shader".into(),
            material_name: label.into(),
        });
        for i in 0..n {
            let x = i as f32;
            for k in 0..3 {
                jms.vertices.push(JmsVertex {
                    position: RealPoint3d { x, y: k as f32, z: 0.0 },
                    normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
                    tangent: None,
                    binormal: None,
                    node_sets: vec![(0, 1.0)],
                    uvs: vec![RealPoint2d { x: 0.0, y: 0.0 }],
                    color: None,
                });
            }
            let b = (i * 3) as u32;
            jms.triangles.push(JmsTriangle { material: 0, v: [b, b + 1, b + 2], region: 0 });
        }
        jms
    }

    #[test]
    fn a_section_within_budget_is_left_alone() {
        let mut jms = model(100, "(0) default hull");
        let before = jms.materials.len();
        let report = split_oversized_sections(&mut jms, &SplitBudget::default()).unwrap();
        assert!(!report.changed());
        assert_eq!(jms.materials.len(), before);
        assert_eq!(report.sections_before, 1);
        assert_eq!(report.sections_after, 1);
    }

    #[test]
    fn an_oversized_section_becomes_numbered_regions() {
        // `indices_per_triangle` is pinned so it cannot be the binding
        // ceiling here: 65,535 / 1.0 is above the 32,767 the vertex
        // ceiling gives, which keeps the arithmetic these tests were
        // written around.
        let budget = SplitBudget {
            vertices_per_triangle: 1.0,
            indices_per_triangle: 1.0,
            ..Default::default()
        };
        // 3 sections' worth at a 32767-triangle budget.
        let mut jms = model(80_000, "(0) default hull");
        let report = split_oversized_sections(&mut jms, &budget).unwrap();

        assert!(report.changed());
        assert_eq!(report.splits.len(), 1);
        assert_eq!(report.splits[0].regions, vec!["hull", "hull02", "hull03"]);
        assert_eq!(report.sections_after, 3);
        assert!(
            report.largest_section_triangles <= budget.triangles_per_section(),
            "largest section {} still over budget {}",
            report.largest_section_triangles,
            budget.triangles_per_section()
        );
        // Every triangle still points at a real material.
        for t in &jms.triangles {
            assert!((t.material as usize) < jms.materials.len());
        }
    }

    #[test]
    fn the_split_keeps_every_triangle_exactly_once() {
        // `indices_per_triangle` is pinned so it cannot be the binding
        // ceiling here: 65,535 / 1.0 is above the 32,767 the vertex
        // ceiling gives, which keeps the arithmetic these tests were
        // written around.
        let budget = SplitBudget {
            vertices_per_triangle: 1.0,
            indices_per_triangle: 1.0,
            ..Default::default()
        };
        let mut jms = model(80_000, "(0) default hull");
        let triangles_before = jms.triangles.len();
        let vertices_before = jms.vertices.len();
        split_oversized_sections(&mut jms, &budget).unwrap();
        assert_eq!(jms.triangles.len(), triangles_before, "triangles must not change");
        assert_eq!(jms.vertices.len(), vertices_before, "vertices must not change");
    }

    #[test]
    fn splitting_is_deterministic() {
        // `indices_per_triangle` is pinned so it cannot be the binding
        // ceiling here: 65,535 / 1.0 is above the 32,767 the vertex
        // ceiling gives, which keeps the arithmetic these tests were
        // written around.
        let budget = SplitBudget {
            vertices_per_triangle: 1.0,
            indices_per_triangle: 1.0,
            ..Default::default()
        };
        let mut a = model(80_000, "(0) default hull");
        let mut b = model(80_000, "(0) default hull");
        let ra = split_oversized_sections(&mut a, &budget).unwrap();
        let rb = split_oversized_sections(&mut b, &budget).unwrap();
        assert_eq!(ra, rb);
        let mats_a: Vec<_> = a.materials.iter().map(|m| m.material_name.clone()).collect();
        let mats_b: Vec<_> = b.materials.iter().map(|m| m.material_name.clone()).collect();
        assert_eq!(mats_a, mats_b);
        let tris_a: Vec<_> = a.triangles.iter().map(|t| t.material).collect();
        let tris_b: Vec<_> = b.triangles.iter().map(|t| t.material).collect();
        assert_eq!(tris_a, tris_b);
    }


    #[test]
    fn the_index_ceiling_binds_when_it_is_the_tighter_one() {
        // Vertices allow 65,535 / 1.0; indices allow 65,535 / 3.0. The
        // section limit has to be the smaller, or a section is cut to fit
        // a budget that was never the one refusing it.
        let budget = SplitBudget {
            max_vertices_per_section: 65_535,
            vertices_per_triangle: 1.0,
            max_indices_per_section: 65_535,
            indices_per_triangle: 3.0,
            ..Default::default()
        };
        assert_eq!(budget.triangles_per_section(), 21_845);

        // 50,000 triangles is inside the vertex ceiling and outside the
        // index one, so budgeting vertices alone would leave it whole and
        // the importer would then refuse it.
        let mut jms = model(50_000, "(0) default hull");
        let report = split_oversized_sections(&mut jms, &budget).unwrap();
        assert!(report.changed(), "a section over the index ceiling was left whole");
        assert!(
            report.largest_section_triangles <= budget.triangles_per_section(),
            "largest section {} still over budget {}",
            report.largest_section_triangles,
            budget.triangles_per_section()
        );

        // And with room to spare on indices, the vertex ceiling is back
        // in charge — the limit is a minimum of the two, not a swap.
        let loose = SplitBudget { indices_per_triangle: 0.5, ..budget.clone() };
        assert_eq!(loose.triangles_per_section(), 65_535);
    }

    #[test]
    fn chunks_are_spatially_coherent_not_interleaved() {
        // `indices_per_triangle` is pinned so it cannot be the binding
        // ceiling here: 65,535 / 1.0 is above the 32,767 the vertex
        // ceiling gives, which keeps the arithmetic these tests were
        // written around.
        let budget = SplitBudget {
            vertices_per_triangle: 1.0,
            indices_per_triangle: 1.0,
            ..Default::default()
        };
        let mut jms = model(80_000, "(0) default hull");
        split_oversized_sections(&mut jms, &budget).unwrap();

        // Each region should occupy a contiguous span of X, not be
        // sprinkled through the model. Compare each region's X range.
        let labels: Vec<MaterialLabel> =
            jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
        let mut spans: BTreeMap<String, (f32, f32)> = BTreeMap::new();
        for t in &jms.triangles {
            let region = labels[t.material as usize].region.clone();
            let x = jms.vertices[t.v[0] as usize].position.x;
            let e = spans.entry(region).or_insert((f32::INFINITY, f32::NEG_INFINITY));
            e.0 = e.0.min(x);
            e.1 = e.1.max(x);
        }
        assert_eq!(spans.len(), 3);
        // Total span covered by the three regions individually should be
        // close to the whole model, not 3x it (which is what interleaving
        // would give).
        let total: f32 = spans.values().map(|(lo, hi)| hi - lo).sum();
        let whole = 80_000.0f32;
        assert!(total < whole * 1.5, "regions overlap heavily: {total} vs {whole}");
    }

    #[test]
    fn a_model_needing_more_than_sixteen_regions_is_refused() {
        // `indices_per_triangle` is pinned so it cannot be the binding
        // ceiling here: 65,535 / 1.0 is above the 32,767 the vertex
        // ceiling gives, which keeps the arithmetic these tests were
        // written around.
        let budget = SplitBudget {
            vertices_per_triangle: 1.0,
            indices_per_triangle: 1.0,
            ..Default::default()
        };
        // 32767 triangles per region x 16 regions is the ceiling; ask for
        // well past it.
        let mut jms = model(700_000, "(0) default hull");
        let err = split_oversized_sections(&mut jms, &budget).unwrap_err();
        assert!(matches!(err, SplitError::TooManyRegions { .. }), "got {err:?}");
        // And nothing was modified on the way to failing.
        assert_eq!(jms.materials.len(), 1);
        assert!(jms.triangles.iter().all(|t| t.material == 0));
    }

    #[test]
    fn material_symbols_are_read_off_each_end_only() {
        // `)` is bit 13, which is the precise flag.
        assert_eq!(material_symbol_flags(")"), 1 << 13);
        assert!(material_is_precise("shader)"));
        assert!(material_is_precise(")shader"));
        // The worked example from the corpus doc: `)`, `!` and `%`.
        assert_eq!(
            material_symbol_flags("flood_fronds)!%"),
            (1 << 13) | (1 << 3) | 1,
            "a trailing run sets one bit per symbol"
        );
        // A symbol inside a word is part of the name, not a flag.
        assert_eq!(material_symbol_flags("foo)bar"), 0);
        assert!(!material_is_precise("foo)bar"));
        // Whitespace is skipped rather than ending a run.
        assert!(material_is_precise("  ) shader"));
        assert_eq!(material_symbol_flags("plain"), 0);
    }

    #[test]
    fn sections_are_keyed_by_lod_permutation_and_region_together() {
        let mut jms = JmsFile::default();
        for label in ["(0) default hull", "(1) damaged hull", "(2) l2 default hull"] {
            jms.materials.push(JmsMaterial { name: "s".into(), material_name: label.into() });
        }
        jms.vertices.push(JmsVertex {
            position: RealPoint3d::default(),
            normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
            tangent: None,
            binormal: None,
            node_sets: vec![(0, 1.0)],
            uvs: vec![],
            color: None,
        });
        for m in 0..3 {
            jms.triangles.push(JmsTriangle { material: m, v: [0, 0, 0], region: 0 });
        }
        let report = split_oversized_sections(&mut jms, &SplitBudget::default()).unwrap();
        // Same region name, but three different sections: two
        // permutations and one LOD variant.
        assert_eq!(report.sections_before, 3);
        assert_eq!(report.regions_before, 1);
    }
}
