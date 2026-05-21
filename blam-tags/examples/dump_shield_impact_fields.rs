use blam_tags::TagFile;
fn main() {
    let t = TagFile::read("/Users/camden/Halo/halo3_mcc/tags/globals/global_shield_impact_settings.shield_impact").unwrap();
    let root = t.root();
    for f in root.fields() {
        println!("  type={:?}  name={:?}", f.field_type(), f.name());
    }
}
