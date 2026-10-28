use std::path::{Path, PathBuf};

use crate::classic::{ClassicHeader, read_classic_tag_file};
use crate::game::GameId;
use crate::layout::TagLayout;
use crate::TagFile;

use super::*;

fn definitions() -> PathBuf {
    Path::new("../definitions").to_path_buf()
}

/// A game's string text syntax, from the definitions.
fn game_syntax(game: &str) -> TextSyntax {
    TextSyntax::load(definitions().join(game)).unwrap_or_else(|error| panic!("{game}: {error}"))
}

fn game_dir(game: GameId) -> &'static str {
    game.as_str()
}

fn new_tag(game: &str, group: &str) -> TagFile {
    let path = definitions().join(game).join(format!("{group}.json"));
    TagFile::new(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// What [`parse_shown`] returns for text refused only at bytes `start..end`.
fn refused(start: usize, end: usize) -> Result<String, Vec<std::ops::Range<usize>>> {
    Err(vec![std::ops::Range { start, end }])
}

fn h3() -> TextSyntax {
    game_syntax("halo3_mcc")
}

fn reach() -> TextSyntax {
    game_syntax(game_dir(GameId::HaloReach))
}

#[test]
fn known_tables_have_their_tools_counts() {
    // Each tool's table, less the `&` entry `&&` uses.
    let counts = [
        (GameId::Halo2, 184),
        (GameId::Halo3, 167),
        (GameId::Halo3Odst, 187),
        (GameId::HaloReach, 157),
    ];
    for (game, count) in counts {
        assert_eq!(game_syntax(game_dir(game)).glyph_names().len(), count, "{game:?}");
    }
    assert!(game_syntax(game_dir(GameId::Halo4)).glyph_names().is_empty());
    assert!(!game_syntax(game_dir(GameId::HaloCe)).has_glyphs());
}

#[test]
fn escapes_and_glyphs_read_as_the_tool_reads_them() {
    let syntax = h3();
    assert_eq!(
        parse_shown(r#"Press &a_button to \"board\"\nthe &&Warthog\\"#, &syntax),
        Ok("Press \u{E100} to \"board\"\nthe &Warthog\\".to_owned())
    );
    assert_eq!(parse_shown("&U+E45B&U+0041", &syntax), Ok("\u{E45B}A".to_owned()));
    // The editor takes the longest name in every game; Halo 3's tool would
    // take the first (see `strings_files_follow_each_tools_rule`).
    assert_eq!(parse_shown("&stick_move_forward", &syntax), Ok("\u{E45B}".to_owned()));
    assert_eq!(parse_shown("&stick_move_forward", &reach()), Ok("\u{E468}".to_owned()));
}

#[test]
fn what_the_tool_refuses_is_marked() {
    let syntax = h3();
    assert_eq!(parse_shown("News & Updates", &syntax), refused(5, 6));
    assert_eq!(parse_shown("&bogus_name here", &syntax), refused(0, 11));
    // `\&` is not an escape: a literal `&` is `&&`.
    assert_eq!(parse_shown(r"a \& b", &syntax), refused(2, 4));
    assert_eq!(parse_shown("&u+E100", &syntax), refused(0, 2));
    assert_eq!(parse_shown("&U+E10", &syntax), refused(0, 2));
    assert_eq!(parse_shown("trailing \\", &syntax), refused(9, 10));
}

#[test]
fn halo_ce_text_has_no_glyphs() {
    let syntax = game_syntax(game_dir(GameId::HaloCe));
    assert_eq!(parse_shown("Films & Clips &a_button", &syntax), Ok("Films & Clips &a_button".to_owned()));
    assert_eq!(format_shown("A & B\r\n", &syntax), "A & B\\r\\n");
}

#[test]
fn every_glyph_shows_as_text_that_reads_back() {
    for game in [GameId::Halo2, GameId::Halo3, GameId::Halo3Odst, GameId::HaloReach, GameId::Halo4] {
        let syntax = game_syntax(game_dir(game));
        let mut values: Vec<u16> = syntax.glyph_names().iter().map(|&(_, value)| value).collect();
        // Glyphs no table names, as the shipped tags hold them.
        values.extend([0xE033, 0xF076, 0x0001]);
        for value in values {
            let glyph = char::from_u32(u32::from(value)).unwrap();
            // Followed by text that could extend a name, and by nothing.
            for after in ["", "_forward", "s", "-up", " and & then \\ \r\n\"x\""] {
                let text = format!("{glyph}{after}");
                let shown = format_shown(&text, &syntax);
                assert_eq!(parse_shown(&shown, &syntax), Ok(text.clone()), "{game:?} {shown:?}");
            }
        }
    }
}

#[test]
fn names_that_read_back_as_others_show_their_code_point() {
    // Every glyph shows by its name, Halo 3's stick_move_forward included.
    assert_eq!(format_shown("\u{E45B}", &h3()), "&stick_move_forward");
    assert_eq!(format_shown("\u{E468}", &reach()), "&stick_move_forward");
    // A name followed by text that makes a longer one shows by code point.
    assert_eq!(format_shown("\u{E453}_forward", &h3()), "&U+E453_forward");
    assert_eq!(format_shown("\u{E460}_forward", &reach()), "&U+E460_forward");
    assert_eq!(format_shown("Press \u{E100}", &h3()), "Press &a_button");
}

#[test]
fn strings_files_follow_each_tools_rule() {
    // Halo 3's tool takes the first name: its file writes stick_move_forward
    // by code point, and reads the name as stick_move and `_forward`.
    assert_eq!(strings_file_line("a", "\u{E45B}", &h3()), "a = \"&U+E45B\"");
    assert_eq!(strings_file_line("a", "\u{E453}_forward", &h3()), "a = \"&stick_move_forward\"");
    let read = |line: &str, syntax: &TextSyntax| parse_strings_file(line, syntax).unwrap()[0].1.clone();
    assert_eq!(read("a = \"&stick_move_forward\"", &h3()), "\u{E453}_forward");
    // Reach's takes the longest, as the editor does.
    assert_eq!(strings_file_line("a", "\u{E468}", &reach()), "a = \"&stick_move_forward\"");
    assert_eq!(read("a = \"&stick_move_forward\"", &reach()), "\u{E468}");
    // Each way round, a line reads back as the text it was written from.
    for text in ["\u{E45B}", "\u{E453}_forward", "x \u{E100}"] {
        assert_eq!(read(&strings_file_line("a", text, &h3()), &h3()), text, "{text:?}");
    }
}

#[test]
fn completion_follows_an_ampersand_that_starts_a_glyph() {
    let syntax = h3();
    let at = |shown: &str| completion_at(shown, shown.len(), &syntax);
    assert_eq!(
        at("Press &a_b"),
        Some(glyph(6..10, "a_b"))
    );
    assert_eq!(at("Press &"), Some(glyph(6..7, "")));
    assert_eq!(at("Press &&"), None);
    assert_eq!(at("Press &&a"), None);
    assert_eq!(at("Press &a_button "), None);
    assert_eq!(at(r"\&a"), None);
    // The run after the cursor is replaced too.
    let shown = "Press &a_bXX now";
    assert_eq!(
        completion_at(shown, 9, &syntax),
        Some(glyph(6..12, "a_"))
    );
    assert_eq!(completion_at("&a", 2, &game_syntax(game_dir(GameId::HaloCe))), None);
}

fn glyph(replace: std::ops::Range<usize>, typed: &str) -> Completion {
    Completion { kind: CompletionKind::Glyph, replace, typed: typed.to_owned() }
}

fn keyword(replace: std::ops::Range<usize>, typed: &str) -> Completion {
    Completion { kind: CompletionKind::Keyword, replace, typed: typed.to_owned() }
}

#[test]
fn the_best_match_starts_with_what_was_typed_else_contains_it() {
    let syntax = h3();
    let names = || syntax.glyph_names().iter().map(|(name, _)| name.as_str());
    let at = |typed: &str| best_match(names(), typed).map(|index| syntax.glyph_names()[index].0.as_str());
    assert_eq!(at("a_"), Some("a_button"));
    assert_eq!(at("button_s"), Some("button_switch_grenade"));
    assert_eq!(at("RIFLE"), Some("assault_rifle"));
    assert_eq!(at(""), Some("a_button"));
    assert_eq!(at("nothing_like_it"), None);
}

fn marked(text: &str, game: GameId) -> Vec<(String, MarkupKind, bool)> {
    scan_markup(text, &game_syntax(game_dir(game)).markup().clone())
        .into_iter()
        .map(|span| (text[span.range].to_owned(), span.kind, span.warning))
        .collect()
}

fn mark(text: &str, kind: MarkupKind, warning: bool) -> (String, MarkupKind, bool) {
    (text.to_owned(), kind, warning)
}

#[test]
fn keywords_are_read_whole_and_anything_else_after_a_hash_is_text() {
    let text = "#cause_player killed #effect_player #cause_players #CAUSE_PLAYER # 5";
    assert_eq!(
        marked(text, GameId::Halo3),
        [mark("#cause_player", MarkupKind::Keyword, false), mark("#effect_player", MarkupKind::Keyword, false)]
    );
    // Each game reads its own list.
    assert_eq!(h3().keywords().len(), 17);
    assert_eq!(game_syntax(game_dir(GameId::Halo2)).keywords().len(), 13);
    assert_eq!(game_syntax(game_dir(GameId::Halo3Odst)).keywords().len(), 18);
    assert_eq!(reach().keywords().len(), 19);
    assert_eq!(game_syntax(game_dir(GameId::Halo4)).keywords(), reach().keywords());
    assert!(game_syntax(game_dir(GameId::HaloCe)).keywords().is_empty());
    assert!(marked("#territory_name", GameId::Halo3).is_empty(), "Halo 2's, not Halo 3's");
}

#[test]
fn halo2_reads_a_keyword_by_its_start() {
    assert_eq!(marked("#territory_name", GameId::Halo2), [mark("#territory_name", MarkupKind::Keyword, false)]);
    // `#cause` and a bare `#` both read as the first keyword they start.
    assert_eq!(
        marked("#cause #1 #territory", GameId::Halo2),
        [
            mark("#cause", MarkupKind::Keyword, true),
            mark("#", MarkupKind::Keyword, true),
            mark("#territory", MarkupKind::Keyword, true),
        ]
    );
    let note = scan_markup("#", &game_syntax(game_dir(GameId::Halo2)).markup().clone())[0].note.clone().unwrap();
    assert!(note.contains("#cause_player"), "{note}");
    assert!(marked("#delta_points", GameId::Halo2).is_empty(), "handled nowhere: shown as written");
}

#[test]
fn pipe_escapes_differ_by_game() {
    let text = "a|nb|rc|kFF00FF00d|je||f|xg|kZZ";
    assert_eq!(
        marked(text, GameId::Halo3),
        [
            mark("|n", MarkupKind::Escape, false),
            mark("|r", MarkupKind::Escape, false),
            mark("|kFF00FF00", MarkupKind::Color, false),
            mark("|j", MarkupKind::Escape, false),
            mark("||", MarkupKind::Escape, false),
            mark("|k", MarkupKind::Color, true),
        ]
    );
    assert_eq!(
        marked(text, GameId::Halo2).into_iter().map(|(text, ..)| text).collect::<Vec<_>>(),
        ["|n", "|r", "||"],
        "no color escapes"
    );
    assert_eq!(marked(text, GameId::HaloCe), [mark("|n", MarkupKind::Escape, false)]);
    assert!(marked(text, GameId::CampaignEvolved).is_empty());
    let note = scan_markup("|r", &game_syntax(game_dir(GameId::HaloReach)).markup().clone())[0].note.clone();
    assert_eq!(note.as_deref(), Some("align right"), "|r aligns; it isn't a carriage return");
}

#[test]
fn tags_are_read_as_the_game_reads_them() {
    let text = "<color argb=#FFa1b375>&a_button OK</color> <BR/> <build_number/> <games-found/> <!--x--> <!-- note -->";
    assert_eq!(
        marked(text, GameId::Halo3),
        [
            mark("<color argb=#FFa1b375>", MarkupKind::Tag, false),
            mark("</color>", MarkupKind::Tag, false),
            mark("<BR/>", MarkupKind::Tag, false),
            mark("<build_number/>", MarkupKind::Placeholder, false),
            mark("<!-- note -->", MarkupKind::Comment, false),
        ]
    );
    assert!(
        marked(text, GameId::Halo2).iter().all(|(_, kind, _)| *kind == MarkupKind::Keyword),
        "Halo 2 has no tags (its # in argb=#FF… reads as a keyword's start)"
    );
    // Screen tags are Halo 3's, and only noted.
    assert_eq!(marked("<lobby-privacy/>", GameId::Halo3), [mark("<lobby-privacy/>", MarkupKind::Placeholder, false)]);
    assert!(marked("<lobby-privacy/>", GameId::HaloReach).is_empty());
    // ODST's screens, from its tag-test build: Firefight's own among them.
    for tag in ["<lobby-privacy/>", "<survival-waves/>", "<metagame-scoring/>", "<loc_time_hm/>"] {
        assert_eq!(marked(tag, GameId::Halo3Odst).len(), 1, "{tag}");
    }
    assert!(marked("<survival-waves/>", GameId::Halo3).is_empty());
    assert!(marked("<loc_time_hm/>", GameId::Halo3).is_empty(), "ODST's date string alone has the time");
    assert_eq!(marked("<uui_month_name/>", GameId::Halo4).len(), 1);
    assert!(marked("<uui_month_name/>", GameId::Halo2Amp).is_empty());
}

#[test]
fn tags_the_game_cant_read_are_warned_about() {
    let warned = |text: &str, game| {
        let spans = scan_markup(text, &game_syntax(game_dir(game)).markup().clone());
        assert_eq!(spans.len(), 1, "{text}");
        spans[0].warning.then(|| spans[0].note.clone().unwrap())
    };
    let literal = warned("<color>", GameId::Halo3).unwrap();
    assert!(literal.contains("as written"), "{literal}");
    let fails = warned("<color>", GameId::HaloReach).unwrap();
    assert!(fails.contains("fails to show the whole string"), "{fails}");
    assert!(warned("<color argb=#a1b375>", GameId::Halo3).unwrap().contains("transparent"));
    assert!(warned("<color argb='#FFa1b375'>", GameId::Halo3).is_some());
    assert_eq!(warned("<color argb=\"0xFFA1B375\">", GameId::Halo3), None);
    assert!(warned("<font face=body_text>", GameId::Halo3).unwrap().contains("quote it"));
    assert_eq!(warned("<font face=\"body_text\">", GameId::Halo3), None);
    assert!(warned("<font face=\"body_text\">", GameId::Halo4).is_some(), "Halo 4's faces differ");
    assert_eq!(warned("<font face=\"arame_thin_14\">", GameId::Halo4), None);
    assert!(warned("<p align=middle>", GameId::Halo3).is_some());
    assert_eq!(warned("<div align=CENTER>", GameId::Halo3), None);
}

#[test]
fn entities_and_bindings() {
    assert_eq!(
        marked("&lt;b&gt; &copy; &#65; &#200; &bogus;", GameId::Halo3),
        [
            mark("&lt;", MarkupKind::Entity, false),
            mark("&gt;", MarkupKind::Entity, false),
            mark("&copy;", MarkupKind::Entity, true),
            mark("&#65;", MarkupKind::Entity, false),
            mark("&#200;", MarkupKind::Entity, true),
        ]
    );
    assert!(marked("&lt;", GameId::Halo2).is_empty());
    let text = "{cui:listing.prop_item_name} {cui:TEMP GAMERTAG} {cui:rank_1.prop_team_index+2}";
    assert_eq!(
        marked(text, GameId::HaloReach),
        [
            mark("{cui:listing.prop_item_name}", MarkupKind::Binding, false),
            mark("{cui:TEMP GAMERTAG}", MarkupKind::Binding, true),
            mark("{cui:rank_1.prop_team_index+2}", MarkupKind::Binding, true),
        ]
    );
    assert_eq!(marked("{cui:rank_1.prop_team_index+2}", GameId::Halo4).len(), 1);
    assert!(!marked("{cui:rank_1.prop_team_index+2}", GameId::Halo4)[0].2, "Halo 4 takes an offset");
    assert!(marked(text, GameId::Halo3).is_empty());
}

#[test]
fn markup_is_placed_on_the_text_as_shown() {
    // A literal `&` is shown `&&`, so `&lt;` is shown `&&lt;`.
    let shown = "Press \\\\|n&&lt; &a_button <br/>";
    let analysis = analyze(shown, &h3());
    let placed: Vec<&str> = analysis.markup.iter().map(|span| &shown[span.range.clone()]).collect();
    assert_eq!(placed, ["|n", "&&lt;", "<br/>"], "\\\\ is a backslash; the | after it starts an escape");
    assert!(!analysis.has_errors());
}

#[test]
fn completion_follows_a_hash_in_a_game_with_keywords() {
    let syntax = h3();
    let at = |shown: &str| completion_at(shown, shown.len(), &syntax);
    assert_eq!(at("Killed by #cause_p"), Some(keyword(10..18, "cause_p")));
    assert_eq!(at("#"), Some(keyword(0..1, "")));
    assert_eq!(at("x #cause_player"), Some(keyword(2..15, "cause_player")));
    assert_eq!(at("# "), None);
    assert_eq!(completion_at("#a", 2, &game_syntax(game_dir(GameId::HaloCe))), None);
}

#[test]
fn completion_follows_a_pipe_or_an_angle_bracket() {
    let syntax = h3();
    let at = |shown: &str| completion_at(shown, shown.len(), &syntax).map(|found| (found.kind, found.typed, found.replace));
    assert_eq!(at("a |"), Some((CompletionKind::Escape, String::new(), 2..3)));
    assert_eq!(at("a |n"), Some((CompletionKind::Escape, "n".to_owned(), 2..4)));
    assert_eq!(at("a ||"), None, "the second | closes the pair");
    assert_eq!(at("a |||"), Some((CompletionKind::Escape, String::new(), 4..5)));
    assert_eq!(at("x <co"), Some((CompletionKind::Tag, "co".to_owned(), 2..5)));
    assert_eq!(at("x </"), Some((CompletionKind::Tag, "/".to_owned(), 2..4)));
    let halo2 = game_syntax(game_dir(GameId::Halo2));
    assert_eq!(completion_at("<", 1, &halo2), None, "Halo 2 reads no tags");
    let ce = game_syntax(game_dir(GameId::HaloCe));
    assert_eq!(completion_at("|", 1, &ce).map(|found| found.kind), Some(CompletionKind::Escape));
}

#[test]
fn each_game_offers_its_own_escapes_and_tags() {
    let count = |game, kind| choices(&game_syntax(game_dir(game)), kind).len();
    assert_eq!(count(GameId::Halo3, CompletionKind::Escape), 8);
    assert_eq!(count(GameId::Halo2, CompletionKind::Escape), 6);
    assert_eq!(count(GameId::HaloCe, CompletionKind::Escape), 1);
    assert_eq!(count(GameId::Halo3, CompletionKind::Tag), 30, "10 formatting, 13 global, 4 saved-file, 3 date");
    assert_eq!(count(GameId::Halo4, CompletionKind::Tag), 32);
    assert_eq!(count(GameId::Halo3Odst, CompletionKind::Tag), 31, "and loc_time_hm");
    assert_eq!(count(GameId::Halo2, CompletionKind::Tag), 0);
    let color = choices(&h3(), CompletionKind::Tag).into_iter().find(|choice| choice.name == "color").unwrap();
    assert_eq!(color.insert, "<color argb=#FFFFFFFF>");
    let glyph = &choices(&h3(), CompletionKind::Glyph)[0];
    assert_eq!((glyph.label.as_str(), glyph.insert.as_str(), glyph.detail.as_str()), ("&a_button", "&a_button", "U+E100"));
}

#[test]
fn strings_file_lines_round_trip() {
    let syntax = h3();
    let text = "Say \"hi\" \u{E100}\r\n& go\\";
    let line = strings_file_line("greeting", text, &syntax);
    assert_eq!(line, r#"greeting = "Say \"hi\" &a_button\r\n&& go\\""#);
    let parsed = parse_strings_file(&format!("\u{feff}[Strings]\n{line}\n\nother = \"x\"\n"), &syntax).unwrap();
    assert_eq!(parsed, vec![("greeting".to_owned(), text.to_owned()), ("other".to_owned(), "x".to_owned())]);
    assert!(parse_strings_file("no equals here", &syntax).is_err());
    assert!(parse_strings_file("id = \"News & Updates\"", &syntax).is_err());
}

#[test]
fn tool_utf8_reads_and_writes_split_surrogates() {
    let mut bytes = Vec::new();
    encode_tool_utf8("a\u{1F600}é", &mut bytes);
    // U+1F600 as two three-byte surrogate sequences, as the tools write it.
    assert_eq!(bytes, [b'a', 0xED, 0xA0, 0xBD, 0xED, 0xB8, 0x80, 0xC3, 0xA9]);
    assert_eq!(decode_tool_utf8(&bytes), "a\u{1F600}é");
    assert_eq!(decode_tool_utf8("a\u{1F600}".as_bytes()), "a\u{1F600}");
}

fn entry(id: &str, english: &str) -> StringEntry {
    StringEntry { id: Some(id.to_owned()), texts: vec![Some(english.to_owned())] }
}

#[test]
fn multilingual_edits_rebuild_the_blob_in_the_tools_order() {
    let mut tag = new_tag("halo3_mcc", "multilingual_unicode_string_list");
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.storage, Storage::Multilingual);
    assert_eq!(list.languages.len(), 12);
    assert_eq!(list.languages[5].name, "Spanish (Latin America)");
    assert!(list.entries.is_empty());

    apply_string_list_edits(
        &mut tag,
        None,
        &[
            StringListEdit::Insert { at: 0, entries: vec![entry("first", "One"), entry("second", "Two")] },
            StringListEdit::SetText { index: 0, language: 1, text: "一".to_owned() },
            StringListEdit::Insert { at: 1, entries: vec![entry("middle", "Mid")] },
        ],
    )
    .unwrap();
    let list = read_string_list(&tag).unwrap();
    let ids: Vec<_> = list.entries.iter().map(|entry| entry.id.clone().unwrap()).collect();
    assert_eq!(ids, ["first", "middle", "second"]);
    assert_eq!(list.entries[0].texts[1].as_deref(), Some("一"));
    assert_eq!(list.entries[1].texts[1], None);

    let root = tag.root();
    let data = root.field_path(TEXT_DATA).unwrap().as_data().unwrap();
    assert_eq!(data, "One\0一\0Mid\0Two\0".as_bytes());
    let block = root.field_path(REFERENCES_BLOCK).unwrap().as_block().unwrap();
    let offsets = |index: usize| -> Vec<i128> {
        let element = block.element(index).unwrap();
        list.languages.iter().map(|language| element.read_int_any(&language.field).unwrap()).collect()
    };
    assert_eq!(offsets(0)[..3], [0, 4, -1]);
    assert_eq!(offsets(1)[..2], [8, -1]);
    assert_eq!(offsets(2)[0], 12);

    // Emptying a text removes it; removing strings closes the gaps.
    apply_string_list_edits(
        &mut tag,
        None,
        &[
            StringListEdit::SetText { index: 0, language: 1, text: String::new() },
            StringListEdit::Remove { indices: vec![1] },
            StringListEdit::SetId { index: 1, id: "renamed".to_owned() },
        ],
    )
    .unwrap();
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.entries, vec![entry_with_languages("first", "One", 12), entry_with_languages("renamed", "Two", 12)]);
    let data = tag.root().field_path(TEXT_DATA).unwrap().as_data().unwrap().to_vec();
    assert_eq!(data, b"One\0Two\0");
}

fn entry_with_languages(id: &str, english: &str, languages: usize) -> StringEntry {
    let mut texts = vec![None; languages];
    texts[0] = Some(english.to_owned());
    StringEntry { id: Some(id.to_owned()), texts }
}

#[test]
fn out_of_range_edits_change_nothing() {
    let mut tag = new_tag("haloreach_mcc", "multilingual_unicode_string_list");
    let error = apply_string_list_edits(&mut tag, None, &[StringListEdit::Remove { indices: vec![0] }]);
    assert!(error.is_err());
    assert!(read_string_list(&tag).unwrap().entries.is_empty());
}

#[test]
fn new_entries_take_unused_ids() {
    let mut tag = new_tag("halo3_mcc", "multilingual_unicode_string_list");
    let list = read_string_list(&tag).unwrap();
    let first = list.new_entry();
    assert_eq!(first.id.as_deref(), Some("new_string"));
    apply_string_list_edits(&mut tag, None, &[StringListEdit::Insert { at: 0, entries: vec![first] }]).unwrap();
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.new_entry().id.as_deref(), Some("new_string_1"));
}

#[test]
fn every_game_with_the_group_reads_and_writes() {
    for (game, languages) in [
        ("halo2_mcc", 9),
        ("halo3_mcc", 12),
        ("halo3odst_mcc", 12),
        ("haloreach_mcc", 12),
        ("halo4_mcc", 17),
        ("halo2amp_mcc", 17),
        ("haloce_evolved", 12),
    ] {
        let mut tag = new_tag(game, "multilingual_unicode_string_list");
        let list = read_string_list(&tag).unwrap_or_else(|| panic!("{game}"));
        assert_eq!(list.languages.len(), languages, "{game}");
        apply_string_list_edits(&mut tag, None, &[StringListEdit::Insert { at: 0, entries: vec![entry("a", "Text")] }])
            .unwrap_or_else(|error| panic!("{game}: {error}"));
        assert_eq!(read_string_list(&tag).unwrap().entries[0].texts[0].as_deref(), Some("Text"), "{game}");
    }
}

#[test]
fn halo_ce_lists_hold_utf16_text_per_element() {
    let mut tag = new_tag("haloce_mcc", "unicode_string_list");
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.storage, Storage::Classic);
    assert_eq!(list.languages.len(), 1);
    let ce_entry = |text: &str| StringEntry { id: None, texts: vec![Some(text.to_owned())] };
    apply_string_list_edits(
        &mut tag,
        None,
        &[StringListEdit::Insert { at: 0, entries: vec![ce_entry("Slayer"), ce_entry("Line one\r\nLine two")] }],
    )
    .unwrap();
    let root = tag.root();
    let block = root.field_path(REFERENCES_BLOCK).unwrap().as_block().unwrap();
    let bytes = block.element(0).unwrap().field_path(CLASSIC_TEXT).unwrap().as_data().unwrap().to_vec();
    assert_eq!(bytes, b"S\0l\0a\0y\0e\0r\0\0\0");
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.entries[1].texts[0].as_deref(), Some("Line one\r\nLine two"));
    assert!(apply_string_list_edits(&mut tag, None, &[StringListEdit::SetId { index: 0, id: "x".to_owned() }]).is_err());
}

