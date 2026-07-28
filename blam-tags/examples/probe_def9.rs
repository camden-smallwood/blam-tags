use blam_tags::file::TagFile;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: <sbsp>")?;
    let target: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(9);
    let tag = TagFile::read(&path)?;
    let root = tag.root();
    let ri = root.field("resource interface").and_then(|f| f.as_struct()).ok_or("no ri")?;
    let rr = ri.field("raw_resources").and_then(|f| f.as_block()).ok_or("no rr")?;
    let elem0 = rr.element(0).ok_or("no elem0")?;
    let items = elem0.field("raw_items").and_then(|f| f.as_struct()).ok_or("no items")?;
    let defs = items.field("instanced geometries definitions").and_then(|f| f.as_block()).ok_or("no defs")?;
    println!("total defs: {}", defs.len());
    let d = defs.element(target).ok_or("no def")?;
    let bc = |name: &str| d.field(name).and_then(|f| f.as_block()).map(|b| b.len());
    let sc = |name: &str| {
        d.field(name).and_then(|f| f.as_struct()).map(|cs| {
            let g = |n: &str| cs.field(n).and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
            (g("surfaces"), g("large surfaces"), g("bsp3d nodes"), g("large bsp3d nodes"))
        })
    };
    println!("def[{target}] mesh index = {:?}", d.read_int_any("mesh index"));
    println!("  collision info: (surfaces,large,bsp3d,large_bsp3d) = {:?}", sc("collision info"));
    println!("  poopie cutter:  (surfaces,large,bsp3d,large_bsp3d) = {:?}", sc("poopie cutter collision"));
    println!("  render bsp (block count) = {:?}", bc("render bsp"));
    if let Some(rb) = d.field("render bsp").and_then(|f| f.as_block()) {
        if let Some(e0) = rb.element(0) {
            let g = |n: &str| e0.field(n).and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
            println!("    render bsp[0]: (surfaces,large,bsp3d,large_bsp3d) = ({},{},{},{})",
                g("surfaces"), g("large surfaces"), g("bsp3d nodes"), g("large bsp3d nodes"));
        }
    }
    println!("  def-level surfaces* (small)  = {:?}", bc("surfaces"));
    println!("  def-level large surfaces*    = {:?}", bc("large surfaces"));
    println!("  def-level surf->tri mapping* = {:?}", bc("surface to triangle mapping"));
    Ok(())
}
