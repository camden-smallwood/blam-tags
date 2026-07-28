//! Probe: does a collision JMS carry a posed armature on a normal MCC kit, where
//! a `render_model` exists to supply one?
//!
//! Run:
//!   cargo run -p blam-tags --example mcc_coll_nodes -- \
//!     <render_model.render_model> <collision.collision_model>

use blam_tags::file::TagFile;
use blam_tags::JmsFile;

fn posed(jms: &JmsFile) -> (usize, usize) {
    let n = jms
        .nodes
        .iter()
        .filter(|n| {
            n.translation.x != 0.0 || n.translation.y != 0.0 || n.translation.z != 0.0
        })
        .count();
    (n, jms.nodes.len())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let render = args.next().expect("render_model path");
    let coll = args.next().expect("collision_model path");

    let render = TagFile::read(&render).expect("read render_model");
    let coll = TagFile::read(&coll).expect("read collision_model");

    let render_jms = JmsFile::from_render_model(&render).expect("render jms");
    let (p, t) = posed(&render_jms);
    println!("render_model JMS: {p}/{t} nodes posed, {} verts", render_jms.vertices.len());

    let plain = JmsFile::from_collision_model(&coll).expect("collision jms");
    let (p, t) = posed(&plain);
    println!("collision, no skeleton: {p}/{t} nodes posed, {} verts", plain.vertices.len());

    let with = JmsFile::from_collision_model_with_skeleton(&coll, &render_jms.nodes)
        .expect("collision jms with skeleton");
    let (p, t) = posed(&with);
    println!("collision, with render skeleton: {p}/{t} nodes posed, {} verts", with.vertices.len());

    let bbox = |jms: &JmsFile| {
        let mut mn = [f32::MAX; 3];
        let mut mx = [f32::MIN; 3];
        for v in &jms.vertices {
            for (i, c) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
                mn[i] = mn[i].min(c);
                mx[i] = mx[i].max(c);
            }
        }
        format!(
            "[{:.1} {:.1} {:.1}]",
            mx[0] - mn[0],
            mx[1] - mn[1],
            mx[2] - mn[2]
        )
    };
    println!("sizes -- render {}, coll(no skel) {}, coll(skel) {}", bbox(&render_jms), bbox(&plain), bbox(&with));
}