/// Read a kit tag, classic or not.
fn read_kit_tag(path: &Path, game: &str) -> TagFile {
    let bytes = std::fs::read(path).unwrap();
    if ClassicHeader::parse(&bytes).is_some() {
        let group = path.extension().unwrap().to_str().unwrap();
        let layout = TagLayout::from_json(definitions().join(game).join(format!("{group}.json"))).unwrap();
        return read_classic_tag_file(&bytes, layout).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
    TagFile::read_from_bytes(&bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn string_lists_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            string_lists_under(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext == "multilingual_unicode_string_list" || ext == "unicode_string_list")
        {
            out.push(path);
        }
    }
}

/// Every string list in a kit (`var` names its root): written back
/// unchanged, it is byte for byte what the tool wrote, and every text shows as
/// text that reads back.
fn check_kit(var: &str, game: GameId) {
    let Some(kit) = std::env::var_os(var).map(PathBuf::from) else {
        eprintln!("skipped: no {var}");
        return;
    };
    let mut paths = Vec::new();
    string_lists_under(&kit.join("tags"), &mut paths);
    assert!(!paths.is_empty(), "no string lists under {}", kit.display());
    let syntax = game_syntax(game_dir(game));
    let mut damaged = Vec::new();
    for path in &paths {
        let tag = read_kit_tag(path, game.as_str());
        let original = tag.write_to_bytes().unwrap();
        let list = read_string_list(&tag).unwrap();
        if list.damaged_offsets > 0 {
            // Written back with its dangling offsets cleared, so not as it was.
            damaged.push(path.strip_prefix(&kit).unwrap().display().to_string().replace('/', "\\"));
            continue;
        }
        let mut rewritten = read_kit_tag(path, game.as_str());
        write_string_list(&mut rewritten, &list).unwrap();
        if game == GameId::HaloReach {
            rebuild_substitution_pairs(&mut rewritten, &list).unwrap();
        }
        assert!(
            rewritten.write_to_bytes().unwrap() == original,
            "{} changed when written back unchanged",
            path.display()
        );
        for text in list.entries.iter().flat_map(|entry| entry.texts.iter().flatten()) {
            let shown = format_shown(text, &syntax);
            assert_eq!(parse_shown(&shown, &syntax).as_ref(), Ok(text), "{}: {shown:?}", path.display());
        }
    }
    eprintln!("{} string lists checked", paths.len());
    if game == GameId::HaloReach {
        // The pair rebuild is checked only where there are pairs.
        let with_pairs = paths
            .iter()
            .filter(|path| read_string_list(&read_kit_tag(path, game.as_str())).is_some_and(|list| !list.pairs.is_empty()))
            .count();
        eprintln!("{with_pairs} with substitution pairs");
        assert!(with_pairs > 0);
    }
    let expected: &[&str] = match game {
        GameId::Halo2 => &["tags\\scenarios\\descriptions\\anchor_point.multilingual_unicode_string_list"],
        _ => &[],
    };
    assert_eq!(damaged, expected);
}

#[test]
fn halo_ce_kit_string_lists_write_back_unchanged() {
    check_kit("BLAM_TEST_HCEEK", GameId::HaloCe);
}

#[test]
fn halo2_kit_string_lists_write_back_unchanged() {
    check_kit("BLAM_TEST_H2EK", GameId::Halo2);
}

#[test]
fn halo3_kit_string_lists_write_back_unchanged() {
    check_kit("BLAM_TEST_H3EK", GameId::Halo3);
}

#[test]
fn odst_kit_string_lists_write_back_unchanged() {
    check_kit("BLAM_TEST_ODSTEK", GameId::Halo3Odst);
}

#[test]
fn reach_kit_string_lists_write_back_unchanged() {
    check_kit("BLAM_TEST_HREK", GameId::HaloReach);
}

#[test]
fn offsets_past_the_text_read_as_no_text_and_write_back_cleared() {
    let mut tag = new_tag("halo3_mcc", "multilingual_unicode_string_list");
    apply_string_list_edits(&mut tag, None, &[StringListEdit::Insert { at: 0, entries: vec![entry("a", "Text")] }]).unwrap();
    {
        let mut root = tag.root_mut();
        let mut field = root.field_path_mut(REFERENCES_BLOCK).unwrap();
        let mut block = field.as_block_mut().unwrap();
        let mut element = block.element_mut(0).unwrap();
        element.field_path_mut("japanese offset").unwrap().set(crate::TagFieldData::LongInteger(500)).unwrap();
    }
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.damaged_offsets, 1);
    assert_eq!(list.entries[0].texts[1], None);
    write_string_list(&mut tag, &list).unwrap();
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.damaged_offsets, 0);
    let root = tag.root();
    let element = root.field_path(REFERENCES_BLOCK).unwrap().as_block().unwrap().element(0).unwrap();
    assert_eq!(element.read_int_any("japanese offset"), Some(-1));
}

