//! Follow a Blueprint bound-event into its ubergraph and report which asset
//! imports the handler actually reaches.
//!
//! Event thunks compile to `EX_LocalFinalFunction ExecuteUbergraph, EX_IntConst
//! <entry>` — so each handler has a numeric entry offset into the ubergraph's
//! script. The ubergraph opens with a jump table (one `EX_Jump` per entry), and
//! following that jump lands on the handler's real code.
//!
//! `read_ufunction_script` truncates large functions, so the script base is
//! recovered empirically: find the offset B into the raw export payload where
//! every known entry offset lands on an `EX_Jump` opcode.
//!
//! Run: cargo run --release --features iostore --example ce_uber_trace -- <pkg-substr>

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
const EX_JUMP: u8 = 0x06;

fn norm(p: &str) -> String {
    p.to_ascii_lowercase().replace('\\', "/")
}

fn main() -> anyhow::Result<()> {
    let pkg = norm(&std::env::args().nth(1).expect("usage: <pkg-substr>"));

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let archives: Vec<Arc<IoStoreArchive>> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new)).collect();

    let bytes = archives
        .iter()
        .find_map(|a| {
            a.entries()
                .iter()
                .find(|e| norm(&e.path).contains(&pkg) && norm(&e.path).ends_with(".uasset"))
                .and_then(|e| a.read(&e.path).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("no package matching {pkg:?}"))?;

    let h = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let header_size = h.summary.header_size as usize;

    let mut import_name: BTreeMap<i32, String> = BTreeMap::new();
    for (i, idx) in h.import_map.iter().enumerate() {
        if let Some(r) = idx.package_import()
            && let Some(p) = h.imported_package_names.get(r.imported_package_index as usize)
        {
            import_name.insert(-(i as i32) - 1, p.clone());
        }
    }

    // Locate the ubergraph payload, and every thunk's entry offset.
    let mut uber: Option<(usize, usize)> = None;
    let mut entries: Vec<(String, i32)> = Vec::new();
    for ex in &h.export_map {
        let fname = h.name_map.get(ex.object_name).to_string();
        let s = header_size + ex.cooked_serial_offset as usize;
        let e = s + ex.cooked_serial_size as usize;
        let Some(payload) = bytes.get(s..e) else { continue };
        if fname.starts_with("ExecuteUbergraph") {
            uber = Some((s, e));
            continue;
        }
        // Thunk shape: 0x46 <i32 func> 0x1D <i32 entry> 0x16 0x04
        for o in 0..payload.len().saturating_sub(11) {
            if payload[o] == 0x46 && payload[o + 5] == 0x1D && payload[o + 10] == 0x16 {
                let entry = i32::from_le_bytes(payload[o + 6..o + 10].try_into().unwrap());
                entries.push((fname.clone(), entry));
                break;
            }
        }
    }
    let (us, ue) = uber.ok_or_else(|| anyhow::anyhow!("no ubergraph export"))?;
    let payload = &bytes[us..ue];
    println!("ubergraph raw payload: {} bytes", payload.len());
    println!("thunk entry offsets:");
    for (f, e) in &entries {
        println!("   {e:>6}  {f}");
    }

    // Recover the script base: every entry offset must land on EX_Jump.
    // Only the tightly-packed entries (exactly 5 apart) can be a jump table;
    // the rest point straight at handler code, whose opening opcode varies.
    let mut sorted: Vec<i32> = entries.iter().map(|(_, e)| *e).collect();
    sorted.sort_unstable();
    let table: Vec<i32> = sorted
        .iter()
        .copied()
        .filter(|e| sorted.contains(&(e - 5)) || sorted.contains(&(e + 5)))
        .collect();
    println!("\njump-table candidates (5 apart): {table:?}");
    let bases: Vec<usize> = (0..payload.len())
        .filter(|&b| {
            table.iter().all(|e| {
                let o = b + *e as usize;
                o + 5 <= payload.len() && payload[o] == EX_JUMP
            })
        })
        .collect();
    println!("candidate script bases: {bases:?}");
    let base = *bases
        .first()
        .ok_or_else(|| anyhow::anyhow!("could not recover script base"))?;
    println!("\nrecovered script base: +{base} (script = {} bytes)", payload.len() - base);

    // Where every audio import sits, in script coordinates.
    let mut refs: Vec<(usize, &String)> = Vec::new();
    for o in base..payload.len().saturating_sub(4) {
        let v = i32::from_le_bytes(payload[o..o + 4].try_into().unwrap());
        if let Some(n) = import_name.get(&v) {
            refs.push((o - base, n));
        }
    }

    println!("\nhandler regions (entry -> jump target):");
    let mut targets: Vec<(String, usize)> = Vec::new();
    for (f, e) in &entries {
        let o = base + *e as usize;
        let dest = i32::from_le_bytes(payload[o + 1..o + 5].try_into().unwrap()) as usize;
        println!("   {f}\n      entry {e} -> jumps to script offset {dest}");
        targets.push((f.clone(), dest));
    }

    // A handler's region runs from its jump target to the next handler's target.
    let mut bounds: Vec<usize> = targets.iter().map(|(_, d)| *d).collect();
    bounds.push(payload.len() - base);
    bounds.sort_unstable();
    println!("\nasset imports by owning handler region:");
    for (f, d) in &targets {
        let end = *bounds.iter().find(|b| **b > *d).unwrap_or(&(payload.len() - base));
        let owned: Vec<&(usize, &String)> =
            refs.iter().filter(|(o, _)| *o >= *d && *o < end).collect();
        if owned.is_empty() {
            continue;
        }
        println!("   {f}  [{d}..{end})");
        for (o, n) in owned {
            println!("      @{o}: {n}");
        }
    }
    Ok(())
}
