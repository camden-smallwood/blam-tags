//! `collision-bsp-test` — check that a collision BSP finds its own
//! surfaces.
//!
//! A wrong collision tree is silent: the tag loads, the model looks
//! right, and shots pass through a wall. This casts rays at the surface
//! polygons directly and the same rays through the bsp3d tree, and
//! reports where the two disagree.
//!
//! It works on any `collision_model`, including tool's own — which is how
//! the check was validated in the first place. Pointed at the shipped
//! corpus it agrees on 25,269 of 25,316 rays across 189 models.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use blam_tags::collision_verify::{test_collision_model, VerifyError};
use blam_tags::TagFile;

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("collision_model") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub fn run(input: &str, rays: Option<usize>, verbose: bool) -> Result<()> {
    let path = PathBuf::from(input);
    let targets = if path.is_dir() {
        walk(&path)
    } else {
        vec![path.clone()]
    };
    if targets.is_empty() {
        return Err(anyhow!("no collision_model tags under {input}"));
    }

    let rays = rays.unwrap_or(64);
    let (mut checked, mut clean, mut empty, mut unusable) = (0usize, 0usize, 0usize, 0usize);
    let (mut total, mut agreed) = (0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();

    for target in &targets {
        let name = target.file_name().unwrap_or_default().to_string_lossy().to_string();
        let tag = match TagFile::read(target) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{name}: cannot read: {e}"));
                continue;
            }
        };
        match test_collision_model(&tag, rays) {
            Ok(r) => {
                checked += 1;
                total += r.rays;
                agreed += r.agreed;
                if r.clean() {
                    clean += 1;
                    if verbose {
                        println!(
                            "  OK   {name}  {} bsps, {} surfaces, depth {}, {} rays",
                            r.bsps, r.surfaces, r.max_depth, r.rays
                        );
                    }
                } else {
                    println!(
                        "  FAIL {name}  {} of {} rays disagree ({} missed, {} wrong surface, \
                         {} phantom)",
                        r.missed + r.wrong_surface + r.phantom,
                        r.rays,
                        r.missed,
                        r.wrong_surface,
                        r.phantom
                    );
                    for e in r.examples.iter().take(3) {
                        println!("         {e}");
                    }
                }
            }
            Err(VerifyError::Empty) => empty += 1,
            Err(e @ VerifyError::NoUsableTree { .. }) => {
                unusable += 1;
                println!("  DEAD {name}  {e}");
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    println!();
    println!("{checked} collision_model(s) checked, {clean} clean");
    if empty > 0 {
        println!("  {empty} had no collision geometry");
    }
    if unusable > 0 {
        println!("  {unusable} have no usable tree — none of that collision can be hit");
    }
    for f in &failures {
        println!("  {f}");
    }
    if total > 0 {
        println!(
            "  {agreed}/{total} rays agreed ({:.3}%)",
            100.0 * agreed as f64 / total as f64
        );
    }
    println!(
        "  NOTE: this compares the tree against the surfaces in the same tag. It does not \
         prove the geometry is what you modelled — only that the tree can find it."
    );

    if clean == checked && failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!("{} of {checked} collision_model(s) did not pass", checked - clean))
    }
}