#[test]
fn halo_ce_strings_files_round_trip() {
    let texts = ["Slayer", "Line one\r\nLine two", " "];
    let file = ce_strings_file(texts);
    assert_eq!(file, "Slayer\r\n###END-STRING###\r\nLine one\r\nLine two\r\n###END-STRING###\r\n \r\n###END-STRING###\r\n");
    assert_eq!(parse_ce_strings_file(&format!("\u{feff}{file}")), texts);
    assert_eq!(parse_ce_strings_file("one\n###END-STRING###\n\ntwo"), ["one", "two"]);
}

#[test]
fn bindings_are_found_as_reachs_tool_finds_them() {
    assert_eq!(
        cui_bindings("a {cui:listing.prop_item_name} b {cui:Grade_Info.prop_grade_name}"),
        [("listing".to_owned(), "prop_item_name".to_owned()), ("Grade_Info".to_owned(), "prop_grade_name".to_owned())]
    );
    // The scan stops at the first binding it can't read.
    assert_eq!(cui_bindings("{cui:TEMP GAMERTAG} {cui:a.b}"), []);
    assert_eq!(fold_binding_name("Grade-Info x"), "grade_info_x");
}

#[test]
fn reach_edits_rebuild_the_substitution_pairs() {
    let mut tag = new_tag("haloreach_mcc", "multilingual_unicode_string_list");
    let mut japanese = entry("b", "{cui:Listing.prop_item_count}");
    japanese.texts.resize(12, None);
    japanese.texts[1] = Some("{cui:map_data.prop_item_name}".to_owned());
    let edits = [StringListEdit::Insert {
        at: 0,
        entries: vec![entry("a", "{cui:listing.prop_item_count} of {cui:listing.prop_item_name}"), japanese],
    }];
    // Another game's edit leaves the block alone.
    let mut untouched = new_tag("haloreach_mcc", "multilingual_unicode_string_list");
    apply_string_list_edits(&mut untouched, Some(GameId::Halo4), &edits).unwrap();
    assert!(read_string_list(&untouched).unwrap().pairs.is_empty());
    apply_string_list_edits(&mut tag, Some(GameId::HaloReach), &edits).unwrap();
    let list = read_string_list(&tag).unwrap();
    let pair = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    assert_eq!(
        list.pairs,
        [pair("listing", "prop_item_count"), pair("listing", "prop_item_name"), pair("map_data", "prop_item_name")],
        "in string, language and text order, folded, each once"
    );
    assert!(list.has_pair("Listing", "prop_item_count"));
    apply_string_list_edits(&mut tag, Some(GameId::HaloReach), &[StringListEdit::Remove { indices: vec![0] }]).unwrap();
    let list = read_string_list(&tag).unwrap();
    assert_eq!(list.pairs, [pair("listing", "prop_item_count"), pair("map_data", "prop_item_name")], "removed with their text");
}

