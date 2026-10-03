//! Re-express a JMS for the other generation's tools: Halo CE (8200) on one
//! side, Halo 2 / Halo 3 and later (8205+) on the other.
//!
//! A [`JmsFile`] holds the same thing whichever builder made it — world-space
//! nodes and node-local markers in the H2/H3 quaternion convention, world
//! vertex positions, V already in file form, triangles wound to agree with
//! their normals — and each writer turns that into its version's layout. What
//! does not carry across is where a triangle's *permutation* lives, read off
//! the three MCC tool.exe JMS readers:
//!
//! - **Halo CE** (`sub_4372F0`, `sub_43BB70`) takes a permutation from the
//!   **file name** (`<permutation>[ <lod>].jms`), its regions from a REGIONS
//!   section and a per-triangle index, and its shader from the material
//!   name alone. Vertices carry two influences, `node0 = 1 − node1 weight`.
//! - **Halo 2 / 3** (`sub_46925A` / `sub_1400E5200`) take permutation and
//!   region from each material's `(slot) [lN] permutation region` line, in one
//!   file, and strip flag symbols off the shader name.
//!
//! So going to Halo CE splits one file into one per permutation, and coming
//! from it merges them back into labels.

use crate::jms::{JmsFile, JmsMaterial, JmsMarker, JmsTriangle, JmsVertex};
use crate::jms_split::{material_base_name, MaterialLabel};

/// Halo CE gbxmodel block limits (from the tool's definitions): beyond these
/// the import fails.
const HALO1_MAX_NODES: usize = 64;
const HALO1_MAX_REGIONS: usize = 32;
const HALO1_MAX_SHADERS: usize = 32;
const HALO1_MAX_PERMUTATIONS: usize = 32;

/// Halo 2 tool.exe reads a material's definition line into 32 bytes.
const HALO2_MAX_LABEL: usize = 31;

/// One Halo CE JMS: the file a permutation is imported from.
#[derive(Debug, Clone)]
pub struct Halo1Permutation {
    /// The permutation name, which is also the file stem. `base` is what
    /// Halo CE tool.exe turns into the tag's `__base`.
    pub name: String,
    pub jms: JmsFile,
}

impl Halo1Permutation {
    /// `<name>.jms`. A permutation the tag calls `__base` is written `base`,
    /// which the tool maps back.
    pub fn file_name(&self) -> String {
        format!("{}.jms", file_stem_for(&self.name))
    }
}

fn file_stem_for(permutation: &str) -> &str {
    match permutation {
        "" | "__base" => "base",
        other => other,
    }
}

/// A Halo 2 / 3 label token cannot hold whitespace: the tools split the line
/// on it. Halo CE names can, so it becomes `_`.
fn label_token(name: &str) -> String {
    let name = file_stem_for(name);
    name.split_whitespace().collect::<Vec<_>>().join("_")
}

