//! Validate that a tag .uasset's Zen package HEADER survives
//! deserialize -> serialize byte-identically (the trust anchor for generating
//! new/renamed packages). Compares the re-serialized header against the
//! original's first `header_size` bytes.
//!
//!   cargo run -p blam-tags --features iostore --example zen_roundtrip -- <file.uasset> ...
//!
//! Ported from trumank/retoc (MIT) examples/roundtrip_zen.rs

use std::io::Cursor;

use blam_tags::iostore::container_header::{EIoContainerHeaderVersion, StoreEntry};
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;

fn main() {
    let cv = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
    let ver = std::env::var("HV").ok().and_then(|s| s.parse::<u8>().ok()).unwrap_or(4);
    let candidates = [(
        "hv",
        match ver {
            3 => EIoContainerHeaderVersion::NoExportInfo,
            5 => EIoContainerHeaderVersion::SoftPackageReferencesOffset,
            _ => EIoContainerHeaderVersion::SoftPackageReferences,
        },
    )];
    for path in std::env::args().skip(1) {
        let bytes = std::fs::read(&path).unwrap();
        let mut ok = false;
        for (label, hv) in candidates {
            let mut cur = Cursor::new(bytes.as_slice());
            let hdr = match FZenPackageHeader::deserialize(&mut cur, None, cv, hv, None) {
                Ok(h) => h,
                Err(e) => {
                    println!("{path} [{label}]: deserialize error: {e}");
                    continue;
                }
            };
            let header_size = hdr.summary.header_size as usize;

            let mut out = Cursor::new(Vec::new());
            let mut store = StoreEntry::default();
            if let Err(e) = hdr.serialize(&mut out, &mut store, hv) {
                println!("{path} [{label}]: serialize error: {e}");
                continue;
            }
            let reser = out.into_inner();
            let orig = &bytes[..header_size.min(bytes.len())];
            let identical = reser.as_slice() == orig;
            let first_diff = orig
                .iter()
                .zip(reser.iter())
                .position(|(a, b)| a != b)
                .map(|i| format!("0x{i:x}"))
                .unwrap_or_else(|| "-".into());
            println!(
                "{} [{label}]: header_size={} reser={} identical={} first_diff={}",
                path.rsplit('/').next().unwrap(),
                header_size,
                reser.len(),
                identical,
                first_diff
            );
            ok = true;
            break;
        }
        if !ok {
            println!("{path}: no candidate header version parsed");
        }
    }
}
