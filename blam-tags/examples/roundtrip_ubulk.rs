//! Read each given `.ubulk` (a raw Reach tag), re-serialize via
//! `write_to_bytes`, and report whether the output is byte-identical and
//! same-length. Validates that an unmodified edit is a true no-op (critical for
//! same-size overlay writes).
//!
//!   cargo run -p blam-tags --features iostore --example roundtrip_ubulk -- <file.ubulk> ...

use blam_tags::file::TagFile;

fn main() {
    for path in std::env::args().skip(1) {
        let orig = std::fs::read(&path).expect("read file");
        let tag = match TagFile::read_from_bytes(&orig) {
            Ok(t) => t,
            Err(e) => {
                println!("{path}: NOT A TAG / parse error: {e}");
                continue;
            }
        };
        let out = tag.write_to_bytes().expect("write_to_bytes");
        let same_len = out.len() == orig.len();
        let identical = out == orig;
        let first_diff = orig
            .iter()
            .zip(out.iter())
            .position(|(a, b)| a != b)
            .map(|i| format!("0x{i:x}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "{}: in={} out={} same_len={} identical={} first_diff={}",
            path.rsplit('/').next().unwrap(),
            orig.len(),
            out.len(),
            same_len,
            identical,
            first_diff
        );
    }
}