impl JmsFile {
    /// Split an H2/H3-form JMS into one Halo CE file per permutation, plus
    /// warnings for whatever Halo CE has no room for.
    ///
    /// Each file keeps every node (Halo CE tool.exe requires identical node
    /// lists across a model's files), the triangles of its permutation with
    /// their regions in a REGIONS section, one material per distinct shader
    /// name with its flag symbols taken off, and every marker — an unscoped
    /// marker in each file, a `(permutation region)`-scoped one only in its
    /// permutation's, scope removed. Vertices keep their two heaviest
    /// influences, renormalised. Physics shapes have no Halo CE form and are
    /// dropped.
    pub fn split_for_halo1(&self) -> (Vec<Halo1Permutation>, Vec<String>) {
        let mut warnings = Vec::new();
        let labels: Vec<MaterialLabel> = self
            .materials
            .iter()
            .map(|m| MaterialLabel::parse(&m.material_name))
            .collect();

        // Permutations in first-seen order, matched case-insensitively.
        let mut permutations: Vec<String> = Vec::new();
        let mut triangle_permutation = Vec::with_capacity(self.triangles.len());
        for t in &self.triangles {
            let label = labels.get(t.material.max(0) as usize);
            let name = label.map_or("default", |l| l.permutation.as_str());
            let index = match permutations.iter().position(|p| p.eq_ignore_ascii_case(name)) {
                Some(index) => index,
                None => {
                    permutations.push(name.to_owned());
                    permutations.len() - 1
                }
            };
            triangle_permutation.push(index);
        }
        if permutations.is_empty() {
            permutations.push("base".to_owned());
        }

        let mut lossy_vertices = 0usize;
        let mut out = Vec::with_capacity(permutations.len());
        for (perm_index, perm_name) in permutations.iter().enumerate() {
            let mut jms = JmsFile { nodes: self.nodes.clone(), ..Default::default() };
            for (t, triangle) in self.triangles.iter().enumerate() {
                if triangle_permutation[t] != perm_index {
                    continue;
                }
                let material = self.materials.get(triangle.material.max(0) as usize);
                let label = labels.get(triangle.material.max(0) as usize);
                let shader = material.map_or("default", |m| material_base_name(&m.name));
                let shader = if shader.is_empty() { "default" } else { shader };
                let region = label.map_or("default", |l| l.region.as_str());

                let material_index = index_of(&mut jms.materials, shader, |m| &m.name, || JmsMaterial {
                    name: shader.to_owned(),
                    material_name: "<none>".to_owned(),
                });
                let region_index = match jms.regions.iter().position(|r| r.eq_ignore_ascii_case(region)) {
                    Some(index) => index,
                    None => {
                        jms.regions.push(region.to_owned());
                        jms.regions.len() - 1
                    }
                };
                let base = jms.vertices.len() as u32;
                for &v in &triangle.v {
                    let mut vertex = self.vertices[v as usize].clone();
                    if two_influences(&mut vertex) {
                        lossy_vertices += 1;
                    }
                    vertex.uvs.truncate(1);
                    vertex.color = None;
                    jms.vertices.push(vertex);
                }
                jms.triangles.push(JmsTriangle {
                    material: material_index as i32,
                    v: [base, base + 1, base + 2],
                    region: region_index as i32,
                });
            }
            for marker in &self.markers {
                let (scope, name) = split_marker_scope(&marker.name);
                if scope.is_some_and(|(permutation, _)| !permutation.eq_ignore_ascii_case(perm_name)) {
                    continue;
                }
                jms.markers.push(JmsMarker { name: name.to_owned(), ..marker.clone() });
            }
            if jms.regions.is_empty() {
                jms.regions.push("default".to_owned());
            }
            if jms.regions.len() > HALO1_MAX_REGIONS {
                warnings.push(format!(
                    "permutation '{perm_name}' has {} regions; Halo CE allows {HALO1_MAX_REGIONS}",
                    jms.regions.len()
                ));
            }
            if jms.materials.len() > HALO1_MAX_SHADERS {
                warnings.push(format!(
                    "permutation '{perm_name}' uses {} shaders; Halo CE allows {HALO1_MAX_SHADERS}",
                    jms.materials.len()
                ));
            }
            out.push(Halo1Permutation { name: perm_name.clone(), jms });
        }

        if self.nodes.len() > HALO1_MAX_NODES {
            warnings.push(format!(
                "{} nodes; Halo CE allows {HALO1_MAX_NODES}",
                self.nodes.len()
            ));
        }
        if out.len() > HALO1_MAX_PERMUTATIONS {
            warnings.push(format!(
                "{} permutations; Halo CE allows {HALO1_MAX_PERMUTATIONS}",
                out.len()
            ));
        }
        if lossy_vertices > 0 {
            warnings.push(format!(
                "{lossy_vertices} vertices had more than two influences; Halo CE keeps the two heaviest"
            ));
        }
        let physics = self.spheres.len()
            + self.boxes.len()
            + self.capsules.len()
            + self.convex_shapes.len()
            + self.ragdolls.len()
            + self.hinges.len();
        if physics > 0 {
            warnings.push(format!("{physics} physics shapes dropped; Halo CE JMS has none"));
        }
        (out, warnings)
    }

