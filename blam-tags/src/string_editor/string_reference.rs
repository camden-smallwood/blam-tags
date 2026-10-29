//! A resolved string-reference list: separate existence from lookup availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringReferenceSource {
    pub group_tag: u32,
    /// Tag path without an extension, suitable for a reference/Open action.
    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringReferenceEntry {
    pub id: String,
    /// Preview language text, if present. Its absence does not invalidate the ID.
    pub preview: Option<String>,
}
/// One source's contiguous entries, retaining previews for IDs shared by lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringReferenceGroup {
    pub source: StringReferenceSource,
    pub entries: std::ops::Range<usize>,
    pub unavailable: Option<String>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StringReferenceList {
    pub source: Option<StringReferenceSource>,
    pub entries: Vec<StringReferenceEntry>,
    /// Present for a collection of lists. Order follows the configured references;
    /// entries are sorted within each source, and duplicate IDs retain each preview.
    pub groups: Vec<StringReferenceGroup>,
    /// None means lookup succeeded, including when the source list is empty.
    pub unavailable: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StringReferenceValidation {
    Valid,
    Missing,
    Unavailable,
}
impl StringReferenceList {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            unavailable: Some(reason.into()),
            ..Default::default()
        }
    }
    /// Compare exact stored IDs. Blank is the unset value. A failed lookup is
    /// never evidence that a nonempty ID is missing; a known empty list is.
    pub fn validate(&self, id: &str) -> StringReferenceValidation {
        if id.is_empty() {
            StringReferenceValidation::Valid
        } else if self.entries.iter().any(|entry| entry.id == id) {
            StringReferenceValidation::Valid
        } else if self.unavailable.is_some() {
            StringReferenceValidation::Unavailable
        } else {
            StringReferenceValidation::Missing
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Context, StringEditors, TagSource};
    use super::*;
    use crate::classic::ClassicEngine;
    use crate::fields::TagReferenceData;
    use crate::string_list::{self, StringListEdit};
    use crate::{TagFieldData, TagFile};
    use std::collections::HashMap;
    use std::sync::Arc;
    fn root() -> std::path::PathBuf {
        std::env::var_os("BLAM_TEST_DEFINITIONS").map(std::path::PathBuf::from).unwrap_or_else(|| "../definitions".into())
    }
    fn tag(group: &str) -> TagFile {
        TagFile::new_classic(
            root().join(format!("halo2_mcc/{group}.json")),
            ClassicEngine::Halo2V4,
        )
        .unwrap()
    }
    fn editors() -> StringEditors {
        StringEditors::load(root().join("halo2_mcc")).unwrap()
    }
    fn set_ref(tag: &mut TagFile, path: &str, group: &[u8; 4], target: &str) {
        tag.root_mut()
            .field_path_mut(path)
            .unwrap()
            .set(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: Some((u32::from_be_bytes(*group), target.into())),
            }))
            .unwrap();
    }
    struct Tags(HashMap<(u32, String), Arc<TagFile>>);
    impl TagSource for Tags {
        fn tag(&self, group: u32, path: &str) -> Option<Arc<TagFile>> {
            self.0.get(&(group, path.into())).cloned()
        }
    }
    fn fixture(empty: bool) -> Tags {
        let mut globals = tag("globals");
        {
            let mut root = globals.root_mut();
            let mut field = root.field_path_mut("interface tags").unwrap();
            field.as_block_mut().unwrap().add_element();
        }
        set_ref(
            &mut globals,
            "interface tags[0]/hud globals",
            b"hudg",
            "ui\\custom_hud",
        );
        let mut hud = tag("hud_globals");
        set_ref(
            &mut hud,
            "new globals/hud text",
            b"unic",
            "ui\\custom_strings",
        );
        let mut strings = tag("multilingual_unicode_string_list");
        if !empty {
            let list = string_list::read_string_list(&strings).unwrap();
            let mut first = list.new_entry();
            first.id = Some("weapon_swap".into());
            first.texts[0] = Some("Hold action to swap for a rifle".into());
            let mut second = list.new_entry();
            second.id = Some("empty_english".into());
            second.texts[1] = Some("他の言語".into());
            string_list::apply_string_list_edits(
                &mut strings,
                None,
                &[StringListEdit::Insert {
                    at: 0,
                    entries: vec![first, second],
                }],
            )
            .unwrap();
        }
        Tags(
            [
                (u32::from_be_bytes(*b"matg"), "globals\\globals", globals),
                (u32::from_be_bytes(*b"hudg"), "ui\\custom_hud", hud),
                (u32::from_be_bytes(*b"unic"), "ui\\custom_strings", strings),
            ]
            .into_iter()
            .map(|(g, p, t)| ((g, p.into()), Arc::new(t)))
            .collect(),
        )
    }
    #[test]
    fn string_references_resolve_real_h2_chain_and_preserve_ids_without_english() {
        let editors = editors();
        let tags = fixture(false);
        let weapon = tag("weapon");
        let ctx = Context {
            tag: &weapon,
            tag_path: "objects\\weapon",
            field_path: "item/pickup message",
        };
        let list = editors
            .string_references("hud_message", &ctx, &tags)
            .unwrap();
        assert!(list.unavailable.is_none(), "{:?}", list.unavailable);
        assert_eq!(list.source.unwrap().path, "ui\\custom_strings");
        assert_eq!(
            list.entries
                .iter()
                .map(|e| e.id.as_str())
                .collect::<Vec<_>>(),
            ["empty_english", "weapon_swap"]
        );
        assert!(list.entries[0].preview.is_none());
        assert!(list.entries[1].preview.as_ref().unwrap().contains("rifle"));
        let list = editors
            .string_references("hud_message", &ctx, &tags)
            .unwrap();
        assert_eq!(
            list.validate("empty_english"),
            StringReferenceValidation::Valid
        );
        assert_eq!(list.validate("missing"), StringReferenceValidation::Missing);
        assert_eq!(list.validate(""), StringReferenceValidation::Valid);
        assert_eq!(
            list.validate("WEAPON_SWAP"),
            StringReferenceValidation::Missing,
            "exact stored IDs are preserved"
        );
    }
    #[test]
    fn string_references_distinguish_known_empty_source_from_unavailable_chain() {
        let editors = editors();
        let weapon = tag("weapon");
        let ctx = Context {
            tag: &weapon,
            tag_path: "",
            field_path: "",
        };
        let mut tags = fixture(true);
        let empty = editors
            .string_references("hud_message", &ctx, &tags)
            .unwrap();
        assert!(empty.unavailable.is_none());
        assert_eq!(
            empty.validate("anything"),
            StringReferenceValidation::Missing
        );
        tags.0
            .remove(&(u32::from_be_bytes(*b"unic"), "ui\\custom_strings".into()));
        let missing = editors
            .string_references("hud_message", &ctx, &tags)
            .unwrap();
        assert_eq!(
            missing.validate("anything"),
            StringReferenceValidation::Unavailable
        );
        assert_eq!(
            missing.source.unwrap().path,
            "ui\\custom_strings",
            "known destination can still be opened when unreadable"
        );
        tags.0
            .remove(&(u32::from_be_bytes(*b"hudg"), "ui\\custom_hud".into()));
        let missing = editors
            .string_references("hud_message", &ctx, &tags)
            .unwrap();
        assert!(
            missing.source.is_none(),
            "do not offer intermediate globals as the string list"
        );
        assert_eq!(
            missing.validate("anything"),
            StringReferenceValidation::Unavailable
        );
    }
    #[test]
    fn string_references_h2_definition_annotations_cover_weapon_and_equipment() {
        let editors = StringEditors::load(root().join("halo2_mcc")).unwrap();
        for group in ["weapon", "equipment"] {
            let tag = tag(group);
            for name in [
                "pickup message",
                "swap message",
                "pickup or dual msg",
                "swap or dual msg",
                "dual-only msg",
                "picked up msg",
                "singluar quantity msg",
                "plural quantity msg",
                "switch-to msg",
                "switch-to from ai msg",
            ] {
                let path = format!("item/{name}");
                assert_eq!(
                    editors.editor_at(tag.root(), &path),
                    Some("hud_message"),
                    "{group}: {path}"
                );
            }
        }
    }
    #[test]
    fn string_references_optional_h2ek_saved_corpus_lookup() {
        let Some(kit) = std::env::var_os("BLAM_TEST_H2EK") else {
            return;
        };
        let kit = super::super::tests::KitTags::new(
            std::path::PathBuf::from(kit).join("tags"),
            "halo2_mcc",
        );
        let editors = editors();
        let weapon = tag("weapon");
        let ctx = Context {
            tag: &weapon,
            tag_path: "",
            field_path: "item/pickup message",
        };
        let list = editors
            .string_references("hud_message", &ctx, &kit)
            .unwrap();
        assert!(list.unavailable.is_none(), "{:?}", list.unavailable);
        assert!(!list.entries.is_empty());
        let mut checked = 0;
        for weapon in ["battle_rifle", "smg", "shotgun", "plasma_rifle"] {
            let path = format!("objects\\weapons\\rifle\\{weapon}\\{weapon}");
            let tag =
                TagSource::tag(&kit, u32::from_be_bytes(*b"weap"), &path).expect("stock H2 weapon");
            for field in [
                "pickup message",
                "swap message",
                "pickup or dual msg",
                "swap or dual msg",
                "dual-only msg",
                "picked up msg",
                "singluar quantity msg",
                "plural quantity msg",
                "switch-to msg",
                "switch-to from ai msg",
            ] {
                let holder = tag.root().descend("item").expect("native item fields");
                assert!(holder.field(field).is_some(), "{weapon}: {field}");
                assert_eq!(
                    editors.editor_at(tag.root(), &format!("item/{field}")),
                    Some("hud_message")
                );
                let id = holder.read_string_id(field).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                assert_eq!(
                    list.validate(&id),
                    StringReferenceValidation::Valid,
                    "{weapon}/{field}: {id}"
                );
                checked += 1;
            }
        }
        eprintln!(
            "Resolved {} saved H2 HUD IDs; checked {checked} nonempty action references in 4 stock weapons; source {:?}",
            list.entries.len(),
            list.source
        );
    }

    fn gen3_tag(game: &str, group: &str) -> TagFile {
        TagFile::new(root().join(game).join(format!("{group}.json"))).unwrap()
    }
    fn add_elements(tag: &mut TagFile, path: &str, count: usize) {
        let mut root = tag.root_mut();
        let mut field = root.field_path_mut(path).unwrap();
        let mut block = field.as_block_mut().unwrap();
        for _ in 0..count {
            block.add_element();
        }
    }
    fn chud_fixture(game: &str) -> Tags {
        let mut globals = gen3_tag(game, "globals");
        add_elements(&mut globals, "interface tags", 1);
        set_ref(
            &mut globals,
            "interface tags[0]/chud globals",
            b"chgd",
            "ui\\custom_chud",
        );
        let mut chud = gen3_tag(game, "chud_globals_definition");
        add_elements(&mut chud, "skins", 2);
        for index in 0..2 {
            set_ref(
                &mut chud,
                &format!("skins[{index}]/chud text"),
                b"unic",
                "ui\\custom_strings",
            );
        }
        let mut strings = gen3_tag(game, "multilingual_unicode_string_list");
        let list = string_list::read_string_list(&strings).unwrap();
        let mut entry = list.new_entry();
        entry.id = Some("vehicle_enter".into());
        entry.texts[0] = Some("Enter vehicle".into());
        string_list::apply_string_list_edits(
            &mut strings,
            None,
            &[StringListEdit::Insert {
                at: 0,
                entries: vec![entry],
            }],
        )
        .unwrap();
        Tags(
            [
                (u32::from_be_bytes(*b"matg"), "globals\\globals", globals),
                (u32::from_be_bytes(*b"chgd"), "ui\\custom_chud", chud),
                (u32::from_be_bytes(*b"unic"), "ui\\custom_strings", strings),
            ]
            .into_iter()
            .map(|(g, p, t)| ((g, p.into()), Arc::new(t)))
            .collect(),
        )
    }
    #[test]
    fn string_references_chud_chains_and_inherited_item_and_seat_annotations() {
        for game in [
            "halo3_mcc",
            "halo3odst_mcc",
            "haloreach_mcc",
            "haloce_evolved",
        ] {
            let editors = StringEditors::load(root().join(game)).unwrap();
            let source = chud_fixture(game);
            if ["haloreach_mcc", "haloce_evolved"].contains(&game) {
                for group in ["weapon", "equipment", "vehicle", "biped", "scenery", "crate"] {
                    let tag = gen3_tag(game, group);
                    let path = match group {
                        "weapon" | "equipment" => "item/object/generic hud text",
                        "vehicle" | "biped" => "unit/object/generic hud text",
                        _ => "object/generic hud text",
                    };
                    assert_eq!(
                        editors.editor_at(tag.root(), path),
                        Some("hud_message"),
                        "{game}/{group}/generic hud text"
                    );
                }
            }
            for group in ["weapon", "equipment"] {
                let tag = gen3_tag(game, group);
                for name in [
                    "pickup message",
                    "swap message",
                    "pickup message (dual)",
                    "swap message (dual)",
                    "picked up msg",
                    "switch-to msg",
                    "switch-to from ai msg",
                    "notify empty msg",
                ] {
                    assert_eq!(
                        editors.editor_at(tag.root(), &format!("item/{name}")),
                        Some("hud_message"),
                        "{game}/{group}/{name}"
                    );
                }
                if ["haloreach_mcc", "haloce_evolved"].contains(&game) {
                    assert_eq!(
                        editors.editor_at(tag.root(), "item/notify overheated message"),
                        Some("hud_message")
                    );
                }
                let list = editors
                    .string_references(
                        "hud_message",
                        &Context {
                            tag: &tag,
                            tag_path: "",
                            field_path: "item/pickup message",
                        },
                        &source,
                    )
                    .unwrap();
                assert!(list.unavailable.is_none(), "{game}: {:?}", list.unavailable);
                assert_eq!(
                    list.validate("vehicle_enter"),
                    StringReferenceValidation::Valid
                );
                assert_eq!(list.entries[0].preview.as_deref(), Some("Enter vehicle"));
            }
            for group in ["vehicle", "biped"] {
                let mut tag = gen3_tag(game, group);
                add_elements(&mut tag, "unit/seats", 1);
                for name in [
                    "enter seat string",
                    "boarding grenade string",
                    "boarding melee string",
                    "in-seat string",
                ] {
                    assert_eq!(
                        editors.editor_at(tag.root(), &format!("unit/seats[0]/{name}")),
                        Some("hud_message"),
                        "{game}/{group}/{name}"
                    );
                }
            }
        }
        let editors = editors();
        for group in ["vehicle", "biped"] {
            let mut tag = tag(group);
            add_elements(&mut tag, "unit/seats", 1);
            for name in [
                "enter seat string",
                "boarding grenade string",
                "boarding melee string",
            ] {
                assert_eq!(
                    editors.editor_at(tag.root(), &format!("unit/seats[0]/{name}")),
                    Some("hud_message"),
                    "halo2/{group}/{name}"
                );
            }
        }
    }
    #[test]
    fn string_references_chud_requires_a_shared_populated_skin_reference() {
        let game = "halo3_mcc";
        let editors = StringEditors::load(root().join(game)).unwrap();
        let tag = gen3_tag(game, "weapon");
        let ctx = Context {
            tag: &tag,
            tag_path: "",
            field_path: "item/pickup message",
        };
        let key = (u32::from_be_bytes(*b"chgd"), "ui\\custom_chud".to_string());
        let mut source = chud_fixture(game);
        // Equivalent spelling remains the same list, without rewriting its path.
        set_ref(
            Arc::get_mut(source.0.get_mut(&key).unwrap()).unwrap(),
            "skins[1]/chud text",
            b"unic",
            "UI/custom_strings",
        );
        let list = editors
            .string_references("hud_message", &ctx, &source)
            .unwrap();
        assert!(list.unavailable.is_none(), "{:?}", list.unavailable);
        for target in [
            Some((u32::from_be_bytes(*b"unic"), "ui\\other".into())),
            Some((u32::from_be_bytes(*b"unic"), String::new())),
            None,
        ] {
            Arc::get_mut(source.0.get_mut(&key).unwrap())
                .unwrap()
                .root_mut()
                .field_path_mut("skins[1]/chud text")
                .unwrap()
                .set(TagFieldData::TagReference(TagReferenceData {
                    group_tag_and_name: target,
                }))
                .unwrap();
            let list = editors
                .string_references("hud_message", &ctx, &source)
                .unwrap();
            assert_eq!(
                list.validate("vehicle_enter"),
                StringReferenceValidation::Unavailable
            );
            assert!(list.source.is_none());
        }
    }
    #[test]
    fn string_references_optional_chud_saved_corpus_lookup() {
        for (game, var) in [
            ("halo3_mcc", "BLAM_TEST_H3EK"),
            ("halo3odst_mcc", "BLAM_TEST_ODSTEK"),
            ("haloreach_mcc", "BLAM_TEST_HREK"),
        ] {
            let Some(path) = std::env::var_os(var) else {
                continue;
            };
            let source = super::super::tests::KitTags::new(path.into(), game);
            let editors = StringEditors::load(root().join(game)).unwrap();
            let tag = gen3_tag(game, "weapon");
            let list = editors
                .string_references(
                    "hud_message",
                    &Context {
                        tag: &tag,
                        tag_path: "",
                        field_path: "item/pickup message",
                    },
                    &source,
                )
                .unwrap();
            assert!(list.unavailable.is_none(), "{game}: {:?}", list.unavailable);
            assert!(!list.entries.is_empty(), "{game}");
            assert!(list.source.is_some());
            eprintln!("{game}: resolved {} saved HUD IDs", list.entries.len());
        }
    }

    fn collection_editors(game: &str) -> StringEditors {
        let mut editors = StringEditors::load(root().join(game)).unwrap();
        let chain = |mode: &str| serde_json::json!(["globals", {"ref": format!("interface tags[0]/{mode} ui globals")}, {"ref": "HUD globals"}]);
        editors.specs.insert("hud_message".into(), serde_json::from_value(serde_json::json!({
            "builder": "string_list", "sources": [chain("singleplayer"), chain("multiplayer")],
            "lists": {"block": "string references", "ref": "string list"}
        })).unwrap());
        editors
    }
    fn collection_strings(game: &str, rows: &[(&str, &str)]) -> TagFile {
        let mut tag = gen3_tag(game, "multilingual_unicode_string_list");
        let list = string_list::read_string_list(&tag).unwrap();
        let entries = rows.iter().map(|(id,text)| {let mut row=list.new_entry();row.id=Some((*id).into());row.texts[0]=Some((*text).into());row}).collect();
        string_list::apply_string_list_edits(&mut tag,None,&[StringListEdit::Insert{at:0,entries}]).unwrap();tag
    }
    fn collection_fixture(game: &str) -> Tags {
        let mut globals=gen3_tag(game,"globals");add_elements(&mut globals,"interface tags",1);
        set_ref(&mut globals,"interface tags[0]/multiplayer ui globals",b"wgtz","ui\\mp");
        // A mode-specific kit can retain dangling references to other modes.
        set_ref(&mut globals,"interface tags[0]/singleplayer ui globals",b"wgtz","ui\\missing_sp");
        let mut ui=gen3_tag(game,"user_interface_globals_definition");set_ref(&mut ui,"HUD globals",b"uihg","ui\\hud");
        let mut hud=gen3_tag(game,"user_interface_hud_globals_definition");add_elements(&mut hud,"string references",4);
        for(i,path)in ["ui\\strings\\first","ui\\strings\\second","UI/STRINGS/FIRST"].iter().enumerate(){set_ref(&mut hud,&format!("string references[{i}]/string list"),b"unic",path);}
        Tags([
            (b"matg","globals\\globals",globals),(b"wgtz","ui\\mp",ui),(b"uihg","ui\\hud",hud),
            (b"unic","ui\\strings\\first",collection_strings(game,&[("shared","First preview"),("first_only","First only")])),
            (b"unic","ui\\strings\\second",collection_strings(game,&[("second_only","Second only"),("shared","Second preview")])),
        ].into_iter().map(|(g,p,t)|((u32::from_be_bytes(*g),p.into()),Arc::new(t))).collect())
    }
    #[test]
    fn string_references_collect_lists_preserving_source_order_and_conflicting_previews() {
        for game in ["halo2amp_mcc","halo4_mcc"] {
            let editors=collection_editors(game);let tags=collection_fixture(game);let tag=gen3_tag(game,"vehicle");
            let list=editors.string_references("hud_message",&Context{tag:&tag,tag_path:"",field_path:"flip message"},&tags).unwrap();
            assert_eq!(list.unavailable,None);assert_eq!(list.source,None);assert_eq!(list.groups.len(),2);
            assert_eq!(list.groups[0].source.path,"ui\\strings\\first");assert_eq!(list.groups[0].entries,0..2);
            assert_eq!(list.groups[1].entries,2..4);
            assert_eq!(list.entries.iter().filter(|e|e.id=="shared").map(|e|e.preview.as_deref().unwrap()).collect::<Vec<_>>(),["First preview","Second preview"]);
            for id in ["first_only","second_only","shared",""]{assert_eq!(list.validate(id),StringReferenceValidation::Valid);}
            assert_eq!(list.validate("FIRST_ONLY"),StringReferenceValidation::Missing);
            assert_eq!(list.validate("unlisted"),StringReferenceValidation::Missing);
        }
    }
    #[test]
    fn string_references_partial_collection_keeps_known_ids_and_marks_unknowns_unavailable() {
        let game="halo2amp_mcc";let editors=collection_editors(game);let mut tags=collection_fixture(game);let tag=gen3_tag(game,"vehicle");
        tags.0.remove(&(u32::from_be_bytes(*b"unic"),"ui\\strings\\second".into()));
        let list=editors.string_references("hud_message",&Context{tag:&tag,tag_path:"",field_path:""},&tags).unwrap();
        assert!(list.unavailable.is_some());assert_eq!(list.groups.len(),2);assert!(list.groups[1].unavailable.is_some());
        assert_eq!(list.groups[1].entries,2..2);assert_eq!(list.validate("first_only"),StringReferenceValidation::Valid);
        assert_eq!(list.validate("second_only"),StringReferenceValidation::Unavailable);assert_eq!(list.validate(""),StringReferenceValidation::Valid);
    }
    #[test]
    fn string_references_empty_collection_is_known_empty_and_missing_roots_are_unavailable() {
        let game="halo4_mcc";let editors=collection_editors(game);let mut tags=collection_fixture(game);let tag=gen3_tag(game,"vehicle");
        let key=(u32::from_be_bytes(*b"uihg"),"ui\\hud".into());
        tags.0.insert(key,Arc::new(gen3_tag(game,"user_interface_hud_globals_definition")));
        let ctx=Context{tag:&tag,tag_path:"",field_path:""};let list=editors.string_references("hud_message",&ctx,&tags).unwrap();
        assert_eq!(list.unavailable,None);assert_eq!(list.validate("anything"),StringReferenceValidation::Missing);
        tags.0.clear();let list=editors.string_references("hud_message",&ctx,&tags).unwrap();
        assert_eq!(list.validate("anything"),StringReferenceValidation::Unavailable);assert!(list.groups.is_empty());assert!(list.source.is_none());
    }
    #[test]
    fn string_references_collect_distinct_available_mode_roots() {
        let game="halo4_mcc";let editors=collection_editors(game);let mut tags=collection_fixture(game);let tag=gen3_tag(game,"vehicle");
        let mut ui=gen3_tag(game,"user_interface_globals_definition");set_ref(&mut ui,"HUD globals",b"uihg","ui\\sp_hud");
        tags.0.insert((u32::from_be_bytes(*b"wgtz"),"ui\\missing_sp".into()),Arc::new(ui));
        let mut hud=gen3_tag(game,"user_interface_hud_globals_definition");add_elements(&mut hud,"string references",1);set_ref(&mut hud,"string references[0]/string list",b"unic","ui\\strings\\sp");
        tags.0.insert((u32::from_be_bytes(*b"uihg"),"ui\\sp_hud".into()),Arc::new(hud));
        tags.0.insert((u32::from_be_bytes(*b"unic"),"ui\\strings\\sp".into()),Arc::new(collection_strings(game,&[("sp_only","Campaign text")])));
        let list=editors.string_references("hud_message",&Context{tag:&tag,tag_path:"",field_path:""},&tags).unwrap();
        assert_eq!(list.unavailable,None);assert_eq!(list.groups.len(),3);assert_eq!(list.groups[0].source.path,"ui\\strings\\sp");
        assert_eq!(list.validate("sp_only"),StringReferenceValidation::Valid);assert_eq!(list.validate("second_only"),StringReferenceValidation::Valid);
    }
    #[test]
    fn string_references_optional_h2a_saved_collection() {
        let Some(root)=std::env::var_os("BLAM_TEST_H2AMPEK")else{return};let game="halo2amp_mcc";
        let editors=collection_editors(game);let tags=super::super::tests::KitTags::new(root.into(),game);let tag=gen3_tag(game,"vehicle");
        let list=editors.string_references("hud_message",&Context{tag:&tag,tag_path:"",field_path:"flip message"},&tags).unwrap();
        assert_eq!(list.unavailable,None);assert_eq!(list.groups.len(),6);assert_eq!(list.entries.len(),1788);
        let ids:std::collections::BTreeSet<_>=list.entries.iter().map(|e|&e.id).collect();assert_eq!(ids.len(),1766);
        assert_eq!(list.validate("warthog_flip"),StringReferenceValidation::Valid);assert_eq!(list.validate("device_h2a_coagulation_emp_use"),StringReferenceValidation::Valid);
    }
}
