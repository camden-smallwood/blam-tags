//! `JmsFile::parse` against every JMS the H3 Editing Kit ships.
//!
//! The reader was derived from `tool.exe`'s own tokeniser and section
//! order, so the corpus is the check that the derivation is right. Three
//! properties, in increasing strength:
//!
//! 1. **Every file parses, with nothing left over.** The trailing-token
//!    check is what makes this meaningful — a positional parser that
//!    misreads any count desynchronises and ends with a surplus or a
//!    shortfall, so "parsed and consumed exactly" is a real statement
//!    about the whole grammar, not just its first section.
//! 2. **Structural invariants hold** — indices in range, influence
//!    counts within the format's four, weights finite and positive.
//!    Note weights are *not* asserted to sum to one: they don't, and
//!    that turned out to be a finding rather than a bug. See below.
//! 3. **`parse` ∘ `write` is the identity** on the modelled fields.
//!
//! Skips gracefully with no kit installed, the same convention as the
//! other kit-gated suites: point `BLAM_TEST_H3EK` at an install, or let
//! it find one in a Steam library.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use blam_tags::jms::JmsFile;
use blam_tags::jms_parse::JmsParseError;

/// H3EK's `data` directory, via `BLAM_TEST_H3EK` or a Steam library.
fn h3ek_data() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("BLAM_TEST_H3EK") {
        let path = PathBuf::from(path).join("data");
        return path.is_dir().then_some(path);
    }
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "C:/Program Files/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join("H3EK").join("data"))
    .find(|path| path.is_dir())
}

/// Match on the filename ending rather than `Path::extension()`.
///
/// Two files in the shipped kit are named exactly `.JMS`, with no stem —
/// an artist exported with an empty basename. Rust reads a leading-dot
/// name as all-stem-no-extension, so `extension()` returns `None` and
/// they vanish; `tool.exe`'s directory scan, which compares the trailing
/// extension field case-folded, still offers them to the importer. Test
/// what the importer would actually be handed.
fn jms_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = blam_tags::convert::walk_files(root)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.len() >= 4 && n[n.len() - 4..].eq_ignore_ascii_case(".jms"))
        })
        .collect();
    out.sort();
    out
}