    /// Merge Halo CE permutation files into one H2/H3-form JMS: each
    /// triangle's permutation (its file) and region become its material's
    /// `(slot) permutation region` line. Nodes come from the first file;
    /// markers from all of them, once per name and node.
    pub fn merge_halo1_permutations(permutations: &[Halo1Permutation]) -> (JmsFile, Vec<String>) {
        let mut warnings = Vec::new();
        let mut jms = JmsFile {
            nodes: permutations.first().map(|p| p.jms.nodes.clone()).unwrap_or_default(),
            ..Default::default()
        };
        let mut keys: Vec<(String, String, String)> = Vec::new();
        for permutation in permutations {
            let perm_token = label_token(&permutation.name);
            let source = &permutation.jms;
            for triangle in &source.triangles {
                let shader = source
                    .materials
                    .get(triangle.material.max(0) as usize)
                    .map_or("default", |m| m.name.as_str());
                let region = source
                    .regions
                    .get(triangle.region.max(0) as usize)
                    .map_or("default", |r| r.as_str());
                let key = (shader.to_owned(), perm_token.clone(), label_token(region));
                let material_index = match keys.iter().position(|k| *k == key) {
                    Some(index) => index,
                    None => {
                        let slot = keys.len() + 1;
                        let label = format!("({slot}) {} {}", key.1, key.2);
                        if label.len() > HALO2_MAX_LABEL {
                            warnings.push(format!(
                                "material line '{label}' is longer than Halo 2's {HALO2_MAX_LABEL} characters"
                            ));
                        }
                        jms.materials.push(JmsMaterial { name: key.0.clone(), material_name: label });
                        keys.push(key);
                        keys.len() - 1
                    }
                };
                let base = jms.vertices.len() as u32;
                for &v in &triangle.v {
                    jms.vertices.push(source.vertices[v as usize].clone());
                }
                jms.triangles.push(JmsTriangle {
                    material: material_index as i32,
                    v: [base, base + 1, base + 2],
                    region: 0,
                });
            }
            for marker in &source.markers {
                if !jms.markers.iter().any(|m| m.name == marker.name && m.node_index == marker.node_index) {
                    jms.markers.push(marker.clone());
                }
            }
        }
        (jms, warnings)
    }
}

/// The index of the entry whose key equals `name` (case-insensitively),
/// pushing a new one when there is none.
fn index_of<T>(items: &mut Vec<T>, name: &str, key: impl Fn(&T) -> &str, make: impl FnOnce() -> T) -> usize {
    match items.iter().position(|item| key(item).eq_ignore_ascii_case(name)) {
        Some(index) => index,
        None => {
            items.push(make());
            items.len() - 1
        }
    }
}

/// Cut a vertex down to Halo CE's two influences: the two heaviest real
/// nodes, renormalised to sum to one (Halo CE derives `node0`'s weight as
/// `1 − node1`'s). Returns whether any weight was dropped.
fn two_influences(vertex: &mut JmsVertex) -> bool {
    let mut sets: Vec<(i16, f32)> = vertex.node_sets.iter().copied().filter(|&(node, weight)| node >= 0 && weight > 0.0).collect();
    sets.sort_by(|a, b| b.1.total_cmp(&a.1));
    let lossy = sets.len() > 2;
    sets.truncate(2);
    let total: f32 = sets.iter().map(|s| s.1).sum();
    if total > 0.0 {
        for set in &mut sets {
            set.1 /= total;
        }
    }
    vertex.node_sets = sets;
    lossy
}

