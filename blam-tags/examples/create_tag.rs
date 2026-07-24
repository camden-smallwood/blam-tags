//! Generate a new/renamed tag override container natively (no retoc).
//!   cargo run -p blam-tags --features iostore --example create_tag -- \
//!     <template.uasset> <tag.ubulk> <new-package-path> <out.utoc> [redirect-from]

use blam_tags::iostore::writer::write_new_tag_container;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let template = std::fs::read(&a[0]).unwrap();
    let ubulk = std::fs::read(&a[1]).unwrap();
    let new_pkg = &a[2];
    let out = std::path::Path::new(&a[3]);
    let redirect = a.get(4).map(String::as_str);

    write_new_tag_container(&template, &ubulk, new_pkg, redirect, out).unwrap();
    println!("wrote {}", out.display());
    println!("  new package: {new_pkg}");
    if let Some(r) = redirect {
        println!("  redirect from: {r}");
    }
}
