#![no_main]
//! read → write → read must be stable.
//!
//! Not that arbitrary input decodes — most will not — but that whatever *does*
//! decode re-encodes and decodes again to the same thing. A writer that is
//! wrong only for shapes the shipped corpus lacks shows up here and nowhere
//! else.
use blam_tags::iostore::object::unversioned::{read_export, write_export};
use blam_tags::iostore::usmap::Usmap;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(usmap) = Usmap::meteorite() else { return };
    let names: Vec<String> = (0..64).map(|i| format!("Name{i}")).collect();
    for class in ["FontFace", "HaloAudioCategory", "StaticMeshComponent"] {
        let Ok(first) = read_export(data, &names, &usmap, class, 0) else { continue };
        let Ok(bytes) = write_export(class, &first, &usmap) else { continue };
        // Re-reading what we just wrote must succeed and produce the same bytes.
        let Ok(second) = read_export(&bytes, &names, &usmap, class, 0) else {
            panic!("{class}: re-reading our own output failed");
        };
        let again = write_export(class, &second, &usmap)
            .unwrap_or_else(|e| panic!("{class}: re-writing our own output failed: {e}"));
        assert_eq!(bytes, again, "{class}: write is not idempotent");
    }
});
