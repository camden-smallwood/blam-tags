//! Decode an animation-Blueprint CDO's property block and whatever follows it.
//!
//! The 136 exports `ce_absent_trailer` flags are all of this shape, and this is
//! the bench for them. What it has established so far, so the next attempt does
//! not re-walk it:
//!
//!  * **The block is not obviously short.** Its header declares exactly the 7
//!    properties the Blueprint adds, and every nested `FAnimNode_*` block
//!    decodes to sensible values (`MaxTransitionsPerFrame = 3`, `SlotName =
//!    "Facial_MouthOverride"`, a `LinkID`), consuming 65 of 157 bytes.
//!  * **The 92 stranded bytes are not an `FBoneContainer`**, which is what
//!    `UAnimInstance::Serialize` writes. Searched every start offset with and
//!    without a leading guid flag, against the exact field list in
//!    `operator<<` (BoneContainer.h:487) and `TBitArray::Serialize`
//!    (BitArray.h): no parse ends on the last byte. The closest is a plausible
//!    but incomplete reading at +8 (6 required bones, 37 skeleton bones) that
//!    stops 24 bytes early.
//!  * **They are not a second block for any class in the chain** — tried the
//!    generated class, `AnimInstance` and `Object`; each fails.
//!  * They *do* parse as a well-formed unversioned header (12 values from index
//!    4, one zero-masked) and they end with five identical 7-byte property
//!    blocks, which reads like an array of small structs.
//!
//! Also ruled out elsewhere: sparse class data (`IsTransacting()`-only,
//! Obj.cpp:1722), a `SerializeDefaultObject` override on
//! `UAnimBlueprintGeneratedClass` (there is none — only `Serialize` at
//! AnimBlueprintGeneratedClass.cpp:457, which belongs to the class export, not
//! the CDO), and a missing nested schema.
//!
//! Run: `ce_cdo_block <substring>`
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{
    flattened_schema, parse_header, read_export_in, ExportContext,
};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let want = std::env::args().nth(1).expect("substring").to_ascii_lowercase();
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    world.register_generated_classes();
    let usmap = world.usmap();

    for a in world.archives() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.contains(&want) || !lo.ends_with(".uasset") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let names = h.name_map.copy_raw_names();
            let resolver = world.resolver(&h, &b, &names);
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for ex in &h.export_map {
                if ex.object_flags & 0x10 == 0 {
                    continue; // CDOs only
                }
                let Some(class) = world.class_key(&h, ex.class_index) else { continue };
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                let Ok(parts) = read_export_in(body, &names, usmap, &class, ex.object_flags, &ctx)
                else {
                    continue;
                };
                let Ok(flat) = flattened_schema(&class, usmap) else { continue };
                println!("{} :: {}", h.package_name(), h.name_map.get(ex.object_name));
                println!("  class {class}  ({} flattened slots)", flat.len());
                println!("  export {} bytes, block+trailer {} , leftover {}",
                    body.len(), body.len() - parts.tail.len(), parts.tail.len());
                if let Ok((hdr, used)) = parse_header(body) {
                    println!("  FIRST header {used}B present={:?}", hdr.present);
                    for (i, _) in &hdr.present {
                        let n = flat.get(*i).map(|(p, _, o)| format!("{} <- {o}", p.name));
                        println!("      {i}: {}", n.unwrap_or("<beyond>".into()));
                    }
                }
                println!("TAILHEX {}", parts.tail.iter().map(|x| format!("{x:02x}")).collect::<String>());
                println!("  block bytes: {}", body[..65.min(body.len())]
                    .iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "));
                if let Some(bl) = parts.properties() {
                    for entry in &bl.entries {
                        let n = match &entry.value {
                            blam_tags::iostore::object::unversioned::PropValue::Struct(inner) =>
                                format!("Struct block, {} entries, schema_len {:?}", inner.entries.len(), inner.layout),
                            other => format!("{other:?}"),
                        };
                        println!("    {} = {n}", entry.name);
                        if entry.name.contains("StateMachine") || entry.name.contains("Slot") {
                            if let blam_tags::iostore::object::unversioned::PropValue::Struct(inner) = &entry.value {
                                for ie in &inner.entries {
                                    println!("        {} slot={:?} = {:?}", ie.name, ie.slot.map(|s| s.index), ie.value);
                                }
                            }
                        }
                    }
                }
                if let Ok((hdr, used)) = parse_header(&parts.tail) {
                    println!("  SECOND header {used}B present={:?}", hdr.present);
                    for (i, _) in &hdr.present {
                        let n = flat.get(*i).map(|(p, _, o)| format!("{} <- {o}", p.name));
                        println!("      {i}: {}", n.unwrap_or("<beyond>".into()));
                    }
                    println!("  second values {} bytes", parts.tail.len() - used);
                    // Whose schema is the second block written against?
                    let mut cands: Vec<String> = vec![class.clone()];
                    let mut cur = class.clone();
                    while let Some(sup) = usmap.get(&cur).and_then(|s| s.super_name.clone()) {
                        cands.push(sup.clone());
                        cur = sup;
                    }
                    // Brute force: does the stranded span parse as a block of
                    // *any* known struct, consuming exactly?
                    for skip in [0usize, 4] {
                        let span = &parts.tail[skip..];
                        let mut fits: Vec<&str> = Vec::new();
                        for st in &usmap.structs {
                            if let Ok((_, n)) = blam_tags::iostore::object::unversioned::read_export_struct_len_in(
                                span, &names, usmap, &st.name, &ctx,
                            ) {
                                if n == span.len() {
                                    fits.push(&st.name);
                                }
                            }
                        }
                        println!("  exact-fit schemas for span[{skip}..] ({} bytes): {}",
                            span.len(),
                            if fits.is_empty() { "NONE".to_string() } else { fits.join(", ") });
                    }
                    for cand in &cands {
                        match blam_tags::iostore::object::unversioned::read_export_struct_len(
                            &parts.tail, &names, usmap, cand,
                        ) {
                            Ok((blk, n)) if n == parts.tail.len() => println!(
                                "  >>> EXACT FIT against {cand}: {n} bytes, {} entries: {}",
                                blk.entries.len(),
                                blk.entries.iter().map(|e| e.name.to_string()).collect::<Vec<_>>().join(", ")
                            ),
                            Ok((_, n)) => println!("      {cand}: consumed {n} of {}", parts.tail.len()),
                            Err(e) => println!("      {cand}: {e}"),
                        }
                    }
                }
                return;
            }
        }
    }
}
