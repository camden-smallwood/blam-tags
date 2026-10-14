use super::{Definitions, Document, FieldKind, Schema, Source, is_halo5_file, is_halo5_folder};
use std::{path::PathBuf, sync::Arc};

fn put(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn tag() -> Vec<u8> {
    // Two metadata data blocks: a 28-byte block header and its float element.
    let mut b = vec![0; 208];
    b[..4].copy_from_slice(b"ucsh");
    put(&mut b, 4, 27);
    put(&mut b, 32, 2);
    put(&mut b, 36, 2);
    put(&mut b, 60, 176);
    put(&mut b, 64, 32);
    put(&mut b, 80, 28);
    b[86] = 1;
    put(&mut b, 96, 4);
    b[102] = 1;
    put(&mut b, 104, 28);
    put(&mut b, 132, 0);
    put(&mut b, 136, u32::MAX); // Main structure target / host.
    b[160] = 1;
    put(&mut b, 164, 1); // Tag block target 1, host 0, field offset 0.
    put(&mut b, 192, 1); // Element count at block header + 16.
    put(&mut b, 204, 1.25f32.to_bits());
    b
}
fn schema() -> Arc<Schema> {
    Arc::new(Schema::parse("<plugin><_field_block_64 name='values'><_field_real name='speed'/></_field_block_64></plugin>").unwrap())
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "baboon-halo5-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn shared_tag_extensions_are_detected_by_header_and_version() {
    let tmp = Temp::new();
    let path = tmp.0.join("example.biped");
    std::fs::write(&path, tag()).unwrap();
    assert!(is_halo5_file(&path));
    assert!(is_halo5_folder(&tmp.0));
    let mut mcc = vec![0; 64];
    mcc[60..64].copy_from_slice(b"MALB");
    std::fs::write(&path, mcc).unwrap();
    assert!(!is_halo5_file(&path));
    assert!(!is_halo5_folder(&tmp.0));
    std::fs::write(&path, b"ucsh").unwrap();
    assert!(!is_halo5_file(&path));
    let mut infinite = tag();
    put(&mut infinite, 4, 53);
    std::fs::write(&path, infinite).unwrap();
    assert!(!is_halo5_file(&path));
    let archive = tmp.0.join("example.module");
    std::fs::write(&archive, b"mohd").unwrap();
    assert!(!is_halo5_file(&archive));
    assert!(!is_halo5_folder(&tmp.0));
    assert!(!is_halo5_file(&tmp.0.join("missing.biped")));
}

#[test]
fn nested_field_write_preserves_header_and_unknown_bytes_and_validates_writes() {
    let original = tag();
    let d = Document::parse(original.clone(), schema()).unwrap();
    let block = d.root_fields().unwrap().remove(0);
    assert!(matches!(block.kind, FieldKind::Block { count: 1, .. }));
    let f = d.block_fields(&block, 0).unwrap().remove(0);
    assert_eq!(
        &d.bytes()[f.offset..f.offset + f.size],
        &1.25f32.to_le_bytes()
    );
    let edited = d
        .with_field_bytes(&[(f.clone(), 2.5f32.to_le_bytes().to_vec())])
        .unwrap();
    assert_eq!(&edited.bytes()[..204], &original[..204]);
    assert_eq!(
        &edited.bytes()[f.offset..f.offset + f.size],
        &2.5f32.to_le_bytes()
    );
    assert_eq!(d.bytes(), original);
    let tmp = Temp::new();
    let output = tmp.0.join("edited.test");
    edited.save_as(&output).unwrap();
    let reopened = Document::open(&output, schema()).unwrap();
    assert_eq!(reopened.bytes(), edited.bytes());
    assert!(d.with_field_bytes(&[(f.clone(), vec![0; 3])]).is_err());
    let mut invalid = f.clone();
    invalid.editable = false;
    assert!(d.with_field_bytes(&[(invalid, vec![0; 4])]).is_err());
    let mut invalid = f.clone();
    invalid.offset = 80;
    assert!(d.with_field_bytes(&[(invalid, vec![0; 4])]).is_err());
    let mut invalid = f;
    invalid.offset = original.len();
    assert!(d.with_field_bytes(&[(invalid, vec![0; 4])]).is_err());
    assert_eq!(d.bytes(), original);
}
#[test]
fn data_fields_follow_metadata_targets_and_refuse_invalid_payload_bounds() {
    let schema=Arc::new(Schema::parse("<plugin><_field_struct name='function' srcguid='5a14a98f-4484-53fa-d468-10809e45f10a'><_field_data_64 name='data'/></_field_struct></plugin>").unwrap());
    assert_eq!(
        schema.fields[0].children[0].data_definition.as_deref(),
        Some("function_definition_data")
    );
    let mut b = vec![0; 224];
    b[..4].copy_from_slice(b"ucsh");
    for (o, v) in [
        (4, 27),
        (32, 2),
        (36, 1),
        (40, 1),
        (60, 164),
        (64, 60),
        (80, 28),
        (96, 32),
        (104, 28),
        (132, 0),
        (136, u32::MAX),
        (152, 1),
        (156, 0),
        (160, 0),
        (188, 32),
    ] {
        put(&mut b, o, v);
    }
    b[86] = 1;
    b[102] = 1;
    b[192] = 0;
    b[193] = 0x24;
    put(&mut b, 200, 1.0f32.to_bits());
    let doc = Document::parse(b.clone(), schema.clone()).unwrap();
    let FieldKind::Inline(fields) = doc.root_fields().unwrap().remove(0).kind else {
        panic!("inline struct missing")
    };
    assert_eq!(doc.data(&fields[0]).unwrap(), &b[192..]);
    assert!(
        !fields[0].editable,
        "the inline data header is metadata, not the payload"
    );
    let mut bad = b.clone();
    put(&mut bad, 152, 2);
    assert!(Document::parse(bad, schema.clone()).is_err());
    let mut bad = b.clone();
    put(&mut bad, 188, 33);
    assert!(
        Document::parse(bad, schema.clone())
            .unwrap()
            .root_fields()
            .is_err()
    );
    let mut bad = b.clone();
    put(&mut bad, 152, u32::MAX);
    assert!(
        Document::parse(bad, schema.clone())
            .unwrap()
            .root_fields()
            .is_err()
    );
    let mut empty = b;
    put(&mut empty, 188, 0);
    put(&mut empty, 152, u32::MAX);
    let doc = Document::parse(empty, schema).unwrap();
    let FieldKind::Inline(fields) = doc.root_fields().unwrap().remove(0).kind else {
        panic!("inline struct missing")
    };
    assert!(doc.data(&fields[0]).unwrap().is_empty());
}
#[test]
fn malformed_headers_layouts_and_block_lengths_are_refused() {
    let mut b = tag();
    put(&mut b, 32, u32::MAX);
    assert!(Document::parse(b, schema()).is_err());
    let mut b = tag();
    put(&mut b, 104, u32::MAX);
    assert!(Document::parse(b, schema()).is_err());
    let wrong = Arc::new(Schema::parse("<plugin><_field_real name='wrong'/></plugin>").unwrap());
    assert!(Document::parse(tag(), wrong).is_err());
    let mut b = tag();
    put(&mut b, 192, 2);
    let d = Document::parse(b, schema()).unwrap();
    assert!(d.root_fields().is_err());
    assert!(Schema::parse("<plugin><_field_unknown name='bad'/></plugin>").is_err());
    for len in 0..208 {
        assert!(
            Document::parse(tag()[..len].to_vec(), schema()).is_err(),
            "truncation at {len}"
        );
    }
}
#[test]
fn every_bundled_xml_layout_has_known_field_widths() {
    let Some(root) = std::env::var_os("HALO5_DEFINITIONS") else {
        return;
    };
    let root = PathBuf::from(root);
    let defs = Definitions::load(&root).unwrap();
    assert_eq!(defs.names().count(), 289);
    for name in defs.names() {
        defs.schema(name)
            .unwrap_or_else(|e| panic!("{name}: {e:#}"));
    }
}
#[test]
fn real_dump_samples_parse_edit_and_save_when_requested() {
    let Some(root) = std::env::var_os("HALO5_TEST_DUMP") else {
        return;
    };
    let root = PathBuf::from(root);
    let definitions = PathBuf::from(
        std::env::var_os("HALO5_DEFINITIONS").expect("set HALO5_DEFINITIONS with HALO5_TEST_DUMP"),
    );
    let defs = Definitions::load(&definitions).unwrap();
    let files = [
        "objects/characters/grunt_mecha/weapons/grunt_mecha_beam_arm.weapon",
        "ai/generic.character",
    ];
    for file in files {
        let source = Source::open(&root.join("tag_dump").join(file), defs.clone()).unwrap();
        let d = source.read(0).unwrap();
        let bytes = d.bytes().to_vec();
        let fields = d.root_fields().unwrap();
        let float = fields
            .iter()
            .find(|f| f.editable && matches!(f.kind, FieldKind::Value(super::ValueKind::Float(1))));
        if let Some(f) = float {
            let edited = d
                .with_field_bytes(&[(f.clone(), 3.5f32.to_le_bytes().to_vec())])
                .unwrap();
            let tmp = Temp::new();
            let p = tmp.0.join("edited.tag");
            edited.save_as(&p).unwrap();
            assert_eq!(
                Document::open(&p, d.schema.clone()).unwrap().bytes(),
                edited.bytes()
            );
            assert_eq!(d.bytes(), bytes);
        }
    }
}
