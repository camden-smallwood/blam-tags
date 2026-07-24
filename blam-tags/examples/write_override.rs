//! Write a same-size override container for one tag, for external validation.
//!   cargo run -p blam-tags --features iostore --example write_override -- \
//!     <base.utoc> <tag-basename.ubulk> <out.utoc>

use blam_tags::iostore::writer::OverrideContainerWriter;
use blam_tags::iostore::IoStoreArchive;

fn main() {
    let mut args = std::env::args().skip(1);
    let base = args.next().expect("base utoc");
    let tag = args.next().expect("tag basename");
    let out = args.next().expect("out utoc");

    let archive = IoStoreArchive::open(&base).expect("open base");
    let entry = archive
        .entries()
        .iter()
        .find(|e| e.path.ends_with(&tag))
        .expect("tag not found");
    let id = archive.chunk_id(entry.chunk_index).expect("id");
    let bytes = archive.read(&entry.path).expect("read");

    let mut w = OverrideContainerWriter::new("../../../");
    w.add_chunk(id, bytes.clone());
    w.write(std::path::Path::new(&out)).expect("write");
    println!(
        "wrote override {out} with 1 chunk id={} ({} bytes)",
        id.bytes().iter().map(|b| format!("{b:02x}")).collect::<String>(),
        bytes.len()
    );
}
