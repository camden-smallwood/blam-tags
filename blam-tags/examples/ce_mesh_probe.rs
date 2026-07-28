//! Probe: locate the native `FReferenceSkeleton` inside a cooked UE5
//! `USkeletalMesh` export by ANCHORING on it (scan for a plausible bone
//! count + a fully-valid run of `FMeshBoneInfo` = FName + parent), then
//! print the bone names. This proves we can reach the render data past the
//! unversioned property block without decoding it, and that the UE bone
//! names match the classic `skeleton_model` nodes.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_mesh_probe -- \
//!     "<paks>" [uasset-suffix=sk_elite_common_body.uasset]

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    b.get(o..o + 4).map(|s| i32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// Try to read an `FReferenceSkeleton` starting at `o`: `i32 numBones`,
/// then `numBones` × (FName + `i32 parent`). `fname_stride` is the FName
/// width (8 = index+number, 4 = bare index). Returns the bone (nameIdx,
/// parent) list if the whole run validates.
fn try_ref_skeleton(
    b: &[u8],
    o: usize,
    names_len: usize,
    fname_stride: usize,
) -> Option<Vec<(usize, i32)>> {
    let n = i32_at(b, o)?;
    if !(8..=512).contains(&n) {
        return None;
    }
    let n = n as usize;
    let entry = fname_stride + 4;
    let mut bones = Vec::with_capacity(n);
    for i in 0..n {
        let e = o + 4 + i * entry;
        let name_idx = i32_at(b, e)?;
        // FName number word (if present) should be small/zero in practice.
        let parent = i32_at(b, e + fname_stride)?;
        if name_idx < 0 || name_idx as usize >= names_len {
            return None;
        }
        if i == 0 {
            if parent != -1 {
                return None; // root bone parent must be INDEX_NONE
            }
        } else if parent < 0 || parent as usize >= n {
            return None;
        }
        bones.push((name_idx as usize, parent));
    }
    Some(bones)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let suffix = args
        .next()
        .unwrap_or_else(|| "sk_elite_common_body.uasset".to_string())
        .to_ascii_lowercase();

    // Mount + find the target uasset.
    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut bytes = None;
    let mut found = String::new();
    'o: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&suffix) {
                found = e.path.clone();
                bytes = a.read(&e.path).ok();
                break 'o;
            }
        }
    }
    let Some(bytes) = bytes else {
        eprintln!("not found: *{suffix}");
        std::process::exit(1);
    };
    println!("uasset: {found} ({} bytes)", bytes.len());

    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        .expect("parse zen header");
    let names = hdr.name_map.copy_raw_names();
    let hstart = hdr.summary.header_size as usize;
    println!(
        "package: {}  names={}  header_size={hstart}  export_body={} bytes",
        hdr.package_name(),
        names.len(),
        bytes.len().saturating_sub(hstart),
    );

    // Scan the export body for the FReferenceSkeleton anchor.
    let body = &bytes[hstart.min(bytes.len())..];
    for stride in [8usize, 4] {
        let mut hits = Vec::new();
        let mut o = 0;
        while o + 8 < body.len() {
            if let Some(bones) = try_ref_skeleton(body, o, names.len(), stride) {
                hits.push((o, bones));
                // skip past this run
                o += 4 + bones_len(&hits) * (stride + 4);
            } else {
                o += 1;
            }
        }
        if hits.is_empty() {
            println!("\n[fname_stride={stride}] no FReferenceSkeleton candidate");
            continue;
        }
        println!("\n[fname_stride={stride}] {} candidate(s):", hits.len());
        for (off, bones) in hits.iter().take(4) {
            println!(
                "  @export+{off} ({} bones) file_off={}",
                bones.len(),
                hstart + off
            );
            let show: Vec<String> = bones
                .iter()
                .take(20)
                .map(|(ni, par)| format!("{}<-{}", names.get(*ni).cloned().unwrap_or_default(), par))
                .collect();
            println!("    {}", show.join(", "));
        }
    }
}

fn bones_len(hits: &[(usize, Vec<(usize, i32)>)]) -> usize {
    hits.last().map(|(_, b)| b.len()).unwrap_or(0)
}
