//! Dump the 18 compression_vectors from a scenario_lightmap_bsp_data
//! to diagnose which slots actually carry SH L-range values.

use blam_tags::file::TagFile;
use blam_tags::scenario_lightmap::LightmapBspData;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_lightmap_compress <scenario_lightmap_bsp_data path>");
    let tag = TagFile::read(&path).expect("failed to read tag");
    let lbsp = LightmapBspData::from_tag(&tag).expect("failed to parse");

    {
        let bsp_i = 0;
        println!("=== BSP {} (ref_index={}) ===", bsp_i, lbsp.bsp_reference_index);
        println!("flags=0x{:04x}  18 compression_vectors:", lbsp.flags);
        for (i, v) in lbsp.compression_vectors.iter().enumerate() {
            println!("  [{i:2}] = ({:12.4}, {:12.4}, {:12.4})", v.i, v.j, v.k);
        }
        println!();
        println!("Pattern protomorph uses: compression_vectors[2k].i for k ∈ 0..9 (9 ranges):");
        for k in 0..9 {
            let idx = 2 * k;
            let v = &lbsp.compression_vectors[idx];
            println!("  range[{k}] = vectors[{idx}].i = {:12.4}", v.i);
        }
    }
}