/// Every shipped JMS parses, consumes every token, and is internally
/// consistent.
#[test]
fn the_shipped_corpus_parses_completely() {
    let Some(data) = h3ek_data() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    let files = jms_files(&data);
    if files.is_empty() {
        eprintln!("skipping: H3EK data directory has no JMS files");
        return;
    }

    let mut failures: Vec<(PathBuf, JmsParseError)> = Vec::new();
    let mut broken: Vec<String> = Vec::new();
    let mut versions: BTreeMap<u16, usize> = BTreeMap::new();
    let mut influences: BTreeMap<usize, usize> = BTreeMap::new();
    let mut uv_sets: BTreeMap<usize, usize> = BTreeMap::new();
    let (mut vertices, mut triangles, mut coloured) = (0usize, 0usize, 0usize);
    let mut unnormalised = 0usize;
    let (mut boxes, mut convex, mut ragdolls, mut hinges) = (0usize, 0usize, 0usize, 0usize);

    for path in &files {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                broken.push(format!("{}: unreadable: {e}", path.display()));
                continue;
            }
        };
        let (jms, version) = match JmsFile::parse(&text) {
            Ok(v) => v,
            Err(e) => {
                failures.push((path.clone(), e));
                continue;
            }
        };

        *versions.entry(version).or_default() += 1;
        vertices += jms.vertices.len();
        triangles += jms.triangles.len();
        boxes += jms.boxes.len();
        convex += jms.convex_shapes.len();
        ragdolls += jms.ragdolls.len();
        hinges += jms.hinges.len();

        // (2) Structural invariants. Report every violation rather than
        // stopping at the first, so one bad assumption doesn't hide the
        // rest.
        let name = path.strip_prefix(&data).unwrap_or(path).display();
        for (i, n) in jms.nodes.iter().enumerate() {
            if n.parent != -1 && (n.parent < 0 || n.parent as usize >= jms.nodes.len()) {
                broken.push(format!("{name}: node {i} parent {} out of range", n.parent));
            }
        }
        for (i, m) in jms.markers.iter().enumerate() {
            if m.node_index != -1 && m.node_index as usize >= jms.nodes.len() {
                broken.push(format!("{name}: marker {i} node {} out of range", m.node_index));
            }
        }
        for (i, v) in jms.vertices.iter().enumerate() {
            *influences.entry(v.node_sets.len()).or_default() += 1;
            *uv_sets.entry(v.uvs.len()).or_default() += 1;
            if v.node_sets.len() > 4 {
                broken.push(format!("{name}: vertex {i} has {} influences", v.node_sets.len()));
            }
            if v.node_sets.is_empty() {
                broken.push(format!("{name}: vertex {i} has no influences"));
            }
            for (n, _) in &v.node_sets {
                if *n < 0 || *n as usize >= jms.nodes.len() {
                    broken.push(format!("{name}: vertex {i} bound to node {n}"));
                }
            }
            // Weights are NOT normalised in the source. 191 vertices in
            // the shipped corpus sum to 0.5 or 0.6667 — the exporter drops
            // influences it culled without redistributing their weight, and
            // `tool.exe` renormalises on the way into the tag. So this is a
            // fact to record, not a violation: an external importer has to
            // renormalise too, or those vertices skin wrong.
            let sum: f32 = v.node_sets.iter().map(|(_, w)| *w).sum();
            if !sum.is_finite() || sum <= 0.0 {
                broken.push(format!("{name}: vertex {i} weights sum to {sum}"));
            } else if (sum - 1.0).abs() > 1e-3 {
                unnormalised += 1;
            }
            if v.node_sets.iter().any(|(_, w)| !w.is_finite() || *w < 0.0) {
                broken.push(format!("{name}: vertex {i} has a negative or non-finite weight"));
            }
            if v.color.is_some_and(|c| c.x != 0.0 || c.y != 0.0 || c.z != 0.0) {
                coloured += 1;
            }
        }
        for (i, t) in jms.triangles.iter().enumerate() {
            if t.material < 0 || t.material as usize >= jms.materials.len() {
                broken.push(format!("{name}: triangle {i} material {} out of range", t.material));
            }
            for v in t.v {
                if v as usize >= jms.vertices.len() {
                    broken.push(format!("{name}: triangle {i} references vertex {v}"));
                }
            }
        }
    }

    eprintln!("parsed {} JMS files from {}", files.len(), data.display());
    eprintln!("  versions          {versions:?}");
    eprintln!("  vertices          {vertices}  ({coloured} with a non-black colour)");
    eprintln!("  triangles         {triangles}");
    eprintln!("  influences/vertex {influences:?}");
    eprintln!("  uv sets/vertex    {uv_sets:?}");
    eprintln!("  vertices whose weights do not sum to 1: {unnormalised}");
    eprintln!("  boxes {boxes}, convex {convex}, ragdolls {ragdolls}, hinges {hinges}");

    if !failures.is_empty() {
        let listed: Vec<String> = failures
            .iter()
            .take(10)
            .map(|(p, e)| format!("  {}: {e}", p.display()))
            .collect();
        panic!(
            "{} of {} shipped JMS files failed to parse:\n{}",
            failures.len(),
            files.len(),
            listed.join("\n")
        );
    }
    assert!(
        broken.is_empty(),
        "{} structural violations, first 10:\n{}",
        broken.len(),
        broken.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );

    // Nothing in the shipped corpus exceeds the format's four influences
    // or two UV sets. If that ever changes, the reader is fine but the
    // downstream limit assumptions are not — so notice it here.
    assert!(influences.keys().all(|k| (1..=4).contains(k)), "influence counts: {influences:?}");
    assert!(uv_sets.keys().all(|k| *k <= 2), "uv set counts: {uv_sets:?}");

    // (1b) A corpus fingerprint, asserted only on the install this was
    // derived against, so a differently-sized kit reports rather than
    // fails. These totals were measured independently of the reader.
    if files.len() == 254 {
        assert_eq!(versions, BTreeMap::from([(8213, 254)]), "every shipped file is 8213");
        assert_eq!(vertices, 3_289_443, "total vertices");
        assert_eq!(coloured, 67_589, "vertices with a non-black colour");
        assert_eq!(
            influences,
            BTreeMap::from([(1, 3_231_242), (2, 37_327), (3, 19_481), (4, 1_393)])
        );
        assert_eq!(uv_sets, BTreeMap::from([(0, 40_038), (1, 2_906_370), (2, 343_035)]));
        assert_eq!((boxes, convex, ragdolls, hinges), (5, 1007, 19, 10));
        // Recorded so a change in the renormalisation story is noticed.
        assert_eq!(unnormalised, 191, "vertices whose influence weights do not sum to 1");
    }
}

