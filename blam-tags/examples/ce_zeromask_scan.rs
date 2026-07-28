//! Does ANY tag export use a has-zeroes fragment (and therefore a zero mask)?
//! Decides whether the property writer needs zero-mask support at all.
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main() {
    // direct hash checks for the two extra script imports on the customization globals tag
    for p in ["/Script/BlamSynchronization.BlamCustomizationGlobalsTagDataIndices",
              "/Script/GameplayTags.GameplayTag",
              "/Script/BlamSynchronization.BlamVariant",
              "/Script/CoreUObject.SoftObjectPath"] {
        println!("{:016X}  {p}", FPackageObjectIndex::create_script_import(p).raw_index());
    }
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let (mut n, mut with_zero, mut frags_total, mut max_frags) = (0usize, 0usize, 0usize, 0usize);
    let mut heads = std::collections::BTreeMap::new();
    let mut zmask_total = 0usize; let mut tail_nonzero = 0usize; let mut tails = std::collections::BTreeMap::new();
    let mut samples = Vec::new();
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") { continue }
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None) else { continue };
            let Some(ex) = h.export_map.first() else { continue };
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            if off + 2 > ua.len() { continue }
            n += 1;
            // walk the FUnversionedHeader fragments
            let mut o = off; let mut zero = false; let mut cnt = 0usize; let mut zmask_bits = 0usize;
            loop {
                if o + 2 > ua.len() { break }
                let f = u16::from_le_bytes([ua[o], ua[o+1]]); o += 2; cnt += 1;
                let has_zeroes = (f & 0x0080) != 0;   // bit 7
                let is_last    = (f & 0x0100) != 0;   // bit 8
                let vnum       = (f >> 9) as usize;
                if has_zeroes { zmask_bits += vnum; }
                if has_zeroes { zero = true }
                if is_last || cnt > 64 { break }
            }
            frags_total += cnt; max_frags = max_frags.max(cnt); zmask_total += zmask_bits;
            if zero { with_zero += 1; if samples.len() < 6 { samples.push(format!("{} ({} bits)", h.package_name(), zmask_bits)) } }
            // LEADING bytes of the export body
            {
                let b=&ua[off..(off+4).min(ua.len())];
                if b.len()==4 { *heads.entry(format!("{:02x}{:02x}{:02x}{:02x}", b[0],b[1],b[2],b[3])).or_insert(0usize)+=1; }
            }
            // trailing bytes after the last property value
            let end = off + ex.cooked_serial_size as usize;
            if end <= ua.len() && end >= 4 {
                let t = &ua[end-4..end];
                *tails.entry(format!("{:02x}{:02x}{:02x}{:02x}", t[0],t[1],t[2],t[3])).or_insert(0usize) += 1;
                if t != [0,0,0,0] { tail_nonzero += 1; }
            }
        }
    }
    println!("\n{n} tag exports scanned");
    println!("  use a has-zeroes fragment : {with_zero}");
    println!("  fragments: total {frags_total}, max per export {max_frags}, avg {:.2}", frags_total as f64 / n as f64);
    println!("  zero-mask bits total: {zmask_total}");
    println!("  FIRST 4 bytes of the export body: {:?}", heads);
    println!("  last 4 bytes of the export body: {:?}", tails);
    println!("  non-zero tails: {tail_nonzero}");
    for s in &samples { println!("     {s}") }
}