/// `(permutation region)name` → `(Some((permutation, region)), name)`; an
/// unscoped name comes back whole. The grammar is H3 tool.exe's
/// (`sub_14010BAF0`).
fn split_marker_scope(name: &str) -> (Option<(&str, &str)>, &str) {
    let trimmed = name.trim_start();
    let Some(rest) = trimmed.strip_prefix('(') else {
        return (None, name);
    };
    let Some((inside, after)) = rest.split_once(')') else {
        return (None, name);
    };
    let mut tokens = inside.split_whitespace();
    let permutation = tokens.next().unwrap_or("");
    let region = tokens.next().unwrap_or("");
    (Some((permutation, region)), after.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jms::JmsNode;
    use crate::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};

    fn vertex(nodes: &[(i16, f32)]) -> JmsVertex {
        JmsVertex {
            position: RealPoint3d { x: 1.0, y: 2.0, z: 3.0 },
            normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
            tangent: None,
            binormal: None,
            node_sets: nodes.to_vec(),
            uvs: vec![RealPoint2d { x: 0.25, y: 0.75 }, RealPoint2d { x: 0.0, y: 0.0 }],
            color: Some(RealPoint3d { x: 1.0, y: 0.0, z: 0.0 }),
        }
    }

    fn modern() -> JmsFile {
        let node = |name: &str, parent| JmsNode {
            name: name.to_owned(),
            parent,
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::default(),
        };
        let marker = |name: &str| JmsMarker {
            name: name.to_owned(),
            node_index: 1,
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::default(),
            radius: 1.0,
        };
        let mut jms = JmsFile {
            nodes: vec![node("root", -1), node("hand", 0)],
            materials: vec![
                JmsMaterial { name: "metal%".to_owned(), material_name: "(1) base body".to_owned() },
                JmsMaterial { name: "metal".to_owned(), material_name: "(2) base head".to_owned() },
                JmsMaterial { name: "glass!".to_owned(), material_name: "(3) damaged body".to_owned() },
            ],
            markers: vec![marker("primary"), marker("(damaged body)smoke")],
            ..Default::default()
        };
        for material in 0..3 {
            let base = jms.vertices.len() as u32;
            for _ in 0..3 {
                jms.vertices.push(vertex(&[(0, 0.5), (1, 0.3), (0, 0.0), (1, 0.2)]));
            }
            jms.triangles.push(JmsTriangle { material, v: [base, base + 1, base + 2], region: 0 });
        }
        jms
    }

    #[test]
    fn splits_one_halo1_file_per_permutation() {
        let (files, warnings) = modern().split_for_halo1();
        let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["base", "damaged"]);
        let base = &files[0].jms;
        // `metal%` and `metal` are one shader once the flag is off.
        assert_eq!(base.materials.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["metal"]);
        assert_eq!(base.regions, ["body", "head"]);
        assert_eq!(base.triangles.iter().map(|t| t.region).collect::<Vec<_>>(), [0, 1]);
        // Unscoped marker in both files, the scoped one only in its own, unscoped.
        assert_eq!(base.markers.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["primary"]);
        let damaged = &files[1].jms;
        assert_eq!(damaged.markers.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["primary", "smoke"]);
        assert_eq!(damaged.materials[0].name, "glass");
        // Two influences, renormalised; one UV; no colour.
        let v = &base.vertices[0];
        assert_eq!(v.node_sets.len(), 2);
        assert!((v.node_sets[0].1 - 0.625).abs() < 1e-6 && (v.node_sets[1].1 - 0.375).abs() < 1e-6);
        assert_eq!(v.uvs.len(), 1);
        assert!(v.color.is_none());
        assert!(warnings.iter().any(|w| w.contains("two heaviest")), "{warnings:?}");
        assert_eq!(files[0].file_name(), "base.jms");
    }

    #[test]
    fn merging_halo1_files_restores_permutation_region_labels() {
        let (files, _) = modern().split_for_halo1();
        let (merged, warnings) = JmsFile::merge_halo1_permutations(&files);
        assert!(warnings.is_empty(), "{warnings:?}");
        let labels: Vec<_> = merged
            .materials
            .iter()
            .map(|m| {
                let label = MaterialLabel::parse(&m.material_name);
                (m.name.clone(), label.permutation, label.region)
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("metal".to_owned(), "base".to_owned(), "body".to_owned()),
                ("metal".to_owned(), "base".to_owned(), "head".to_owned()),
                ("glass".to_owned(), "damaged".to_owned(), "body".to_owned()),
            ]
        );
        assert_eq!(merged.triangles.len(), 3);
        assert_eq!(merged.nodes.len(), 2);
    }

    #[test]
    fn halo1_names_become_single_label_tokens() {
        let file = |name: &str, region: &str| Halo1Permutation {
            name: name.to_owned(),
            jms: JmsFile {
                materials: vec![JmsMaterial { name: "skin".to_owned(), material_name: "<none>".to_owned() }],
                regions: vec![region.to_owned()],
                vertices: vec![vertex(&[(0, 1.0)]); 3],
                triangles: vec![JmsTriangle { material: 0, v: [0, 1, 2], region: 0 }],
                ..Default::default()
            },
        };
        let (merged, _) = JmsFile::merge_halo1_permutations(&[file("__base", "left arm")]);
        assert_eq!(merged.materials[0].material_name, "(1) base left_arm");
    }

    #[test]
    fn marker_scope_follows_the_tool_grammar() {
        assert_eq!(split_marker_scope("(damaged body)smoke"), (Some(("damaged", "body")), "smoke"));
        assert_eq!(split_marker_scope("primary"), (None, "primary"));
        assert_eq!(split_marker_scope("(broken"), (None, "(broken"));
    }
}