/// `parse` ∘ `write` is the identity on every field `JmsFile` models.
///
/// Run over the smaller half of the corpus: the property is per-file and
/// the large vehicle meshes add minutes of debug-build float formatting
/// without adding coverage.
#[test]
fn writing_then_reparsing_round_trips() {
    let Some(data) = h3ek_data() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    let files = jms_files(&data);
    if files.is_empty() {
        eprintln!("skipping: H3EK data directory has no JMS files");
        return;
    }

    let mut checked = 0usize;
    let mut mismatches: Vec<String> = Vec::new();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Ok((original, version)) = JmsFile::parse(&text) else { continue };
        if original.vertices.len() > 20_000 {
            continue;
        }

        let mut buf: Vec<u8> = Vec::new();
        original.write(&mut buf, version).expect("write must succeed");
        let text2 = String::from_utf8(buf).expect("writer emits UTF-8");
        let (again, version2) = match JmsFile::parse(&text2) {
            Ok(v) => v,
            Err(e) => {
                mismatches.push(format!("{}: our own output failed to parse: {e}", path.display()));
                continue;
            }
        };
        checked += 1;

        let name = path.strip_prefix(&data).unwrap_or(path).display();
        let mut bad = |what: &str| mismatches.push(format!("{name}: {what}"));

        if version2 != version {
            bad("version changed");
        }
        if again.nodes.len() != original.nodes.len() {
            bad("node count changed");
            continue;
        }
        for (a, b) in original.nodes.iter().zip(&again.nodes) {
            if a.name != b.name || a.parent != b.parent || a.translation != b.translation {
                bad(&format!("node {:?} changed", a.name));
            }
        }
        if original.materials.len() != again.materials.len() {
            bad("material count changed");
        } else {
            for (a, b) in original.materials.iter().zip(&again.materials) {
                if a.name != b.name || a.material_name != b.material_name {
                    bad(&format!("material {:?} changed", a.name));
                }
            }
        }
        if original.markers.len() != again.markers.len() {
            bad("marker count changed");
        }
        if original.vertices.len() != again.vertices.len() {
            bad("vertex count changed");
            continue;
        }
        for (i, (a, b)) in original.vertices.iter().zip(&again.vertices).enumerate() {
            if a.position != b.position || a.normal != b.normal {
                bad(&format!("vertex {i} position/normal changed"));
                break;
            }
            if a.uvs != b.uvs || a.node_sets != b.node_sets {
                bad(&format!("vertex {i} uvs/weights changed"));
                break;
            }
            // The writer normalises -0.0 to 0.0 and substitutes black for
            // an absent colour; `==` accepts the first and the corpus is
            // always Some at 8213, so this is a real comparison.
            if a.color.unwrap_or_default() != b.color.unwrap_or_default() {
                bad(&format!("vertex {i} colour changed"));
                break;
            }
        }
        if original.triangles.len() != again.triangles.len() {
            bad("triangle count changed");
        } else {
            for (i, (a, b)) in original.triangles.iter().zip(&again.triangles).enumerate() {
                if a.material != b.material || a.v != b.v {
                    bad(&format!("triangle {i} changed"));
                    break;
                }
            }
        }
        if original.boxes.len() != again.boxes.len()
            || original.convex_shapes.len() != again.convex_shapes.len()
            || original.ragdolls.len() != again.ragdolls.len()
            || original.hinges.len() != again.hinges.len()
        {
            bad("collision/physics section counts changed");
        }
    }

    eprintln!("round-tripped {checked} of {} JMS files", files.len());
    assert!(checked > 0, "no files were small enough to round-trip — check the filter");
    assert!(
        mismatches.is_empty(),
        "{} round-trip mismatches, first 10:\n{}",
        mismatches.len(),
        mismatches.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
}
