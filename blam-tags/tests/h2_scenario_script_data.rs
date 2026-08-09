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

/// How common the audited-loss refusal actually is, and where.
#[test]
#[ignore = "diagnostic; needs H3EK and HREK"]
fn report_how_many_h3_lights_are_refused_for_audited_loss() {
    let (Some(h3), Some(reach)) = (kit("H3EK"), kit("HREK")) else {
        eprintln!("skipping: needs H3EK and HREK");
        return;
    };
    let definitions = definitions();
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&reach, &groups);
    for (extension, fourcc) in [("light", *b"ligh"), ("effect", *b"effe")] {
        let group_tag = u32::from_be_bytes(fourcc);
        let paths = find(&h3, extension, 400);
        let (mut ok, mut refused, mut other) = (0usize, 0usize, 0usize);
        let mut first: Option<(usize, String)> = None;
        for (index, path) in paths.iter().enumerate() {
            let Ok(tag) = blam_tags::convert::read_tag_for_conversion(
                path,
                Some("halo3_mcc"),
                Some(&definitions),
                group_tag,
            ) else {
                continue;
            };
            match blam_tags::convert::analyze_conversion_with_templates(
                &tag,
                "halo3_mcc",
                "haloreach_mcc",
                &definitions,
                Some(&templates),
            ) {
                Ok(_) => ok += 1,
                Err(error) if error.contains("was not written") => {
                    refused += 1;
                    if first.is_none() {
                        first = Some((index, path.display().to_string()));
                    }
                }
                Err(_) => other += 1,
            }
        }
        eprintln!(
            ".{extension:<8} {} scanned: {ok} ok, {refused} refused for audited loss, {other} other; first refusal {first:?}",
            paths.len()
        );
    }
}

/// Where Halo 4's first usable particle template sits in the scanned order.
///
/// `find_native_target_template` walks a group's tags sorted by path and takes
/// the first whose header carries a source revision, stopping after
/// `NATIVE_TEMPLATE_SCAN_LIMIT`. If Halo 4's particles are sparse in that order
/// the cap is the difference between a native layout and a generated one.
#[test]
#[ignore = "diagnostic; needs H4EK"]
fn report_where_halo_4_particle_templates_sit() {
    let Some(h4) = kit("H4EK") else {
        eprintln!("skipping: needs H4EK");
        return;
    };
    for extension in ["particle", "effect", "material"] {
        let paths = find(&h4, extension, usize::MAX);
        let mut accepted_at = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let Ok((header, endian)) = blam_tags::TagFileHeader::peek(path) else {
                continue;
            };
            if endian == blam_tags::Endian::Le && header.version != u32::MAX {
                accepted_at.push(index);
            }
        }
        eprintln!(
            ".{extension:<10} {} shipped, {} acceptable; first at {:?}, within first 256: {}",
            paths.len(),
            accepted_at.len(),
            accepted_at.first(),
            accepted_at.iter().filter(|i| **i < 256).count()
        );
    }
}

