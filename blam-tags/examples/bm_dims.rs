use blam_tags::TagFile;
use blam_tags::bitmap::Bitmap;
fn main() {
    let root="/Users/camden/Halo/halo3_mcc/tags";
    for b in ["ash_01","snowflake"] {
        let p=format!("{root}/fx/particles/weather/_bitmaps/{b}.bitmap");
        let tag=TagFile::read(&p).unwrap();
        let bm=Bitmap::new(&tag).unwrap();
        println!("== {b} ==");
        for (i,img) in bm.iter().enumerate() {
            let (w,h)=(img.width(),img.height());
            println!("  bitmap[{i}] {w}x{h} aspect={:.4}", w as f32/h.max(1) as f32);
        }
        for (si,sq) in bm.sequences().iter().enumerate() {
            println!("  sequence[{si}] sprites={}", sq.sprites.len());
            for (i,s) in sq.sprites.iter().enumerate() {
                println!("    sprite[{i}] L={:.4} R={:.4} T={:.4} B={:.4} reg=({:.4},{:.4}) w={:.4} h={:.4}",
                    s.left,s.right,s.top,s.bottom,s.registration_point[0],s.registration_point[1],
                    s.right-s.left, s.bottom-s.top);
            }
        }
    }
}
