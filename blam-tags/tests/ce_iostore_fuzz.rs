//! Malformed input must produce an error, never a panic.
//!
//! This parser reads third-party mod containers — files no one in this project
//! produced and nothing validates before they reach it. Every other test here
//! feeds it bytes the cooker wrote, which exercises exactly the paths that were
//! designed to work. The interesting inputs are the ones that were not.
//!
//! Deterministic on purpose: a fixed LCG, a fixed set of mutations, and the
//! committed fixtures. A failure reproduces from the printed seed rather than
//! being a story about a fuzzer someone ran once. It runs in CI in well under a
//! second, which a libFuzzer target cannot (it needs nightly and does not
//! terminate). `fuzz/` holds the unbounded libFuzzer targets — `read_package`
//! and `export_roundtrip` — for when that is wanted.
//!
//! The contract under test is narrow and total: for *any* byte string, the read
//! path returns `Ok` or `Err`. It may not unwind, and it may not hang.

#![cfg(feature = "iostore")]

use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::read_export;
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;

const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

const FIXTURES: &[&str] = &[
    "removals",
    "zero-masked",
    "static-array",
    "native-struct",
    "text",
    "leading-empty",
    "multi-export",
    "string",
];

/// Reproducible pseudo-randomness. Not good randomness — good *reproducibility*.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 16
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Iterations per fixture. Small enough that CI does not notice, and raisable
/// for a real hunt: `BLAM_FUZZ_ITERS=200000 cargo test --test ce_iostore_fuzz`.
fn iterations(default: u64) -> u64 {
    std::env::var("BLAM_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ce");
    FIXTURES
        .iter()
        .map(|n| {
            let p = dir.join(format!("{n}.uasset"));
            let b = std::fs::read(&p).unwrap_or_else(|e| panic!("fixture {}: {e}", p.display()));
            (n.to_string(), b)
        })
        .collect()
}

/// One deliberate corruption. Each targets a different way a parser breaks:
/// a length that is now absurd, a count that overflows when multiplied, an
/// offset past the end, a truncated stream.
fn mutate(bytes: &[u8], rng: &mut Lcg) -> Vec<u8> {
    let mut out = bytes.to_vec();
    if out.is_empty() {
        return out;
    }
    match rng.below(6) {
        // Flip a single byte — the cheapest way to reach a bad discriminant.
        0 => {
            let i = rng.below(out.len());
            out[i] ^= 1 << rng.below(8);
        }
        // Truncate. Every "read N more bytes" has to cope.
        1 => {
            let n = rng.below(out.len());
            out.truncate(n);
        }
        // Write a huge value where a count or length plausibly lives, which is
        // what turns a bad parse into an allocation or an overflow.
        2 => {
            let i = rng.below(out.len().saturating_sub(4).max(1));
            if i + 4 <= out.len() {
                out[i..i + 4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
            }
        }
        // A negative i32 — the sign is load-bearing in several places
        // (FString encoding, INDEX_NONE removal counts).
        3 => {
            let i = rng.below(out.len().saturating_sub(4).max(1));
            if i + 4 <= out.len() {
                out[i..i + 4].copy_from_slice(&(-1i32).to_le_bytes());
            }
        }
        // Zero a span — turns valid structure into a sea of empty.
        4 => {
            let i = rng.below(out.len());
            let n = rng.below(out.len() - i).min(64);
            out[i..i + n].fill(0);
        }
        // Splice: duplicate a chunk, so offsets and totals disagree.
        _ => {
            let i = rng.below(out.len());
            let n = rng.below(out.len() - i).min(32);
            let chunk = out[i..i + n].to_vec();
            out.splice(i..i, chunk);
        }
    }
    out
}

/// Run the whole read path over some bytes, ignoring every error. The only
/// thing being asserted is that it *returns*.
fn read_everything(bytes: &[u8], usmap: &Usmap) {
    let Ok(header) = FZenPackageHeader::deserialize(&mut Cursor::new(bytes), None, CV, HV, None)
    else {
        return;
    };
    let Ok(payloads) = read_payloads(&header, bytes) else { return };
    let names = header.name_map.copy_raw_names();
    for (i, ex) in header.export_map.iter().enumerate() {
        // The class name is not recoverable from mutated bytes, so try the ones
        // the fixtures use plus a couple that exercise different tail paths.
        for class in ["FontFace", "HaloAudioCategory", "WidgetTree", "StaticMeshComponent"] {
            let _ = read_export(&payloads[i], &names, usmap, class, ex.object_flags);
        }
    }
}

/// The contract: any byte string in, `Ok` or `Err` out, never a panic.
#[test]
fn mutated_packages_never_panic() {
    let usmap = Usmap::meteorite().expect("bundled usmap");
    let fixtures = fixtures();
    let mut failures: Vec<String> = Vec::new();

    for (name, original) in &fixtures {
        for seed in 0..iterations(250) {
            let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15) ^ 0xDEADBEEF);
            // Two mutations, so corruptions can interact.
            let once = mutate(original, &mut rng);
            let bytes = mutate(&once, &mut rng);

            let result = catch_unwind(AssertUnwindSafe(|| read_everything(&bytes, &usmap)));
            if result.is_err() {
                failures.push(format!("{name} seed {seed}: panicked on mutated input"));
                if failures.len() >= 10 {
                    break;
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "malformed input must produce an error, not a panic:\n  {}",
        failures.join("\n  ")
    );
}

/// Arbitrary bytes, not just corrupted packages — the case where a caller is
/// handed something that was never a package at all.
#[test]
fn arbitrary_bytes_never_panic() {
    let usmap = Usmap::meteorite().expect("bundled usmap");
    let mut failures = Vec::new();
    for seed in 0..iterations(400) {
        let mut rng = Lcg(seed ^ 0x5EED_1234_5678);
        let len = rng.below(2048);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        if catch_unwind(AssertUnwindSafe(|| read_everything(&bytes, &usmap))).is_err() {
            failures.push(format!("seed {seed} ({len} bytes) panicked"));
            if failures.len() >= 10 {
                break;
            }
        }
    }
    assert!(failures.is_empty(), "random input must not panic:\n  {}", failures.join("\n  "));
}
