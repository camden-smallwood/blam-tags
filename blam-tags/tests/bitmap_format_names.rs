//! Every pixel format a game's bitmap schema can name must resolve to a
//! [`BitmapFormat`], or the whole image is unreadable: `BitmapImage::format`
//! fails before any decoder runs, and preview, thumbnails and DDS export all go
//! with it.
//!
//! This reads the option lists straight out of `definitions/<game>/bitmap.json`
//! — the enum each game's `format` field actually points at — so a schema that
//! grows a format (Halo CE MCC's `BC7`) fails here instead of in the editor.

use std::path::PathBuf;

use blam_tags::bitmap::BitmapFormat;
use serde_json::Value;

fn definitions() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions")
}

/// A field's name without its `*`/`#`/`^`/`:` markup (`"format*#DO NOT CHANGE"`
/// is the field `format`).
fn clean_name(name: &str) -> &str {
    name.split(['*', '#', '^', ':', '{', '|']).next().unwrap_or(name).trim()
}

/// The enum definitions the per-image `format` field points at: a `format`
/// enum in the same field list as `width`. The root struct has its own
/// `format` in some games (Halo 2's is the import compression setting,
/// "compressed with explicit alpha"), which is not a pixel format.
fn format_enums(schema: &Value) -> Vec<String> {
    fn field_name(field: &Value) -> Option<&str> {
        field.get("name").and_then(Value::as_str).map(clean_name)
    }
    fn walk(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => map.values().for_each(|v| walk(v, out)),
            Value::Array(fields) => {
                if fields.iter().any(|f| field_name(f) == Some("width")) {
                    for field in fields {
                        let is_enum = field.get("type").and_then(Value::as_str).is_some_and(|t| t.ends_with("_enum"));
                        if !is_enum || field_name(field) != Some("format") {
                            continue;
                        }
                        if let Some(definition) = field.get("definition").and_then(Value::as_str) {
                            if !out.iter().any(|d| d == definition) {
                                out.push(definition.to_owned());
                            }
                        }
                    }
                }
                fields.iter().for_each(|v| walk(v, out));
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(schema, &mut out);
    out
}

/// Names that are allowed not to resolve, each for a stated reason.
fn allowed_unresolved(game: &str, name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    // Reserved slots. `unused2/3/4/7/8/9` have variants because the gen3
    // schemas share one table; the classic schemas number theirs differently.
    if lower.starts_with("unused") {
        return true;
    }
    // Known gap: Halo 2's float formats. No H2EK bitmap uses them (0 of
    // 20,776 images across 4,184 tags), and their channel order is unverified;
    // a guessed layout would decode wrong rather than refuse. Remove from here
    // when they get variants.
    game == "halo2_mcc" && matches!(lower.as_str(), "argbfp32" | "rgbfp32" | "rgbfp16")
}

#[test]
fn every_schema_bitmap_format_resolves() {
    let mut checked_games = 0;
    let mut checked_names = 0;
    let mut unresolved = Vec::new();
    let mut games: Vec<_> = std::fs::read_dir(definitions())
        .expect("definitions directory")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    games.sort();
    for game in games {
        let path = game.join("bitmap.json");
        if !path.is_file() {
            // Named, not silently dropped: a game with no bitmap schema has
            // nothing to check, and that should be visible in the output.
            eprintln!("skip {}: no bitmap.json", game.display());
            continue;
        }
        let schema: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let enums = format_enums(&schema);
        assert!(!enums.is_empty(), "{}: found no `format` enum field", path.display());
        checked_games += 1;
        for definition in enums {
            let options = schema["enums_flags"][&definition]["options"]
                .as_array()
                .unwrap_or_else(|| panic!("{}: enum `{definition}` has no options", path.display()));
            for option in options {
                let name = option.as_str().expect("enum option is a string");
                checked_names += 1;
                let game_name = game.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                if BitmapFormat::from_schema_name(name).is_none() && !allowed_unresolved(game_name, name) {
                    unresolved.push(format!("{}: {definition}: `{name}`", game.display()));
                }
            }
        }
    }
    eprintln!("checked {checked_names} format names across {checked_games} games");
    assert!(checked_games >= 7, "expected every MCC game's bitmap schema, found {checked_games}");
    assert!(unresolved.is_empty(), "unresolved bitmap formats:\n{}", unresolved.join("\n"));
}

#[test]
fn classic_bc7_is_a_16_byte_block_format() {
    let bc7 = BitmapFormat::from_schema_name("BC7").expect("Halo CE MCC spells it `BC7`");
    assert_eq!(bc7, BitmapFormat::Bc7);
    assert!(bc7.is_compressed());
    assert_eq!(bc7.block_dims_and_size(), Some((4, 4, 16)));
    // 5x3 rounds up to 2x1 blocks.
    assert_eq!(bc7.level_bytes(5, 3), 32);
    assert!(bc7.requires_dxt10());
}

/// BC7 has no legacy fourcc, so its DDS has to carry the DX10 header with
/// `DXGI_FORMAT_BC7_UNORM` (98) and the blocks verbatim.
#[test]
fn bc7_dds_is_dx10_bc7_unorm() {
    let blocks = [0xA5u8; 32]; // 8x4: two blocks
    let mut dds = Vec::new();
    blam_tags::bitmap::dds::write_dds_dx10(&mut dds, BitmapFormat::Bc7, 8, 4, 1, 1, &blocks).unwrap();
    let u32_at = |offset: usize| u32::from_le_bytes(dds[offset..offset + 4].try_into().unwrap());
    assert_eq!(&dds[84..88], b"DX10", "pixel-format fourcc");
    assert_eq!(u32_at(20), 32, "linear size is two 16-byte blocks");
    assert_eq!(u32_at(128), 98, "DXGI_FORMAT_BC7_UNORM");
    assert_eq!(&dds[148..], &blocks);
}
