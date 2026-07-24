//! Overwrite one tag in a container IN PLACE (destructive — run on a COPY).
//!   cargo run -p blam-tags --features iostore --example overwrite_in_place -- <utoc> <tag.ubulk>
//! Flips the tag's last byte (same-size edit) and rewrites the container.

use blam_tags::iostore::writer::overwrite_tag_in_place;
use blam_tags::iostore::IoStoreArchive;

fn main() {
    let mut args = std::env::args().skip(1);
    let utoc = args.next().unwrap();
    let tag = args.next().unwrap();

    let (rel, mut bytes) = {
        let a = IoStoreArchive::open(&utoc).unwrap();
        let e = a.entries().iter().find(|e| e.path.ends_with(&tag)).expect("tag not found");
        (e.path.clone(), a.read(&e.path).unwrap())
    };
    let n = bytes.len();
    bytes[n - 1] ^= 0xff; // same-size edit
    overwrite_tag_in_place(std::path::Path::new(&utoc), &rel, &bytes).unwrap();
    println!("overwrote {rel} ({n} bytes) in place");
}
