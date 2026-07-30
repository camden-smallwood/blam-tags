//! Is `flattened_properties` correct? `schema_index` is documented as the index
//! within the FLATTENED schema, but the function concatenates per-struct lists.
//! Compare, across every class in the usmap.
use blam_tags::iostore::usmap::Usmap;
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
fn main(){
    let usmap=Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let (mut ok,mut bad,mut nogaps)=(0usize,0usize,0usize);
    let mut samples=Vec::new();
    for s in &usmap.structs{
        let Some(flat)=usmap.flattened_properties(&s.name) else{continue};
        if flat.is_empty(){continue}
        // does position == schema_index for every entry?
        let matches=flat.iter().enumerate().all(|(i,p)|p.schema_index as usize==i);
        if matches {ok+=1} else {
            bad+=1;
            if samples.len()<8 {
                let got:Vec<(usize,u16,&str)>=flat.iter().enumerate().take(8).map(|(i,p)|(i,p.schema_index,p.name.as_str())).collect();
                samples.push(format!("{} (prop_count={}, chain flat len={})\n      pos/schema_index/name: {:?}", s.name, s.prop_count, flat.len(), got));
            }
        }
        if flat.len()!=s.prop_count as usize {nogaps+=1}
    }
    // Hypothesis: expanding each property array_dim times makes position == schema_index
    let (mut ok2, mut bad2) = (0usize, 0usize);
    let mut bad2s = Vec::new();
    for s in &usmap.structs {
        let Some(flat) = usmap.flattened_properties(&s.name) else { continue };
        if flat.is_empty() { continue }
        let mut pos = 0usize; let mut good = true; let mut cur_base = 0usize; let mut last_si = -1i64;
        for p in &flat {
            if (p.schema_index as i64) <= last_si { cur_base = pos; }
            last_si = p.schema_index as i64;
            if cur_base + p.schema_index as usize != pos { good = false; break }
            pos += p.array_dim.max(1) as usize;
        }
        if good { ok2 += 1 } else { bad2 += 1; if bad2s.len() < 6 { bad2s.push(s.name.clone()) } }
    }
    println!("with array_dim expansion + per-struct rebase: ok {ok2}, bad {bad2}  {bad2s:?}");
    println!("classes where position == schema_index : {ok}");
    println!("classes where it does NOT              : {bad}");
    println!("classes where flat len != prop_count   : {nogaps}");
    println!("\nsamples of mismatch:");
    for x in &samples{println!("   {x}");}
    // specifically the ones that matter
    for c in ["MaterialInstanceConstant","MaterialParameterInfo","BlamBipedTagDataAsset","BlamMeshSynchronizationComponent","Texture2D"]{
        if let (Some(s),Some(f))=(usmap.get(c),usmap.flattened_properties(c)){
            let m=f.iter().enumerate().all(|(i,p)|p.schema_index as usize==i);
            println!("\n{c}: prop_count={} flat={} positions_match={m}", s.prop_count, f.len());
            for (i,p) in f.iter().enumerate().take(10){ println!("   pos {i:2} schema_index {:2} array_dim {:2}  {}", p.schema_index, p.array_dim, p.name); }
        }
    }
}
