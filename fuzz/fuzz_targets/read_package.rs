#![no_main]
//! Any byte string must parse to `Ok` or `Err`, never a panic.
//!
//! The bounded, deterministic version of this lives in
//! `blam-tags/tests/ce_iostore_fuzz.rs` and runs in CI. This one runs
//! unbounded, needs nightly, and is for a real hunt:
//!
//! ```text
//! cargo +nightly fuzz run read_package
//! ```
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::read_export;
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use libfuzzer_sys::fuzz_target;

const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fuzz_target!(|data: &[u8]| {
    let Ok(usmap) = Usmap::meteorite() else { return };
    let Ok(header) = FZenPackageHeader::deserialize(&mut Cursor::new(data), None, CV, HV, None)
    else {
        return;
    };
    let Ok(payloads) = read_payloads(&header, data) else { return };
    let names = header.name_map.copy_raw_names();
    for (i, ex) in header.export_map.iter().enumerate() {
        for class in ["FontFace", "HaloAudioCategory", "WidgetTree", "StaticMeshComponent"] {
            let _ = read_export(&payloads[i], &names, &usmap, class, ex.object_flags);
        }
    }
});
