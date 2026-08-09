//! What a Halo 2 scenario's script payload actually is, and whether Halo 3 can
//! take it.
//!
//! Diagnostic. Run with `--ignored --nocapture` against real kits.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use blam_tags::{TagFieldData, TagFieldType, TagFile, TagStruct};

/// An editing kit's `tags` directory, via `BLAM_TEST_<KIT>` or a Steam library.
///
/// Same convention as the other kit-gated suites: a machine with no kits skips
/// rather than fails, and a machine with kits somewhere unusual can say where.
fn kit(name: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(format!("BLAM_TEST_{name}")) {
        let path = PathBuf::from(path);
        return path.is_dir().then_some(path);
    }
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "C:/Program Files/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join(name).join("tags"))
    .find(|path| path.is_dir())
}

fn definitions() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions")
}

fn find(root: &Path, extension: &str, limit: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = blam_tags::convert::walk_files(root)
        .into_iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(extension))
        .filter(|p| {
            !p.components()
                .any(|c| c.as_os_str().eq_ignore_ascii_case("baboon_converted"))
        })
        .collect();
    out.sort();
    out.truncate(limit);
    out
}

/// Every `data` blob in a tag, by path, with its length.
fn blobs(tag: &TagFile) -> BTreeMap<String, usize> {
    fn walk(value: TagStruct<'_>, prefix: &str, out: &mut BTreeMap<String, usize>) {
        for field in value.fields() {
            let name = field.name().to_owned();
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if let Some(TagFieldData::Data(bytes)) = field.value() {
                out.insert(path.clone(), bytes.len());
            }
            if let Some(nested) = field.as_struct() {
                walk(nested, &path, out);
            }
            if let Some(block) = field.as_block() {
                for index in 0..block.len() {
                    if let Some(element) = block.element(index) {
                        walk(element, &format!("{path}[{index}]"), out);
                    }
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(tag.root(), "", &mut out);
    out
}

/// The data-definition name a `data` field declares, which is the identity the
/// opaque copy path pairs on when struct GUIDs are useless (every classic GUID
/// is all-zero).
fn data_definition_names(tag: &TagFile) -> BTreeMap<String, String> {
    fn walk(tag: &TagFile, value: TagStruct<'_>, prefix: &str, out: &mut BTreeMap<String, String>) {
        for field in value.fields() {
            let name = field.name().to_owned();
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if field.field_type() == TagFieldType::Data {
                let declared = field
                    .data_definition_name()
                    .unwrap_or("(unnamed)")
                    .to_owned();
                out.insert(path.clone(), declared);
            }
            if let Some(nested) = field.as_struct() {
                walk(tag, nested, &path, out);
            }
            if let Some(block) = field.as_block() {
                for index in 0..block.len() {
                    if let Some(element) = block.element(index) {
                        walk(tag, element, &format!("{path}[{index}]"), out);
                    }
                }
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(tag, tag.root(), "", &mut out);
    out
}

#[test]
#[ignore = "diagnostic; needs H2EK and H3EK"]
fn report_what_a_halo_2_scenario_keeps_its_scripts_in() {
    let (Some(h2), Some(h3)) = (kit("H2EK"), kit("H3EK")) else {
        eprintln!("skipping: needs H2EK and H3EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"scnr");

    eprintln!("=== what each side declares ===");
    for (game, path) in [
        ("halo2_mcc", definitions.join("halo2_mcc/scenario.json")),
        ("halo3_mcc", definitions.join("halo3_mcc/scenario.json")),
    ] {
        match TagFile::new(&path) {
            Ok(tag) => {
                let names = data_definition_names(&tag);
                let script: Vec<_> = names
                    .iter()
                    .filter(|(path, _)| path.to_ascii_lowercase().contains("script"))
                    .collect();
                eprintln!("{game}: {} data field(s) total", names.len());
                for (path, declared) in script {
                    eprintln!("    {path}  ->  {declared}");
                }
            }
            Err(error) => eprintln!("{game}: schema will not build: {error}"),
        }
    }

    eprintln!("\n=== what H2EK actually ships ===");
    for path in find(&h2, "scenario", 6) {
        let Ok(tag) =
            blam_tags::convert::read_tag_for_conversion(&path, Some("halo2_mcc"), Some(&definitions), group_tag)
        else {
            eprintln!("{}: unreadable", path.display());
            continue;
        };
        let filled: Vec<_> = blobs(&tag)
            .into_iter()
            .filter(|(_, len)| *len > 0)
            .collect();
        // Does it carry the *source* text as well as the compiled form? That is
        // the difference between "recompile this by hand from nothing" and
        // "recompile this from the source that came with it".
        let sources = tag
            .root()
            .fields()
            .find(|field| field.name().to_ascii_lowercase().starts_with("source files"))
            .and_then(|field| field.as_block())
            .map(|block| block.len())
            .unwrap_or(0);
        eprintln!(
            "{}\n    non-empty blobs: {:?}\n    source files block: {sources} element(s)",
            path.display(),
            filled
        );
    }

    eprintln!("\n=== what H3EK ships, for comparison ===");
    for path in find(&h3, "scenario", 3) {
        let Ok(tag) = TagFile::read(&path) else {
            continue;
        };
        let filled: Vec<_> = blobs(&tag)
            .into_iter()
            .filter(|(_, len)| *len > 0)
            .collect();
        let sources = tag
            .root()
            .fields()
            .find(|field| field.name().to_ascii_lowercase().starts_with("source files"))
            .and_then(|field| field.as_block())
            .map(|block| block.len())
            .unwrap_or(0);
        eprintln!(
            "{}\n    non-empty blobs: {:?}\n    source files block: {sources} element(s)",
            path.display(),
            filled
        );
    }
}

/// Try the conversion the user tried, and say exactly what stopped it.
#[test]
#[ignore = "diagnostic; needs H2EK and H3EK"]
fn report_halo_2_scenario_conversion_attempts() {
    let (Some(h2), Some(h3)) = (kit("H2EK"), kit("H3EK")) else {
        eprintln!("skipping: needs H2EK and H3EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"scnr");
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h3, &groups);

    for path in find(&h2, "scenario", 400) {
        let Ok(source) =
            blam_tags::convert::read_tag_for_conversion(&path, Some("halo2_mcc"), Some(&definitions), group_tag)
        else {
            eprintln!("{}: unreadable", path.display());
            continue;
        };
        let filled: Vec<_> = blobs(&source)
            .into_iter()
            .filter(|(_, len)| *len > 0)
            .map(|(path, len)| format!("{path}={len}"))
            .collect();
        match blam_tags::convert::analyze_conversion_with_templates(
            &source,
            "halo2_mcc",
            "halo3_mcc",
            &definitions,
            Some(&templates),
        ) {
            Ok(draft) => eprintln!(
                "OK   {}  [{}]  -> exact {} semantic {}",
                path.display(),
                filled.join(", "),
                draft.report.copied_exact,
                draft.report.converted_semantic
            ),
            Err(error) => eprintln!(
                "FAIL {}  [{}]\n       {error}",
                path.display(),
                filled.join(", ")
            ),
        }
    }
}

/// Every script-related field on each side, whatever its type.
///
/// The question a `payload_aliases` rule turns on: is the Halo 2 string table
/// still *indexed* by anything once it lands in Halo 3? A string table nothing
/// points at is not data carried, it is ballast.
#[test]
#[ignore = "diagnostic; needs the definitions"]
fn report_every_script_field_on_both_sides() {
    let definitions = definitions();
    for game in ["haloce_mcc", "halo2_mcc", "halo3_mcc", "halo3odst_mcc", "haloreach_mcc"] {
        let path = definitions.join(game).join("scenario.json");
        let Ok(tag) = TagFile::new(&path) else {
            eprintln!("{game}: schema will not build");
            continue;
        };
        eprintln!("--- {game} ---");
        for field in tag.root().fields() {
            let name = field.name().to_ascii_lowercase();
            if !(name.contains("script") || name.contains("hs ") || name.contains("source file")) {
                continue;
            }
            let kind = format!("{:?}", field.field_type());
            let declared = field.data_definition_name().unwrap_or("-");
            eprintln!("    {:<28} {:<12} {}", field.name(), kind, declared);
        }
    }
}

/// Does any shipped Halo 2 scenario carry compiled script *syntax*?
///
/// If the syntax table is always empty then the string table indexes nothing
/// even in Halo 2, and the question of carrying it answers itself.
#[test]
#[ignore = "diagnostic; needs H2EK"]
fn report_whether_halo_2_scenarios_carry_compiled_syntax() {
    let Some(h2) = kit("H2EK") else {
        eprintln!("skipping: needs H2EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"scnr");
    let mut with_strings = 0usize;
    let mut with_syntax = 0usize;
    let mut with_source = 0usize;
    let mut total = 0usize;
    let mut biggest: (usize, String) = (0, String::new());
    for path in find(&h2, "scenario", 400) {
        let Ok(tag) = blam_tags::convert::read_tag_for_conversion(
            &path,
            Some("halo2_mcc"),
            Some(&definitions),
            group_tag,
        ) else {
            continue;
        };
        total += 1;
        let all = blobs(&tag);
        let strings = all.get("script string data").copied().unwrap_or(0);
        let syntax = all.get("script syntax data").copied().unwrap_or(0);
        let source: usize = all
            .iter()
            .filter(|(path, _)| path.starts_with("source files"))
            .map(|(_, len)| *len)
            .sum();
        if strings > 0 {
            with_strings += 1;
        }
        if syntax > 0 {
            with_syntax += 1;
        }
        if source > 0 {
            with_source += 1;
        }
        if strings > biggest.0 {
            biggest = (strings, path.display().to_string());
        }
    }
    eprintln!(
        "H2EK scenarios: {total} read; {with_strings} carry script string data,          {with_syntax} carry compiled script syntax data, {with_source} carry .hsc source text"
    );
    eprintln!("largest string table: {} bytes in {}", biggest.0, biggest.1);
}

/// Does the compiled script table itself cross, and should it?
///
/// The string table is indexed by `hs syntax datums`. If those carry, refusing
/// the strings leaves datums pointing into nothing; if they do not, carrying the
/// strings leaves a table nothing reads. Either way the two have to agree, so
/// this measures what actually happens to the block before anything is decided
/// about the blob.
#[test]
#[ignore = "diagnostic; needs H2EK and H3EK"]
fn report_whether_compiled_script_datums_cross() {
    let (Some(h2), Some(h3)) = (kit("H2EK"), kit("H3EK")) else {
        eprintln!("skipping: needs H2EK and H3EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"scnr");

    let block_len = |tag: &TagFile, name: &str| -> usize {
        tag.root()
            .fields()
            .find(|field| field.name().eq_ignore_ascii_case(name))
            .and_then(|field| field.as_block())
            .map(|block| block.len())
            .unwrap_or(0)
    };
    // Element size and field count on each side: the block can only carry
    // meaningfully if the two describe the same datum.
    for game in ["halo2_mcc", "halo3_mcc"] {
        let Ok(tag) = TagFile::new(definitions.join(game).join("scenario.json")) else {
            continue;
        };
        for name in ["hs syntax datums", "scripts", "globals"] {
            if let Some(field) = tag
                .root()
                .fields()
                .find(|field| field.name().eq_ignore_ascii_case(name))
            {
                if let Some(block) = field.as_block() {
                    let element_fields = block
                        .element(0)
                        .map(|e| e.fields().count())
                        .unwrap_or(0);
                    eprintln!(
                        "{game:<12} {name:<18} {} element(s) present, {element_fields} field(s) per element",
                        block.len(),
                    );
                }
            }
        }
    }

    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h3, &groups);
    eprintln!("
 source datums / strings  ->  converted datums / strings");
    for path in find(&h2, "scenario", 12) {
        let Ok(source) = blam_tags::convert::read_tag_for_conversion(
            &path,
            Some("halo2_mcc"),
            Some(&definitions),
            group_tag,
        ) else {
            continue;
        };
        let datums = block_len(&source, "hs syntax datums");
        let strings = blobs(&source)
            .get("script string data")
            .copied()
            .unwrap_or(0);
        if datums == 0 && strings == 0 {
            continue;
        }
        match blam_tags::convert::analyze_conversion_with_templates(
            &source,
            "halo2_mcc",
            "halo3_mcc",
            &definitions,
            Some(&templates),
        ) {
            Ok(draft) => {
                let out_datums = block_len(&draft.tag, "hs syntax datums");
                let out_strings = blobs(&draft.tag)
                    .get("script string data")
                    .copied()
                    .unwrap_or(0);
                eprintln!(
                    "  {datums:>6} / {strings:>6}  ->  {out_datums:>6} / {out_strings:>6}   {}",
                    path.file_name().unwrap().to_string_lossy()
                );
            }
            Err(_) => eprintln!(
                "  {datums:>6} / {strings:>6}  ->  REFUSED               {}",
                path.file_name().unwrap().to_string_lossy()
            ),
        }
    }
}

/// Every scenario pair along the chain, not just the one that was reported.
#[test]
#[ignore = "diagnostic; needs the kits"]
fn report_scenario_conversions_across_the_chain() {
    let group_tag = u32::from_be_bytes(*b"scnr");
    let definitions = definitions();
    let pairs = [
        ("HCEEK", "haloce_mcc", "H2EK", "halo2_mcc"),
        ("H2EK", "halo2_mcc", "H3EK", "halo3_mcc"),
        ("H3EK", "halo3_mcc", "H3ODSTEK", "halo3odst_mcc"),
        ("H3EK", "halo3_mcc", "HREK", "haloreach_mcc"),
        ("HREK", "haloreach_mcc", "H4EK", "halo4_mcc"),
    ];
    for (source_kit, source_game, target_kit, target_game) in pairs {
        let (Some(src), Some(dst)) = (kit(source_kit), kit(target_kit)) else {
            eprintln!("{source_game} -> {target_game}: skipping, kit missing");
            continue;
        };
        let groups = blam_tags::convert::GameTagIndex::load(&definitions, target_game).unwrap();
        let templates = blam_tags::convert::NativeTemplateIndex::build(&dst, &groups);
        let (mut ok, mut failed) = (0usize, 0usize);
        let mut first_error = String::new();
        for path in find(&src, "scenario", 25) {
            let Ok(tag) = blam_tags::convert::read_tag_for_conversion(
                &path,
                Some(source_game),
                Some(&definitions),
                group_tag,
            ) else {
                continue;
            };
            match blam_tags::convert::analyze_conversion_with_templates(
                &tag,
                source_game,
                target_game,
                &definitions,
                Some(&templates),
            ) {
                Ok(_) => ok += 1,
                Err(error) => {
                    failed += 1;
                    if first_error.is_empty() {
                        first_error = error;
                    }
                }
            }
        }
        eprintln!("{source_game} -> {target_game}: {ok} ok, {failed} failed  {first_error}");
    }
}

/// How many non-finite numbers Halo 2's kit still holds, after the legacy-string
/// offset fix.
///
/// The NaN the converter guards against was found at
/// `material responses[0]/angular noise` in a Halo 2 projectile. If the field was
/// simply being read from the wrong offset, that NaN was never in the tag and the
/// guard has nothing left to catch here.
#[test]
#[ignore = "diagnostic; needs H2EK"]
fn report_non_finite_numbers_left_in_halo_2() {
    let Some(h2) = kit("H2EK") else {
        eprintln!("skipping: needs H2EK");
        return;
    };
    let definitions = definitions();
    for extension in ["projectile", "weapon", "biped", "vehicle", "crate", "effect"] {
        let group_tag = match extension {
            "projectile" => *b"proj",
            "weapon" => *b"weap",
            "biped" => *b"bipd",
            "vehicle" => *b"vehi",
            "crate" => *b"bloc",
            _ => *b"effe",
        };
        let group_tag = u32::from_be_bytes(group_tag);
        let mut scanned = 0usize;
        let mut with_bad = 0usize;
        let mut examples: Vec<String> = Vec::new();
        for path in find(&h2, extension, 120) {
            let Ok(tag) = blam_tags::convert::read_tag_for_conversion(
                &path,
                Some("halo2_mcc"),
                Some(&definitions),
                group_tag,
            ) else {
                continue;
            };
            scanned += 1;
            let mut bad = Vec::new();
            walk_reals(tag.root(), "", &mut bad);
            if !bad.is_empty() {
                with_bad += 1;
                if examples.len() < 3 {
                    examples.push(format!("{}: {:?}", path.file_name().unwrap().to_string_lossy(), &bad[..bad.len().min(2)]));
                }
            }
        }
        eprintln!(".{extension:<12} {scanned:>4} scanned, {with_bad} with non-finite values  {examples:?}");
    }
}

/// Every non-finite real under `value`, by path.
fn walk_reals(value: TagStruct<'_>, prefix: &str, out: &mut Vec<String>) {
    for field in value.fields() {
        let path = if prefix.is_empty() {
            field.name().to_owned()
        } else {
            format!("{prefix}/{}", field.name())
        };
        if let Some(TagFieldData::Real(v)) = field.value() {
            if !v.is_finite() {
                out.push(path.clone());
            }
        }
        if let Some(nested) = field.as_struct() {
            walk_reals(nested, &path, out);
        }
        if let Some(block) = field.as_block() {
            for index in 0..block.len() {
                if let Some(element) = block.element(index) {
                    walk_reals(element, &format!("{path}[{index}]"), out);
                }
            }
        }
    }
}
