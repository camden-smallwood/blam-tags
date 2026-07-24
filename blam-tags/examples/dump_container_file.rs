//! Dump the decompressed bytes of one or more container files to disk, for
//! analysis. Usage:
//!   cargo run -p blam-tags --features iostore --example dump_container_file -- \
//!     <utoc> <rel_path> [<rel_path> ...]
//! Writes each to /tmp/<basename>.

use blam_tags::iostore::IoStoreArchive;

fn main() {
    let mut args = std::env::args().skip(1);
    let utoc = args.next().expect("utoc path");
    let archive = IoStoreArchive::open(&utoc).expect("open");
    for rel in args {
        // Allow a suffix match so the caller can pass just a basename.
        let entry = archive
            .entries()
            .iter()
            .find(|e| e.path == rel || e.path.ends_with(&rel))
            .unwrap_or_else(|| panic!("not found: {rel}"));
        let bytes = archive.read(&entry.path).expect("read");
        let id = archive.chunk_id(entry.chunk_index).expect("chunk id");
        let base = entry.path.rsplit('/').next().unwrap();
        let out = format!("/tmp/{base}");
        std::fs::write(&out, &bytes).unwrap();
        println!(
            "{} -> {} ({} bytes)  chunk_id={} type={}",
            entry.path,
            out,
            bytes.len(),
            id.bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
            id.chunk_type()
        );
    }
}