/// A converted Reach particle beside a native Halo 4 one.
///
/// Reported as `smoke_fiery_large.particle` converting but not opening in Halo
/// 4's mod tools. Structure first: root size, field list, and the render method,
/// because a tag the tools refuse to open is usually one whose layout they
/// cannot walk.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_a_converted_reach_particle_beside_a_native_halo_4_one() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"prt3");

    let source_path = find(&reach, "particle", usize::MAX)
        .into_iter()
        .find(|p| p.to_string_lossy().contains("fiery_smoke"))
        .or_else(|| find(&reach, "particle", 1).into_iter().next());
    let Some(source_path) = source_path else {
        eprintln!("skipping: no HREK particle");
        return;
    };
    let source = blam_tags::convert::read_tag_for_conversion(
        &source_path,
        Some("haloreach_mcc"),
        Some(&definitions),
        group_tag,
    )
    .unwrap();
    eprintln!("source: {}", source_path.display());

    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let draft = match blam_tags::convert::analyze_conversion_with_templates(
        &source,
        "haloreach_mcc",
        "halo4_mcc",
        &definitions,
        Some(&templates),
    ) {
        Ok(draft) => draft,
        Err(error) => {
            eprintln!("REFUSED: {error}");
            return;
        }
    };
    eprintln!(
        "template used: {:?}",
        draft.native_layout_template.as_ref().map(|p| p.display().to_string())
    );

    let native_path = find(&h4, "particle", 1).into_iter().next().unwrap();
    let native = TagFile::read(&native_path).unwrap();
    eprintln!("native:  {}", native_path.display());

    let describe = |label: &str, tag: &TagFile| {
        eprintln!("--- {label} ---");
        eprintln!("  root size {} bytes", tag.root().definition().size());
        eprintln!(
            "  header build_version={} build_number={} version={:#x} group_version={}",
            tag.header.build_version,
            tag.header.build_number,
            tag.header.version,
            tag.header.group_version
        );
        for field in tag.root().fields() {
            let kind = format!("{:?}", field.field_type());
            if !matches!(
                field.field_type(),
                TagFieldType::Block | TagFieldType::TagReference | TagFieldType::Struct
            ) {
                continue;
            }
            let extra = match field.field_type() {
                TagFieldType::Block => field
                    .as_block()
                    .map(|b| format!(" [{} element(s)]", b.len()))
                    .unwrap_or_default(),
                TagFieldType::TagReference => match field.value() {
                    Some(TagFieldData::TagReference(r)) => match &r.group_tag_and_name {
                        Some((g, n)) => format!(" -> {} {:?}", blam_tags::format_group_tag(*g), n),
                        None => " -> (null)".to_owned(),
                    },
                    _ => String::new(),
                },
                _ => String::new(),
            };
            eprintln!("  {:<38} {:<14}{extra}", field.name(), kind);
        }
    };
    describe("converted", &draft.tag);
    describe("native H4", &native);

    // Does it survive its own write/read cycle?
    match draft.tag.write_to_bytes() {
        Ok(bytes) => match TagFile::read_from_bytes(&bytes) {
            Ok(_) => eprintln!("
roundtrip: OK ({} bytes)", bytes.len()),
            Err(error) => eprintln!("
roundtrip: REOPEN FAILED: {error}"),
        },
        Err(error) => eprintln!("
roundtrip: WRITE FAILED: {error}"),
    }
    for issue in &draft.report.issues {
        eprintln!("  issue {:?} {} - {}", issue.kind, issue.path, issue.message);
    }
}

/// Inside the two template-backed structs of a particle: converted vs native.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_inside_a_converted_halo_4_particle_shader_structs() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"prt3");
    let source_path = find(&reach, "particle", usize::MAX)
        .into_iter()
        .find(|p| p.to_string_lossy().contains("fiery_smoke"))
        .unwrap();
    let source = blam_tags::convert::read_tag_for_conversion(
        &source_path,
        Some("haloreach_mcc"),
        Some(&definitions),
        group_tag,
    )
    .unwrap();
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let draft = blam_tags::convert::analyze_conversion_with_templates(
        &source,
        "haloreach_mcc",
        "halo4_mcc",
        &definitions,
        Some(&templates),
    )
    .unwrap();
    let native_path = draft.native_layout_template.clone().unwrap();
    let native = TagFile::read(&native_path).unwrap();

    let dump = |label: &str, tag: &TagFile, which: &str| {
        let Some(field) = tag
            .root()
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case(which))
        else {
            eprintln!("{label}/{which}: absent");
            return;
        };
        let Some(value) = field.as_struct() else {
            eprintln!("{label}/{which}: not a struct");
            return;
        };
        eprintln!("{label}/{which}: {} bytes", value.definition().size());
        for inner in value.fields() {
            let extra = match inner.value() {
                Some(TagFieldData::TagReference(r)) => match &r.group_tag_and_name {
                    Some((g, n)) if !n.is_empty() => {
                        format!("-> {} {:?}", blam_tags::format_group_tag(*g), n)
                    }
                    Some((g, _)) => format!("-> {} (EMPTY PATH)", blam_tags::format_group_tag(*g)),
                    None => "-> (null)".to_owned(),
                },
                _ => String::new(),
            };
            let blocks = inner
                .as_block()
                .map(|b| format!("[{}]", b.len()))
                .unwrap_or_default();
            eprintln!(
                "    {:<34} {:<14} {blocks} {extra}",
                inner.name(),
                format!("{:?}", inner.field_type())
            );
        }
    };
    for which in ["actual material?", "actual shader?"] {
        dump("CONVERTED", &draft.tag, which);
        dump("NATIVE   ", &native, which);
        eprintln!();
    }
    // And what Reach had to offer in the first place.
    dump("SOURCE(reach)", &source, "actual shader?");
}

