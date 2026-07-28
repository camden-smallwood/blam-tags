use blam_tags::file::TagFile;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: <sbsp>")?;
    let tag = TagFile::read(&path)?;
    let root = tag.root();
    let ri = root.field("resource interface").and_then(|f| f.as_struct()).ok_or("no ri")?;
    let rr = ri.field("raw_resources").and_then(|f| f.as_block()).ok_or("no rr")?;
    let elem0 = rr.element(0).ok_or("no elem0")?;
    let items = elem0.field("raw_items").and_then(|f| f.as_struct()).ok_or("no items")?;
    let defs = items.field("instanced geometries definitions").and_then(|f| f.as_block()).ok_or("no defs")?;
    let n = defs.len();
    let (mut with_render_bsp, mut with_surf, mut with_large_surf, mut with_stm) = (0,0,0,0);
    for i in 0..n {
        let d = defs.element(i).unwrap();
        let bc = |name: &str| d.field(name).and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
        let rb = bc("render bsp");
        let sf = bc("surfaces");
        let ls = bc("large surfaces");
        let stm = bc("surface to triangle mapping");
        if rb>0 { with_render_bsp+=1; }
        if sf>0 { with_surf+=1; }
        if ls>0 { with_large_surf+=1; }
        if stm>0 { with_stm+=1; }
        if i<3 || rb>0 || ls>0 { println!("def[{i}] render_bsp={rb} surfaces={sf} large_surfaces={ls} surf_tri={stm}"); }
    }
    println!("--- of {n} defs: render_bsp>0:{with_render_bsp} surfaces>0:{with_surf} large_surfaces>0:{with_large_surf} surf_tri>0:{with_stm}");
    Ok(())
}
