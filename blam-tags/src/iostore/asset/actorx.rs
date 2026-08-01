//! Minimal ActorX PSK/PSKX writer for decoded Unreal render meshes.
//!
//! The layout follows the public ActorX chunk format used by Unreal tooling.
//! PSK uses `FACE0000` (16-bit wedge indices); PSKX uses `FACE3200` so large
//! cooked meshes are never truncated.

use std::io::Write;

use anyhow::{Result, bail};

use super::skeletal_mesh::{SkelBone, SkeletalMesh};
use super::static_mesh::StaticMesh;
use crate::jms::{JmsFile, JmsMaterial, JmsNode, JmsTriangle, JmsVertex, ue_bind_world};
use crate::math::{Matrix4, RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};

const PSK_VERSION: i32 = 20_220_723;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActorXFormat {
    Psk,
    Pskx,
}

#[derive(Clone, Copy)]
struct Section {
    material: u8,
    first_index: usize,
    index_count: usize,
}

pub fn write_skeletal_mesh<W: Write>(
    mesh: &SkeletalMesh,
    material_names: &[String],
    format: ActorXFormat,
    writer: &mut W,
) -> Result<()> {
    let sections = mesh
        .sections
        .iter()
        .enumerate()
        .map(|(index, section)| Section {
            material: u8::try_from(index).unwrap_or(u8::MAX),
            first_index: section.base_index as usize,
            index_count: section.num_triangles as usize * 3,
        })
        .collect::<Vec<_>>();
    let materials = mesh
        .sections
        .iter()
        .map(|section| {
            material_names
                .get(section.material_index as usize)
                .cloned()
                .unwrap_or_else(|| format!("material_{}", section.material_index))
        })
        .collect::<Vec<_>>();
    let positions = mesh
        .vertices
        .iter()
        .map(|vertex| vertex.position)
        .collect::<Vec<_>>();
    let normals = mesh
        .vertices
        .iter()
        .map(|vertex| vertex.normal)
        .collect::<Vec<_>>();
    let uvs = mesh
        .vertices
        .iter()
        // The skeletal decoder exposes classic-tool UVs (V already flipped),
        // while ActorX stores the original Unreal convention.
        .map(|vertex| [vertex.uv[0], 1.0 - vertex.uv[1]])
        .collect::<Vec<_>>();
    write_common(
        writer,
        &positions,
        &normals,
        &uvs,
        &mesh.indices,
        &sections,
        &materials,
        &mesh.bones,
        Some(mesh),
        format,
    )
}

pub fn write_static_mesh<W: Write>(
    mesh: &StaticMesh,
    material_names: &[String],
    format: ActorXFormat,
    writer: &mut W,
) -> Result<()> {
    let positions = mesh
        .vertices
        .iter()
        .map(|vertex| vertex.position)
        .collect::<Vec<_>>();
    let normals = mesh
        .vertices
        .iter()
        .map(|vertex| vertex.normal)
        .collect::<Vec<_>>();
    let uvs = mesh
        .vertices
        .iter()
        // StaticVertex retains the raw Unreal UV convention.
        .map(|vertex| vertex.uv)
        .collect::<Vec<_>>();
    let sections = [Section {
        material: 0,
        first_index: 0,
        index_count: mesh.indices.len(),
    }];
    let materials = vec![
        material_names
            .first()
            .cloned()
            .unwrap_or_else(|| "material_0".to_owned()),
    ];
    write_common(
        writer,
        &positions,
        &normals,
        &uvs,
        &mesh.indices,
        &sections,
        &materials,
        &[],
        None,
        format,
    )
}