#[test]
fn loading_reads_string_text_from_meta_and_refuses_bad_code_points() {
    let dir = std::env::temp_dir().join(format!("string_text_load_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let meta = dir.join("_meta.json");
    std::fs::write(&meta, r#"{"game": "x"}"#).unwrap();
    let unknown = TextSyntax::load(&dir).unwrap();
    assert!(unknown.has_glyphs() && unknown.glyph_names().is_empty() && !unknown.markup().has_pipes());
    std::fs::write(
        &meta,
        r#"{"string_text": {"glyphs": true, "glyph_match": "first", "glyph_names": {"b": "E101", "a": "E100"},
            "keywords": ["k"], "keyword_match": "prefix", "escapes": "halo2", "tags": null, "cui": null}}"#,
    )
    .unwrap();
    let loaded = TextSyntax::load(&dir).unwrap();
    assert_eq!(loaded.glyph_names(), [("b".to_owned(), 0xE101), ("a".to_owned(), 0xE100)], "in file order");
    assert_eq!(parse_shown("&b&a", &loaded), Ok("\u{E101}\u{E100}".to_owned()));
    assert!(loaded.markup().has_pipes() && !loaded.markup().has_tags());
    std::fs::write(
        &meta,
        r#"{"string_text": {"glyphs": true, "glyph_match": "first", "glyph_names": {"a": "E1"},
            "keywords": [], "keyword_match": "exact", "escapes": "none", "tags": null, "cui": null}}"#,
    )
    .unwrap();
    assert!(matches!(TextSyntax::load(&dir), Err(TextSyntaxError::CodePoint(_, name)) if name == "a"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn colors_carry_their_value_and_where_their_digits_are() {
    let profile = game_syntax("halo3_mcc").markup().clone();
    let text = "W <color argb=#55ffffff>x</color> |kFF00FF00y <color argb=\"0xa1b375\">";
    let colors: Vec<(u32, &str)> = scan_markup(text, &profile)
        .into_iter()
        .filter_map(|span| span.color)
        .map(|color| (color.argb, &text[color.digits]))
        .collect();
    assert_eq!(colors, [(0x55FF_FFFF, "55ffffff"), (0xFF00_FF00, "FF00FF00"), (0x00A1_B375, "a1b375")]);
    let first = scan_markup(text, &profile).into_iter().find_map(|span| span.color).unwrap();
    assert_eq!(first.replaced_in(text, 0x80FF0000), text.replacen("55ffffff", "80FF0000", 1));
    // Placed on the shown text, where a literal & doubles.
    let shown = "&&<color argb=#55ffffff>";
    let analysis = analyze(shown, &game_syntax("halo3_mcc"));
    let color = analysis.markup[0].color.clone().unwrap();
    assert_eq!(&shown[color.digits.clone()], "55ffffff");
    assert_eq!(color.replaced_in(shown, 0x12345678), "&&<color argb=#12345678>");
}


fn hud_tag(text: &str) -> TagFile {
    let mut tag = TagFile::new_classic(definitions().join("haloce_mcc/hud_message_text.json"), crate::classic::ClassicEngine::HaloCe).unwrap();
    apply_string_list_edits(&mut tag, Some(GameId::HaloCe), &[StringListEdit::Insert { at: 0, entries: vec![StringEntry { id: Some("message".into()), texts: vec![Some(text.into())] }] }]).unwrap();
    tag
}

fn reopen_hud(tag: &TagFile) -> TagFile {
    read_classic_tag_file(&tag.write_to_bytes().unwrap(), TagLayout::from_json(definitions().join("haloce_mcc/hud_message_text.json")).unwrap()).unwrap()
}

#[test]
fn hud_runtime_tokens_and_utf16_chunks_survive_classic_save_and_reopen() {
    let text = format!("{}😀%action|n{}%% complete %custom-1%custom-8", "日".repeat(253), "a".repeat(600));
    let tag = hud_tag(&text);
    assert_eq!(read_string_list(&reopen_hud(&tag)).unwrap().entries[0].texts[0].as_deref(), Some(text.as_str()));
    let elements = tag.root().field_path("message elements").unwrap().as_block().unwrap();
    assert!(elements.iter().any(|element| element.read_int_any("data").unwrap() < 0), "text length is an unsigned byte even above 127");
    for name in HUD_TOKENS {
        let text = format!("%{name}");
        assert_eq!(read_string_list(&reopen_hud(&hud_tag(&text))).unwrap().entries[0].texts[0].as_deref(), Some(text.as_str()));
    }
    assert_eq!(read_string_list(&reopen_hud(&hud_tag(""))).unwrap().entries[0].texts[0], None);
}

#[test]
fn hud_invalid_edits_fail_before_mutating_the_tag() {
    let mut tag = hud_tag("Original %action");
    let before = tag.write_to_bytes().unwrap();
    for text in ["%unknown".to_owned(), "abc\0def".to_owned(), "a".repeat(32768), "%action".repeat(256)] {
        assert!(apply_string_list_edits(&mut tag,None,&[StringListEdit::SetText { index:0,language:0,text }]).is_err());
        assert_eq!(tag.write_to_bytes().unwrap(),before);
    }
    for name in ["", " leading", "a=b", "名前", "abcdefghijklmnopqrstuvwxyz123456"] {
        assert!(apply_string_list_edits(&mut tag,None,&[StringListEdit::SetId { index:0,id:name.into() }]).is_err());
        assert_eq!(tag.write_to_bytes().unwrap(),before);
    }
    let entry = read_string_list(&tag).unwrap().entries[0].clone();
    assert!(apply_string_list_edits(&mut tag,None,&[StringListEdit::Insert { at:1,entries:vec![entry;1024] }]).is_err());
    assert_eq!(tag.write_to_bytes().unwrap(),before);
}

#[test]
fn hud_unknown_elements_and_bad_offsets_are_not_silently_rewritten() {
    for (field,value) in [("start index into text blob",TagFieldData::ShortInteger(-1)), ("panel count",TagFieldData::CharInteger(127))] {
        let mut tag=hud_tag("Text %action");
        { let mut root=tag.root_mut(); let mut field_ref=root.field_path_mut("messages").unwrap(); let mut block=field_ref.as_block_mut().unwrap(); set(&mut block.element_mut(0).unwrap(),field,value).unwrap(); }
        assert!(read_string_list(&tag).is_none());
        let before=tag.write_to_bytes().unwrap();
        assert!(apply_string_list_edits(&mut tag,None,&[StringListEdit::SetText {index:0,language:0,text:"Edit".into()}]).is_err());
        assert_eq!(tag.write_to_bytes().unwrap(),before);
    }
    let mut tag=hud_tag("%action");
    { let mut root=tag.root_mut(); let mut field=root.field_path_mut("message elements").unwrap(); let mut block=field.as_block_mut().unwrap(); set(&mut block.element_mut(0).unwrap(),"data",TagFieldData::CharInteger(99)).unwrap(); }
    assert!(read_string_list(&tag).is_none());
}

#[test]
fn hud_native_source_and_editor_syntax_have_distinct_escaping() {
    let syntax=Storage::HudMessages.text_syntax(game_syntax("haloce_mcc"));
    let text="Hold %action|n%something";
    assert!(parse_shown(text,&syntax).is_err());
    assert!(parse_shown(text,&game_syntax("haloce_mcc")).is_ok(), "ordinary CE strings do not acquire HUD syntax");
    let completion=completion_at("Press %cust",11,&syntax).unwrap();
    assert_eq!(completion.kind,CompletionKind::Hud);
    assert_eq!(choices(&syntax,completion.kind).len(),40);
    let native="pickup= Hold %action to pick up %custom-1\\literal=equals\r\nempty=\r\n";
    let rows=parse_hud_messages_file(native).unwrap();
    let entries:Vec<_>=rows.into_iter().map(|(id,text)| StringEntry {id:Some(id),texts:vec![Some(text)]}).collect();
    assert_eq!(hud_messages_file(&entries).unwrap(),native);
    assert!(parse_hud_messages_file("same=a\nsame=b").is_err());
    let tag=hud_tag("100%% complete");
    assert!(hud_messages_file(&read_string_list(&tag).unwrap().entries).is_err(), "native HMT cannot encode a literal percent unambiguously");
}

#[test]
fn hud_hceek_corpus_rebuild_edit_and_source_comparison() {
    let Some(kit)=std::env::var_os("BLAM_TEST_HCEEK").map(PathBuf::from) else { eprintln!("skipped: no BLAM_TEST_HCEEK"); return; };
    fn visit(dir:&Path,paths:&mut Vec<PathBuf>) { for entry in std::fs::read_dir(dir).unwrap().flatten() { let path=entry.path(); if path.is_dir() {visit(&path,paths)} else if path.extension().is_some_and(|ext|ext=="hud_message_text") {paths.push(path)} } }
    let mut paths=Vec::new(); visit(&kit.join("tags"),&mut paths); assert!(!paths.is_empty());
    let mut count=0;
    for path in &paths {
        let mut tag=read_kit_tag(path,"haloce_mcc"); let list=read_string_list(&tag).unwrap(); count+=list.entries.len();
        let original=tag.write_to_bytes().unwrap();
        write_string_list(&mut tag,&list).unwrap(); assert_eq!(tag.write_to_bytes().unwrap(),original,"unchanged HUD tags preserve bytes"); assert_eq!(read_string_list(&reopen_hud(&tag)).unwrap(),list,"{}",path.display());
        let syntax=list.storage.text_syntax(game_syntax("haloce_mcc"));
        for text in list.entries.iter().flat_map(|entry|entry.texts.iter().flatten()) { assert_eq!(parse_shown(&format_shown(text,&syntax),&syntax).unwrap(),*text); }
        if !list.entries.is_empty() {
            apply_string_list_edits(&mut tag,Some(GameId::HaloCe),&[StringListEdit::SetText {index:0,language:0,text:"Changed %action|n日本語 %custom-2".into()},StringListEdit::SetId {index:0,id:"edited_message".into()}]).unwrap();
            let edited=read_string_list(&reopen_hud(&tag)).unwrap(); assert_eq!(edited.entries[0].id.as_deref(),Some("edited_message")); assert_eq!(edited.entries[0].texts[0].as_deref(),Some("Changed %action|n日本語 %custom-2")); assert_eq!(edited.entries[1..],list.entries[1..]);
        }
    }
    let source=std::fs::read(kit.join("data/ui/hud/hud messages.hmt")).unwrap();
    let source=decode_utf16_text(&source[2..]); let parsed=parse_hud_messages_file(&source).unwrap();
    let list=read_string_list(&read_kit_tag(&kit.join("tags/ui/hud/hud messages.hud_message_text"),"haloce_mcc")).unwrap();
    assert_eq!(parsed,list.entries.iter().map(|entry|(entry.id.clone().unwrap(),entry.texts[0].clone().unwrap_or_default())).collect::<Vec<_>>());
    eprintln!("Checked {} HUD tags / {count} messages; source text matches exactly",paths.len());
}


#[test]
fn hud_runtime_boundaries_are_not_reinterpreted_as_different_tokens() {
    let tag=hud_tag("%{back}-button %back-button");
    let list=read_string_list(&reopen_hud(&tag)).unwrap();
    assert_eq!(list.entries[0].texts[0].as_deref(),Some("%{back}-button %back-button"));
    let elements=tag.root().field_path("message elements").unwrap().as_block().unwrap();
    assert_eq!(elements.element(0).unwrap().read_int_any("data"),Some(29));
    let mut rewritten=reopen_hud(&tag); write_string_list(&mut rewritten,&list).unwrap();
    assert_eq!(read_string_list(&reopen_hud(&rewritten)).unwrap(),list);
    assert!(hud_messages_file(&list.entries).is_err());
    assert!(parse_hud_messages_file("name=%%").is_err());
}
