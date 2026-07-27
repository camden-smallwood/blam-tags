//! Dump one UFunction's compiled Kismet bytecode from a cooked package, with a
//! light annotation pass over the opcodes a Blueprint event thunk is made of.
//!
//! `read_ufunction_script` is known to truncate large functions, so the raw
//! export size is always reported alongside the parsed script size — if they
//! disagree wildly, trust the raw dump.
//!
//! Run: cargo run --release --features iostore --example ce_fn_dump -- <pkg-substr> <fn-substr>

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::read_ufunction_script;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn norm(p: &str) -> String {
    p.to_ascii_lowercase().replace('\\', "/")
}

/// The subset of EExprToken that shows up in event thunks and simple graphs.
fn opname(b: u8) -> &'static str {
    match b {
        0x00 => "EX_LocalVariable",
        0x01 => "EX_InstanceVariable",
        0x04 => "EX_Return",
        0x06 => "EX_Jump",
        0x07 => "EX_JumpIfNot",
        0x0B => "EX_Nothing",
        0x0F => "EX_Let",
        0x14 => "EX_LetBool",
        0x16 => "EX_EndFunctionParms",
        0x17 => "EX_Self",
        0x19 => "EX_Context",
        0x1B => "EX_VirtualFunction",
        0x1C => "EX_FinalFunction",
        0x1D => "EX_IntConst",
        0x1E => "EX_FloatConst",
        0x20 => "EX_ObjectConst",
        0x21 => "EX_NameConst",
        0x24 => "EX_ByteConst",
        0x25 => "EX_IntZero",
        0x26 => "EX_IntOne",
        0x27 => "EX_True",
        0x28 => "EX_False",
        0x2A => "EX_NoObject",
        0x2F => "EX_StructConst",
        0x30 => "EX_EndStructConst",
        0x38 => "EX_LocalOutVariable",
        0x44 => "EX_CallMath",
        0x45 => "EX_LocalVirtualFunction",
        0x46 => "EX_LocalFinalFunction",
        0x4F => "EX_SetSparseDelegate",
        0x5A => "EX_WireTracepoint",
        0x5E => "EX_Tracepoint",
        _ => "",
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i % 16 == 0 {
            s.push_str(&format!("\n  {i:04x}  "));
        }
        s.push_str(&format!("{b:02x} "));
    }
    s
}

fn main() -> anyhow::Result<()> {
    let pkg = norm(&std::env::args().nth(1).expect("usage: <pkg-substr> <fn-substr>"));
    let func = std::env::args().nth(2).expect("usage: <pkg-substr> <fn-substr>").to_ascii_lowercase();

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
    let names = h.name_map.copy_raw_names();
    let header_size = h.summary.header_size as usize;

    let mut import_name: BTreeMap<i32, String> = BTreeMap::new();
    for (i, idx) in h.import_map.iter().enumerate() {
        if let Some(r) = idx.package_import()
            && let Some(p) = h.imported_package_names.get(r.imported_package_index as usize)
        {
            import_name.insert(-(i as i32) - 1, p.clone());
        }
    }
    let export_name: BTreeMap<i32, String> = h
        .export_map
        .iter()
        .enumerate()
        .map(|(i, ex)| (i as i32 + 1, h.name_map.get(ex.object_name).to_string()))
        .collect();

    for (i, ex) in h.export_map.iter().enumerate() {
        let fname = h.name_map.get(ex.object_name).to_string();
        if !fname.to_ascii_lowercase().contains(&func) {
            continue;
        }
        let s = header_size + ex.cooked_serial_offset as usize;
        let e = s + ex.cooked_serial_size as usize;
        let Some(payload) = bytes.get(s..e) else { continue };
        println!("=== export[{i}] {fname}");
        println!("    raw serial size: {} bytes", payload.len());

        let script = read_ufunction_script(payload, &names).unwrap_or_default();
        println!("    parsed script  : {} bytes", script.len());
        println!("    raw payload hex:{}", hex(payload));

        if !script.is_empty() {
            println!("\n    script hex:{}", hex(&script));
            println!("\n    annotated:");
            let mut o = 0usize;
            while o < script.len() {
                let b = script[o];
                let n = opname(b);
                let mut line = format!("      +{o:04} {b:02x}  {n}");
                // Show the operand for the constants that carry one inline.
                if b == 0x1D && o + 5 <= script.len() {
                    let v = i32::from_le_bytes(script[o + 1..o + 5].try_into().unwrap());
                    line.push_str(&format!("  = {v}"));
                } else if (b == 0x20 || b == 0x1C || b == 0x46) && o + 5 <= script.len() {
                    let v = i32::from_le_bytes(script[o + 1..o + 5].try_into().unwrap());
                    let who = import_name
                        .get(&v)
                        .cloned()
                        .or_else(|| export_name.get(&v).cloned())
                        .unwrap_or_else(|| format!("<{v}>"));
                    line.push_str(&format!("  -> {who}"));
                }
                if !n.is_empty() {
                    println!("{line}");
                }
                o += 1;
            }
        }
    }
    Ok(())
}