/// Convert a cooked Unreal skeletal mesh to a standalone modern JMS scene,
/// using the mesh's own reference skeleton and bind pose.
pub fn skeletal_mesh_to_jms(mesh: &SkeletalMesh, material_names: &[String]) -> JmsFile {
    const CM_TO_JMS: f32 = 100.0 / 304.8;
    let mirror = Matrix4 {
        m: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, -1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    let nodes = ue_bind_world(&mesh.bones)
        .iter()
        .zip(&mesh.bones)
        .map(|(world, bone)| {
            let (mut translation, rotation, _) = (mirror * *world * mirror).decompose();
            translation.x *= CM_TO_JMS;
            translation.y *= CM_TO_JMS;
            translation.z *= CM_TO_JMS;
            JmsNode {
                name: bone.name.clone(),
                parent: bone.parent.clamp(-1, i16::MAX as i32) as i16,
                rotation,
                translation,
            }
        })
        .collect();
    let mut material_indices = mesh
        .sections
        .iter()
        .map(|section| section.material_index)
        .collect::<Vec<_>>();
    material_indices.sort_unstable();
    material_indices.dedup();
    let materials = material_indices
        .iter()
        .enumerate()
        .map(|(slot, &index)| JmsMaterial {
            name: material_names
                .get(index as usize)
                .cloned()
                .unwrap_or_else(|| format!("material_{index}")),
            material_name: format!("({}) default mesh", slot + 1),
        })
        .collect::<Vec<_>>();
    let slots = material_indices
        .iter()
        .enumerate()
        .map(|(slot, &index)| (index, slot as i32))
        .collect::<std::collections::HashMap<_, _>>();
    let vertices = mesh
        .vertices
        .iter()
        .map(|vertex| JmsVertex {
            position: RealPoint3d {
                x: vertex.position[0] * CM_TO_JMS,
                y: -vertex.position[1] * CM_TO_JMS,
                z: vertex.position[2] * CM_TO_JMS,
            },
            normal: RealVector3d {
                i: vertex.normal[0],
                j: -vertex.normal[1],
                k: vertex.normal[2],
            },
            tangent: None,
            binormal: None,
            node_sets: vertex
                .influences
                .iter()
                .map(|influence| {
                    (
                        influence.bone.min(i16::MAX as u16) as i16,
                        influence.weight,
                    )
                })
                .collect(),
            uvs: vec![RealPoint2d {
                x: vertex.uv[0],
                y: vertex.uv[1],
            }],
        })
        .collect();
    let mut triangles = Vec::new();
    for section in &mesh.sections {
        let material = slots.get(&section.material_index).copied().unwrap_or(0);
        let start = section.base_index as usize;
        let end = start
            .saturating_add(section.num_triangles as usize * 3)
            .min(mesh.indices.len());
        for triangle in mesh.indices[start.min(mesh.indices.len())..end].chunks_exact(3) {
            if triangle[0] != triangle[1]
                && triangle[1] != triangle[2]
                && triangle[0] != triangle[2]
            {
                triangles.push(JmsTriangle {
                    material,
                    v: [triangle[0], triangle[1], triangle[2]],
                    region: 0,
                });
            }
        }
    }
    JmsFile {
        nodes,
        materials,
        vertices,
        triangles,
        ..Default::default()
    }
}

/// Convert a cooked Unreal static mesh to a rigid standalone modern JMS scene.
pub fn static_mesh_to_jms(mesh: &StaticMesh, material_names: &[String]) -> JmsFile {
    const CM_TO_JMS: f32 = 100.0 / 304.8;
    let nodes = vec![JmsNode {
        name: "root".to_owned(),
        parent: -1,
        rotation: RealQuaternion {
            i: 0.0,
            j: 0.0,
            k: 0.0,
            w: 1.0,
        },
        translation: RealPoint3d {
            x: 0.0,
            y: 0.0,
            z: 0.0,
        },
    }];
    let materials = vec![JmsMaterial {
        name: material_names
            .first()
            .cloned()
            .unwrap_or_else(|| "material_0".to_owned()),
        material_name: "(1) default mesh".to_owned(),
    }];
    let vertices = mesh
        .vertices
        .iter()
        .map(|vertex| JmsVertex {
            position: RealPoint3d {
                x: vertex.position[0] * CM_TO_JMS,
                y: -vertex.position[1] * CM_TO_JMS,
                z: vertex.position[2] * CM_TO_JMS,
            },
            normal: RealVector3d {
                i: vertex.normal[0],
                j: -vertex.normal[1],
                k: vertex.normal[2],
            },
            tangent: None,
            binormal: None,
            node_sets: vec![(0, 1.0)],
            uvs: vec![RealPoint2d {
                x: vertex.uv[0],
                y: 1.0 - vertex.uv[1],
            }],
        })
        .collect();
    let triangles = mesh
        .indices
        .chunks_exact(3)
        .filter(|triangle| {
            triangle[0] != triangle[1]
                && triangle[1] != triangle[2]
                && triangle[0] != triangle[2]
        })
        .map(|triangle| JmsTriangle {
            material: 0,
            v: [triangle[0], triangle[1], triangle[2]],
            region: 0,
        })
        .collect();
    JmsFile {
        nodes,
        materials,
        vertices,
        triangles,
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
fn write_common<W: Write>(
    writer: &mut W,
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    uvs: &[[f32; 2]],
    indices: &[u32],
    sections: &[Section],
    material_names: &[String],
    bones: &[SkelBone],
    skeletal: Option<&SkeletalMesh>,
    format: ActorXFormat,
) -> Result<()> {
    if positions.len() != normals.len() || positions.len() != uvs.len() {
        bail!("ActorX mesh vertex streams have different lengths");
    }
    if format == ActorXFormat::Psk && positions.len() > u16::MAX as usize + 1 {
        bail!(
            "mesh has {} wedges; PSK supports at most 65536, choose PSKX",
            positions.len()
        );
    }
    if sections.len() > u8::MAX as usize + 1 {
        bail!(
            "mesh has too many material sections for ActorX ({})",
            sections.len()
        );
    }

    chunk(writer, "ACTRHEAD", PSK_VERSION, 0, 0)?;
    chunk(writer, "PNTS0000", 0, 12, positions.len())?;
    for [x, y, z] in positions {
        floats(writer, &[*x, -*y, *z])?;
    }

    let mut wedge_material = vec![0u8; positions.len()];
    for section in sections {
        let end = section
            .first_index
            .saturating_add(section.index_count)
            .min(indices.len());
        for &index in &indices[section.first_index.min(indices.len())..end] {
            if let Some(material) = wedge_material.get_mut(index as usize) {
                *material = section.material;
            }
        }
    }
    chunk(writer, "VTXW0000", 0, 16, positions.len())?;
    for (index, uv) in uvs.iter().enumerate() {
        writer.write_all(&(index as i32).to_le_bytes())?;
        floats(writer, uv)?;
        writer.write_all(&[wedge_material[index], 0])?;
        writer.write_all(&0i16.to_le_bytes())?;
    }

    let faces = sections
        .iter()
        .flat_map(|section| {
            let end = section
                .first_index
                .saturating_add(section.index_count)
                .min(indices.len());
            indices[section.first_index.min(indices.len())..end]
                .chunks_exact(3)
                .filter(move |face| {
                    face.iter().all(|&index| (index as usize) < positions.len())
                        && face[0] != face[1]
                        && face[1] != face[2]
                        && face[0] != face[2]
                })
                .map(move |face| ([face[1], face[0], face[2]], section.material))
        })
        .collect::<Vec<_>>();
    match format {
        ActorXFormat::Psk => {
            chunk(writer, "FACE0000", 0, 12, faces.len())?;
            for (face, material) in &faces {
                for index in face {
                    writer.write_all(&(*index as u16).to_le_bytes())?;
                }
                writer.write_all(&[*material, 0])?;
                writer.write_all(&1u32.to_le_bytes())?;
            }
        }
        ActorXFormat::Pskx => {
            chunk(writer, "FACE3200", 0, 18, faces.len())?;
            for (face, material) in &faces {
                for index in face {
                    writer.write_all(&index.to_le_bytes())?;
                }
                writer.write_all(&[*material, 0])?;
                writer.write_all(&1u32.to_le_bytes())?;
            }
        }
    }

    chunk(writer, "MATT0000", 0, 88, material_names.len())?;
    for (index, name) in material_names.iter().enumerate() {
        fixed_string(writer, name, 64)?;
        writer.write_all(&(index as i32).to_le_bytes())?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(&0i32.to_le_bytes())?;
        writer.write_all(&0u32.to_le_bytes())?;
        writer.write_all(&0i32.to_le_bytes())?;
        writer.write_all(&0i32.to_le_bytes())?;
    }

    chunk(writer, "VTXNORMS", 0, 12, normals.len())?;
    for normal in normals {
        let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2])
            .sqrt()
            .max(1.0e-8);
        floats(
            writer,
            &[normal[0] / length, -normal[1] / length, normal[2] / length],
        )?;
    }

    chunk(writer, "REFSKELT", 0, 120, bones.len())?;
    for (index, bone) in bones.iter().enumerate() {
        fixed_string(writer, &bone.name, 64)?;
        writer.write_all(&0u32.to_le_bytes())?;
        let children = bones
            .iter()
            .filter(|candidate| candidate.parent == index as i32)
            .count();
        writer.write_all(&(children as i32).to_le_bytes())?;
        writer.write_all(&bone.parent.to_le_bytes())?;
        let mut rotation = bone.rest_rotation;
        rotation[1] = -rotation[1];
        if index == 0 {
            rotation[3] = -rotation[3];
        }
        floats(writer, &rotation)?;
        floats(
            writer,
            &[
                bone.rest_translation[0],
                -bone.rest_translation[1],
                bone.rest_translation[2],
            ],
        )?;
        floats(writer, &[1.0, 1.0, 1.0, 1.0])?;
    }

    let influence_count = skeletal
        .map(|mesh| {
            mesh.vertices
                .iter()
                .map(|vertex| vertex.influences.len())
                .sum()
        })
        .unwrap_or(0);
    chunk(writer, "RAWWEIGHTS", 0, 12, influence_count)?;
    if let Some(mesh) = skeletal {
        for (point, vertex) in mesh.vertices.iter().enumerate() {
            for influence in &vertex.influences {
                writer.write_all(&influence.weight.to_le_bytes())?;
                writer.write_all(&(point as i32).to_le_bytes())?;
                writer.write_all(&(influence.bone as i32).to_le_bytes())?;
            }
        }
    }
    Ok(())
}

fn chunk<W: Write>(
    writer: &mut W,
    id: &str,
    type_flag: i32,
    size: usize,
    count: usize,
) -> Result<()> {
    fixed_string(writer, id, 20)?;
    writer.write_all(&type_flag.to_le_bytes())?;
    writer.write_all(&(size as i32).to_le_bytes())?;
    writer.write_all(&(count as i32).to_le_bytes())?;
    Ok(())
}

fn fixed_string<W: Write>(writer: &mut W, value: &str, length: usize) -> Result<()> {
    let mut bytes = vec![0u8; length];
    let source = value.as_bytes();
    let count = source.len().min(length.saturating_sub(1));
    bytes[..count].copy_from_slice(&source[..count]);
    writer.write_all(&bytes)?;
    Ok(())
}

fn floats<W: Write>(writer: &mut W, values: &[f32]) -> Result<()> {
    for value in values {
        writer.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iostore::skeletal_mesh::{Influence, SkelSection, SkelVertex};

    fn sample() -> SkeletalMesh {
        SkeletalMesh {
            bones: vec![SkelBone {
                name: "root".to_owned(),
                parent: -1,
                rest_rotation: [0.0, 0.0, 0.0, 1.0],
                rest_translation: [0.0; 3],
            }],
            sections: vec![SkelSection {
                material_index: 0,
                base_index: 0,
                num_triangles: 1,
                base_vertex: 0,
                num_vertices: 3,
                bone_map: vec![0],
            }],
            indices: vec![0, 1, 2],
            vertices: (0..3)
                .map(|index| SkelVertex {
                    position: [index as f32, 0.0, 0.0],
                    normal: [0.0, 0.0, 1.0],
                    uv: [0.0; 2],
                    influences: vec![Influence {
                        bone: 0,
                        weight: 1.0,
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn psk_and_pskx_choose_the_expected_face_chunk() {
        for (format, expected) in [
            (ActorXFormat::Psk, b"FACE0000"),
            (ActorXFormat::Pskx, b"FACE3200"),
        ] {
            let mut bytes = Vec::new();
            write_skeletal_mesh(&sample(), &[], format, &mut bytes).unwrap();
            assert!(
                bytes
                    .windows(expected.len())
                    .any(|window| window == expected)
            );
            assert!(bytes.windows(10).any(|window| window == b"RAWWEIGHTS"));
        }
    }
}