/// What Halo 4's own particles put in the two template-backed structs.
///
/// Two candidate causes for the mod tools refusing a converted one: a null
/// `material shader`, and a render method whose `options` count disagrees with
/// what its `rmdf` declares. Both are measured here rather than guessed, because
/// the fix differs completely.
#[test]
#[ignore = "diagnostic; needs H4EK"]
fn report_what_halo_4_particles_put_in_their_material_and_shader() {
    let Some(h4) = kit("H4EK") else {
        eprintln!("skipping: needs H4EK");
        return;
    };
    let mut shaders: BTreeMap<String, usize> = BTreeMap::new();
    let mut option_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut param_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut rmdfs: BTreeMap<String, usize> = BTreeMap::new();
    let mut null_shader = 0usize;
    let mut scanned = 0usize;
    // By rmdf, what option counts occur -- if the rmdf fixes the count, a
    // converted tag carrying a different one is indexing past what it declares.
    let mut by_rmdf: BTreeMap<String, BTreeMap<usize, usize>> = BTreeMap::new();

    for path in find(&h4, "particle", 900) {
        let Ok(tag) = TagFile::read(&path) else { continue };
        scanned += 1;
        let get = |name: &str| tag.root().fields().find(|f| f.name().eq_ignore_ascii_case(name));
        if let Some(value) = get("actual material?").and_then(|f| f.as_struct()) {
            for inner in value.fields() {
                match (inner.name(), inner.value()) {
                    ("material shader", Some(TagFieldData::TagReference(r))) => {
                        match &r.group_tag_and_name {
                            Some((_, n)) if !n.is_empty() => {
                                *shaders.entry(n.clone()).or_default() += 1;
                            }
                            _ => null_shader += 1,
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Some(value) = get("actual shader?").and_then(|f| f.as_struct()) {
            let mut rmdf = String::new();
            let mut options = 0usize;
            let mut params = 0usize;
            for inner in value.fields() {
                if inner.name() == "definition" {
                    if let Some(TagFieldData::TagReference(r)) = inner.value() {
                        if let Some((_, n)) = &r.group_tag_and_name {
                            rmdf = n.clone();
                        }
                    }
                }
                if inner.name() == "options" {
                    options = inner.as_block().map(|b| b.len()).unwrap_or(0);
                }
                if inner.name() == "parameters" {
                    params = inner.as_block().map(|b| b.len()).unwrap_or(0);
                }
            }
            *option_counts.entry(options).or_default() += 1;
            *param_counts.entry(params).or_default() += 1;
            *rmdfs.entry(rmdf.clone()).or_default() += 1;
            *by_rmdf.entry(rmdf).or_default().entry(options).or_default() += 1;
        }
    }
    eprintln!("scanned {scanned} H4EK particles");
    eprintln!("
material shader: {null_shader} null, {} distinct non-null", shaders.len());
    let mut top: Vec<_> = shaders.iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (name, count) in top.iter().take(6) {
        eprintln!("    {count:>5}  {name}");
    }
    eprintln!("
render method rmdf:");
    let mut top: Vec<_> = rmdfs.iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (name, count) in top.iter().take(6) {
        eprintln!("    {count:>5}  {name:?}");
    }
    eprintln!("
options count distribution: {option_counts:?}");
    eprintln!("parameters count distribution: {param_counts:?}");
    eprintln!("
options count per rmdf (does the rmdf fix it?):");
    for (rmdf, counts) in by_rmdf.iter().take(6) {
        eprintln!("    {rmdf:?} -> {counts:?}");
    }
}

/// The option categories `shaders\particle` declares in Reach and in Halo 4.
///
/// A render method's `options` block is one element per category, in order, so
/// the count is fixed by the rmdf and the *meaning* of slot N is whatever
/// category N is. Whether Reach's nine can be copied into Halo 4's ten depends
/// entirely on whether the names line up.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_particle_rmdf_categories_on_both_sides() {
    let mut listing: Vec<(String, Vec<String>)> = Vec::new();
    for (label, kit_name, game) in [
        ("reach", "HREK", "haloreach_mcc"),
        ("halo4", "H4EK", "halo4_mcc"),
    ] {
        let Some(root) = kit(kit_name) else {
            eprintln!("skipping: needs {kit_name}");
            return;
        };
        let path = root.join("shaders/particle.render_method_definition");
        if !path.is_file() {
            eprintln!("{label}: no {}", path.display());
            continue;
        }
        let _ = game;
        let tag = TagFile::read(&path).unwrap();
        let mut names = Vec::new();
        if let Some(block) = tag
            .root()
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case("categories"))
            .and_then(|f| f.as_block())
        {
            for index in 0..block.len() {
                let Some(element) = block.element(index) else { continue };
                let name = element
                    .fields()
                    .find(|f| f.name().to_ascii_lowercase().contains("name"))
                    .and_then(|f| match f.value() {
                        Some(TagFieldData::StringId(s)) => Some(s.string.clone()),
                        Some(TagFieldData::OldStringId(s)) => Some(s.string.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "?".to_owned());
                let options = element
                    .fields()
                    .find(|f| f.name().eq_ignore_ascii_case("options"))
                    .and_then(|f| f.as_block())
                    .map(|b| b.len())
                    .unwrap_or(0);
                names.push(format!("{name} ({options} opt)"));
            }
        }
        eprintln!("{label}: {} categories", names.len());
        for (index, name) in names.iter().enumerate() {
            eprintln!("   [{index}] {name}");
        }
        listing.push((label.to_owned(), names));
    }
    if listing.len() == 2 {
        let (a, b) = (&listing[0].1, &listing[1].1);
        let common = a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count();
        eprintln!("
identical prefix: {common} of {} / {}", a.len(), b.len());
    }
}

/// Where the tenth render-method option goes between Reach and Halo 4.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_where_the_tenth_particle_option_goes() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"prt3");

    let counts = |tag: &TagFile, which: &str, block: &str| -> usize {
        tag.root()
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case(which))
            .and_then(|f| f.as_struct())
            .and_then(|v| {
                v.fields()
                    .find(|f| f.name().eq_ignore_ascii_case(block))
                    .and_then(|f| f.as_block())
                    .map(|b| b.len())
            })
            .unwrap_or(0)
    };
    // What Reach's own particles carry.
    let mut reach_options: BTreeMap<usize, usize> = BTreeMap::new();
    for path in find(&reach, "particle", 400) {
        let Ok(tag) = blam_tags::convert::read_tag_for_conversion(
            &path,
            Some("haloreach_mcc"),
            Some(&definitions),
            group_tag,
        ) else {
            continue;
        };
        *reach_options
            .entry(counts(&tag, "actual shader?", "options"))
            .or_default() += 1;
    }
    eprintln!("HREK particle options counts: {reach_options:?}");

    // The declared max on each side: a block that cannot hold ten is the
    // simplest explanation for arriving with nine.
    for (game, name) in [("haloreach_mcc", "reach"), ("halo4_mcc", "halo4")] {
        let Ok(tag) = TagFile::new(definitions.join(game).join("particle.json")) else {
            eprintln!("{name}: schema will not build");
            continue;
        };
        if let Some(value) = tag
            .root()
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case("actual shader?"))
            .and_then(|f| f.as_struct())
        {
            for inner in value.fields() {
                if let Some(block) = inner.as_block() {
                    eprintln!(
                        "{name} schema: {:<24} max_count {}",
                        inner.name(),
                        block.definition().max_count()
                    );
                }
            }
        }
    }

    // And the specific tag, end to end.
    let source_path = find(&reach, "particle", usize::MAX)
        .into_iter()
        .find(|p| p.to_string_lossy().contains("fiery_smoke"))
        .unwrap();
    let source = blam_tags::convert::read_tag_for_conversion(
        &source_path,
        Some("haloreach_mcc"),
        Some(&definitions),
        group_tag,
    )
    .unwrap();
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let draft = blam_tags::convert::analyze_conversion_with_templates(
        &source,
        "haloreach_mcc",
        "halo4_mcc",
        &definitions,
        Some(&templates),
    )
    .unwrap();
    eprintln!(
        "
smoke_fiery_large: source options={} params={} -> converted options={} params={}",
        counts(&source, "actual shader?", "options"),
        counts(&source, "actual shader?", "parameters"),
        counts(&draft.tag, "actual shader?", "options"),
        counts(&draft.tag, "actual shader?", "parameters"),
    );
    for issue in &draft.report.issues {
        eprintln!("  {:?} {} - {}", issue.kind, issue.path, issue.message);
    }
}

/// What the tenth category's options are, and what H4 particles pick for it.
#[test]
#[ignore = "diagnostic; needs H4EK"]
fn report_the_tenth_particle_option_category() {
    let Some(h4) = kit("H4EK") else {
        eprintln!("skipping: needs H4EK");
        return;
    };
    let rmdf = TagFile::read(h4.join("shaders/particle.render_method_definition")).unwrap();
    let block = rmdf
        .root()
        .fields()
        .find(|f| f.name().eq_ignore_ascii_case("categories"))
        .and_then(|f| f.as_block())
        .unwrap();
    let element = block.element(9).unwrap();
    for inner in element.fields() {
        if inner.name().eq_ignore_ascii_case("options") {
            if let Some(options) = inner.as_block() {
                for index in 0..options.len() {
                    let name = options
                        .element(index)
                        .and_then(|o| {
                            o.fields().find_map(|f| match f.value() {
                                Some(TagFieldData::StringId(s)) => Some(s.string.clone()),
                                Some(TagFieldData::OldStringId(s)) => Some(s.string.clone()),
                                _ => None,
                            })
                        })
                        .unwrap_or_default();
                    eprintln!("  self_illumination option [{index}] = {name:?}");
                }
            }
        }
    }
    // What the shipped tags actually select in each slot.
    let mut slot9: BTreeMap<i64, usize> = BTreeMap::new();
    let mut shape = String::new();
    for path in find(&h4, "particle", 400) {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let Some(options) = tag
            .root()
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case("actual shader?"))
            .and_then(|f| f.as_struct())
            .and_then(|v| {
                v.fields()
                    .find(|f| f.name().eq_ignore_ascii_case("options"))
                    .and_then(|f| f.as_block())
            })
        else {
            continue;
        };
        if shape.is_empty() {
            if let Some(first) = options.element(0) {
                shape = first
                    .fields()
                    .map(|f| format!("{} {:?}", f.name(), f.field_type()))
                    .collect::<Vec<_>>()
                    .join(", ");
            }
        }
        if let Some(element) = options.element(9) {
            if let Some(value) = element.fields().next().and_then(|f| f.value()) {
                let n = match value {
                    TagFieldData::ShortInteger(v) => v as i64,
                    TagFieldData::ShortEnum { value, .. } => value as i64,
                    TagFieldData::ShortBlockIndex(v) => v as i64,
                    other => {
                        eprintln!("  slot 9 holds {other:?}");
                        continue;
                    }
                };
                *slot9.entry(n).or_default() += 1;
            }
        }
    }
    eprintln!("  option element shape: {shape}");
    eprintln!("  slot 9 values across shipped H4 particles: {slot9:?}");
}

/// Every converted Reach fx tag carries the option count Halo 4 requires.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_reach_to_halo_4_fx_option_counts() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let options_of = |tag: &TagFile| -> Option<usize> {
        tag.root()
            .fields()
            .find_map(|f| f.as_struct().filter(|v| {
                v.fields().any(|i| i.name().eq_ignore_ascii_case("options"))
                    && v.fields().any(|i| i.name().eq_ignore_ascii_case("parameters"))
                    && v.fields().any(|i| i.name().eq_ignore_ascii_case("postprocess"))
            }))
            .and_then(|v| {
                v.fields()
                    .find(|i| i.name().eq_ignore_ascii_case("options"))
                    .and_then(|i| i.as_block())
                    .map(|b| b.len())
            })
    };
    for (extension, fourcc) in [("particle", *b"prt3"), ("beam_system", *b"beam"), ("decal_system", *b"decs"), ("light_volume_system", *b"ligh")] {
        let group_tag = u32::from_be_bytes(fourcc);
        let mut short_source = 0usize;
        let mut short_output = 0usize;
        let mut converted = 0usize;
        let mut refused = 0usize;
        let mut null_material = 0usize;
        for path in find(&reach, extension, 250) {
            let Ok(source) = blam_tags::convert::read_tag_for_conversion(
                &path, Some("haloreach_mcc"), Some(&definitions), group_tag) else { continue };
            let src_options = options_of(&source);
            match blam_tags::convert::analyze_conversion_with_templates(
                &source, "haloreach_mcc", "halo4_mcc", &definitions, Some(&templates)) {
                Ok(draft) => {
                    converted += 1;
                    let out = options_of(&draft.tag);
                    if src_options.is_some_and(|n| n < 10) { short_source += 1; }
                    if out.is_some_and(|n| n < 10) { short_output += 1; }
                    let material_null = draft.tag.root().fields()
                        .find(|f| f.name().eq_ignore_ascii_case("actual material?"))
                        .and_then(|f| f.as_struct())
                        .map(|v| v.fields().any(|i| {
                            i.name().eq_ignore_ascii_case("material shader")
                                && matches!(i.value(), Some(TagFieldData::TagReference(r))
                                    if r.group_tag_and_name.as_ref().is_none_or(|(_, n)| n.is_empty()))
                        }))
                        .unwrap_or(false);
                    if material_null { null_material += 1; }
                }
                Err(_) => refused += 1,
            }
        }
        eprintln!(
            ".{extension:<22} {converted} converted, {refused} refused; sources short of 10:              {short_source}, OUTPUTS STILL SHORT: {short_output}; null material shader: {null_material}"
        );
    }
}

/// The seven shipped Halo 4 particles with no material shader: what else do they
/// have? If their render method is populated, a converted particle matches a
/// configuration the game ships, and the null material is a look to reconnect
/// rather than a broken tag.
#[test]
#[ignore = "diagnostic; needs H4EK"]
fn report_shipped_halo_4_particles_with_no_material_shader() {
    let Some(h4) = kit("H4EK") else {
        eprintln!("skipping: needs H4EK");
        return;
    };
    let mut found = 0usize;
    for path in find(&h4, "particle", 900) {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let material = tag.root().fields()
            .find(|f| f.name().eq_ignore_ascii_case("actual material?"))
            .and_then(|f| f.as_struct());
        let null_shader = material.map(|v| v.fields().any(|i| {
            i.name().eq_ignore_ascii_case("material shader")
                && matches!(i.value(), Some(TagFieldData::TagReference(r))
                    if r.group_tag_and_name.as_ref().is_none_or(|(_, n)| n.is_empty()))
        })).unwrap_or(false);
        if !null_shader { continue; }
        found += 1;
        let shader = tag.root().fields()
            .find(|f| f.name().eq_ignore_ascii_case("actual shader?"))
            .and_then(|f| f.as_struct());
        let (rmdf, options, params) = shader.map(|v| {
            let rmdf = v.fields().find(|i| i.name() == "definition")
                .and_then(|i| match i.value() {
                    Some(TagFieldData::TagReference(r)) =>
                        r.group_tag_and_name.map(|(_, n)| n),
                    _ => None })
                .unwrap_or_default();
            let o = v.fields().find(|i| i.name() == "options").and_then(|i| i.as_block()).map(|b| b.len()).unwrap_or(0);
            let p = v.fields().find(|i| i.name() == "parameters").and_then(|i| i.as_block()).map(|b| b.len()).unwrap_or(0);
            (rmdf, o, p)
        }).unwrap_or_default();
        eprintln!(
            "  {}
      render method: rmdf={rmdf:?} options={options} parameters={params}",
            path.strip_prefix(&h4).unwrap_or(&path).display()
        );
    }
    eprintln!("{found} shipped H4 particle(s) ship with no material shader");
}

/// Any *other* block whose length Halo 4 fixes, that a converted tag gets wrong.
///
/// The options bug was one instance of a class: a block whose length is dictated
/// by the destination rather than by the source. This finds every block that is
/// constant across the shipped corpus and checks converted output against it, so
/// a second instance shows up as data rather than as another bug report.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_blocks_halo_4_fixes_that_conversion_gets_wrong() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"prt3");

    fn block_lengths(tag: &TagFile) -> BTreeMap<String, usize> {
        fn walk(value: TagStruct<'_>, prefix: &str, out: &mut BTreeMap<String, usize>) {
            for field in value.fields() {
                let path = if prefix.is_empty() {
                    field.name().to_owned()
                } else {
                    format!("{prefix}/{}", field.name())
                };
                if let Some(block) = field.as_block() {
                    out.insert(path.clone(), block.len());
                }
                if let Some(nested) = field.as_struct() {
                    walk(nested, &path, out);
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(tag.root(), "", &mut out);
        out
    }

    // Which top-level block lengths never vary in the shipped corpus.
    let mut seen: BTreeMap<String, std::collections::BTreeSet<usize>> = BTreeMap::new();
    let mut shipped = 0usize;
    for path in find(&h4, "particle", 600) {
        let Ok(tag) = TagFile::read(&path) else { continue };
        shipped += 1;
        for (name, len) in block_lengths(&tag) {
            seen.entry(name).or_default().insert(len);
        }
    }
    let fixed: BTreeMap<String, usize> = seen
        .iter()
        .filter(|(_, lens)| lens.len() == 1)
        .map(|(name, lens)| (name.clone(), *lens.iter().next().unwrap()))
        .collect();
    eprintln!("across {shipped} shipped H4 particles, {} block(s) never vary:", fixed.len());
    for (name, len) in &fixed {
        eprintln!("    {name} = {len}");
    }

    // Now: do converted particles honour every one of them?
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let mut violations: BTreeMap<String, usize> = BTreeMap::new();
    let mut checked = 0usize;
    for path in find(&reach, "particle", 250) {
        let Ok(source) = blam_tags::convert::read_tag_for_conversion(
            &path, Some("haloreach_mcc"), Some(&definitions), group_tag) else { continue };
        let Ok(draft) = blam_tags::convert::analyze_conversion_with_templates(
            &source, "haloreach_mcc", "halo4_mcc", &definitions, Some(&templates)) else { continue };
        checked += 1;
        for (name, len) in block_lengths(&draft.tag) {
            if let Some(&wanted) = fixed.get(&name) {
                if len != wanted {
                    *violations.entry(format!("{name} (want {wanted}, got {len})")).or_default() += 1;
                }
            }
        }
    }
    eprintln!("
{checked} converted particles checked against those invariants");
    if violations.is_empty() {
        eprintln!("    no violations");
    } else {
        for (what, count) in &violations {
            eprintln!("    {count:>4} x {what}");
        }
    }
}

/// Is Halo 4's `tracer_system` the same tag as Reach's `contrail_system`?
#[test]
#[ignore = "diagnostic; needs the definitions"]
fn report_contrail_against_tracer() {
    let definitions = definitions();
    let describe = |game: &str, group: &str| -> Option<(usize, Vec<String>)> {
        let tag = TagFile::new(definitions.join(game).join(format!("{group}.json"))).ok()?;
        let fields = tag
            .root()
            .fields()
            .map(|f| format!("{} :{:?}", f.name(), f.field_type()))
            .collect::<Vec<_>>();
        Some((tag.root().definition().size(), fields))
    };
    let mut sets: Vec<(String, usize, Vec<String>)> = Vec::new();
    // The dumped Reach/ODST contrail schema panics on build (the `unusable_schemas`
    // case), so schemas are probed behind a catch and real kit tags carry the rest:
    // a shipped tag has its own layout and needs no dump at all.
    for (game, group) in [
        ("halo3_mcc", "contrail_system"),
        ("haloreach_mcc", "contrail_system"),
        ("haloreach_mcc", "beam_system"),
        ("halo4_mcc", "tracer_system"),
    ] {
        let probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| describe(game, group)));
        match probe {
            Ok(Some((size, fields))) => {
                eprintln!("{game}/{group} SCHEMA: {size} bytes, {} fields", fields.len());
                sets.push((format!("{game}/{group}"), size, fields));
            }
            Ok(None) => eprintln!("{game}/{group} SCHEMA: will not build"),
            Err(_) => eprintln!("{game}/{group} SCHEMA: PANICS on build"),
        }
    }
    // Now the same, from tags the kits actually ship.
    sets.clear();
    for (kit_name, group, extension) in [
        ("HREK", "contrail_system", "contrail_system"),
        ("HREK", "beam_system", "beam_system"),
        ("H4EK", "tracer_system", "tracer_system"),
    ] {
        let Some(root) = kit(kit_name) else { continue };
        let Some(path) = find(&root, extension, 1).into_iter().next() else {
            eprintln!("{kit_name}/{group}: none shipped");
            continue;
        };
        let Ok(tag) = TagFile::read(&path) else {
            eprintln!("{kit_name}/{group}: unreadable");
            continue;
        };
        let fields: Vec<String> = tag
            .root()
            .fields()
            .map(|f| format!("{} :{:?}", f.name(), f.field_type()))
            .collect();
        eprintln!(
            "{kit_name}/{group} SHIPPED: {} bytes, {} fields",
            tag.root().definition().size(),
            fields.len()
        );
        sets.push((format!("{kit_name}/{group}"), 0, fields));
    }
    // Overlap of every source against the Halo 4 target.
    if let Some((target_label, _, target)) = sets.last().cloned() {
        let target_names: std::collections::BTreeSet<&String> = target.iter().collect();
        for (label, _, fields) in &sets[..sets.len().saturating_sub(1)] {
            let names: std::collections::BTreeSet<&String> = fields.iter().collect();
            let shared: Vec<_> = names.intersection(&target_names).collect();
            eprintln!(
                "
{label} vs {target_label}: {} of {} source fields match exactly",
                shared.len(),
                fields.len()
            );
            let missing: Vec<&&String> = names.difference(&target_names).collect();
            eprintln!("  source-only ({}): {:?}", missing.len(), &missing[..missing.len().min(12)]);
            let extra: Vec<&&String> = target_names.difference(&names).collect();
            eprintln!("  target-only ({}): {:?}", extra.len(), &extra[..extra.len().min(12)]);
        }
    }
}

/// A Reach contrail entry beside a Halo 4 tracer entry.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_contrail_entry_against_tracer_entry() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let element_fields = |path: &Path, block_name: &str| -> Option<Vec<String>> {
        let tag = TagFile::read(path).ok()?;
        let block = tag
            .root()
            .fields()
            .find(|f| f.name().eq_ignore_ascii_case(block_name))?
            .as_block()?;
        let element = block.element(0)?;
        Some(
            element
                .fields()
                .map(|f| format!("{}:{:?}", f.name(), f.field_type()))
                .collect(),
        )
    };
    // Take the first shipped tag whose block actually has an element.
    let pick = |root: &Path, extension: &str, block_name: &str| -> Option<(PathBuf, Vec<String>)> {
        find(root, extension, 60)
            .into_iter()
            .find_map(|path| element_fields(&path, block_name).map(|f| (path, f)))
    };
    let Some((contrail_path, contrail)) = pick(&reach, "contrail_system", "contrails") else {
        eprintln!("no HREK contrail with entries");
        return;
    };
    let Some((tracer_path, tracer)) = pick(&h4, "tracer_system", "tracers") else {
        eprintln!("no H4EK tracer with entries");
        return;
    };
    eprintln!("contrail entry ({}): {} fields", contrail_path.file_name().unwrap().to_string_lossy(), contrail.len());
    eprintln!("tracer entry   ({}): {} fields", tracer_path.file_name().unwrap().to_string_lossy(), tracer.len());
    let a: std::collections::BTreeSet<&String> = contrail.iter().collect();
    let b: std::collections::BTreeSet<&String> = tracer.iter().collect();
    let shared: Vec<_> = a.intersection(&b).collect();
    eprintln!("
shared, name and type ({} of {} / {}):", shared.len(), contrail.len(), tracer.len());
    for name in &shared {
        eprintln!("    {name}");
    }
    let only_a: Vec<_> = a.difference(&b).collect();
    eprintln!("
contrail-only ({}):", only_a.len());
    for name in only_a.iter().take(20) {
        eprintln!("    {name}");
    }
    let only_b: Vec<_> = b.difference(&a).collect();
    eprintln!("
tracer-only ({}):", only_b.len());
    for name in only_b.iter().take(20) {
        eprintln!("    {name}");
    }
}

/// Does a Reach contrail_system actually reach Halo 4 as a tracer_system?
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_contrail_to_tracer_conversions() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let group_tag = u32::from_be_bytes(*b"cntl");
    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let (mut ok, mut failed) = (0usize, 0usize);
    let mut first_error = String::new();
    let mut carried = (0usize, 0usize);
    for path in find(&reach, "contrail_system", 60) {
        let Ok(source) = blam_tags::convert::read_tag_for_conversion(
            &path, Some("haloreach_mcc"), Some(&definitions), group_tag) else { continue };
        let src_entries = source.root().fields()
            .find(|f| f.name().eq_ignore_ascii_case("contrails"))
            .and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
        match blam_tags::convert::analyze_conversion_with_templates(
            &source, "haloreach_mcc", "halo4_mcc", &definitions, Some(&templates)) {
            Ok(draft) => {
                ok += 1;
                let out_entries = draft.tag.root().fields()
                    .find(|f| f.name().eq_ignore_ascii_case("tracers"))
                    .and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
                carried.0 += src_entries;
                carried.1 += out_entries;
                if ok == 1 {
                    eprintln!(
                        "first: {} -> {} (.{}), {src_entries} contrail(s) -> {out_entries} tracer(s)",
                        path.file_name().unwrap().to_string_lossy(),
                        draft.target_group_name,
                        draft.target_extension
                    );
                    eprintln!("  exact={} semantic={} defaulted={} unsupported={}",
                        draft.report.copied_exact, draft.report.converted_semantic,
                        draft.report.defaulted_target, draft.report.unsupported_source);
                }
            }
            Err(error) => {
                failed += 1;
                if first_error.is_empty() { first_error = error; }
            }
        }
    }
    eprintln!("
contrail_system -> halo4: {ok} converted, {failed} failed");
    eprintln!("entries: {} in -> {} out", carried.0, carried.1);
    if !first_error.is_empty() { eprintln!("first error: {first_error}"); }
}

/// What Halo 4 tracers draw with, and what a converted one is left holding.
#[test]
#[ignore = "diagnostic; needs HREK and H4EK"]
fn report_tracer_entry_materials() {
    let (Some(reach), Some(h4)) = (kit("HREK"), kit("H4EK")) else {
        eprintln!("skipping: needs HREK and H4EK");
        return;
    };
    let definitions = definitions();
    let shaders_in = |tag: &TagFile| -> Vec<String> {
        let mut out = Vec::new();
        if let Some(block) = tag.root().fields()
            .find(|f| f.name().eq_ignore_ascii_case("tracers"))
            .and_then(|f| f.as_block())
        {
            for index in 0..block.len() {
                let Some(element) = block.element(index) else { continue };
                if let Some(material) = element.fields()
                    .find(|f| f.name().eq_ignore_ascii_case("actual material?"))
                    .and_then(|f| f.as_struct())
                {
                    let name = material.fields()
                        .find(|i| i.name().eq_ignore_ascii_case("material shader"))
                        .and_then(|i| match i.value() {
                            Some(TagFieldData::TagReference(r)) =>
                                r.group_tag_and_name.map(|(_, n)| n),
                            _ => None })
                        .unwrap_or_default();
                    out.push(if name.is_empty() { "(null)".to_owned() } else { name });
                }
            }
        }
        out
    };
    let mut shipped: BTreeMap<String, usize> = BTreeMap::new();
    for path in find(&h4, "tracer_system", 200) {
        let Ok(tag) = TagFile::read(&path) else { continue };
        for name in shaders_in(&tag) { *shipped.entry(name).or_default() += 1; }
    }
    let mut top: Vec<_> = shipped.iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    eprintln!("shipped H4 tracer entry materials:");
    for (name, count) in top.iter().take(8) { eprintln!("    {count:>5}  {name}"); }

    let groups = blam_tags::convert::GameTagIndex::load(&definitions, "halo4_mcc").unwrap();
    let templates = blam_tags::convert::NativeTemplateIndex::build(&h4, &groups);
    let mut converted: BTreeMap<String, usize> = BTreeMap::new();
    for path in find(&reach, "contrail_system", 60) {
        let Ok(source) = blam_tags::convert::read_tag_for_conversion(
            &path, Some("haloreach_mcc"), Some(&definitions), u32::from_be_bytes(*b"cntl")) else { continue };
        let Ok(draft) = blam_tags::convert::analyze_conversion_with_templates(
            &source, "haloreach_mcc", "halo4_mcc", &definitions, Some(&templates)) else { continue };
        for name in shaders_in(&draft.tag) { *converted.entry(name).or_default() += 1; }
    }
    eprintln!("
converted tracer entry materials: {converted:?}");
}
