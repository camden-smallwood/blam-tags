//! Reader for the per-instance transforms a cooked
//! `UInstancedStaticMeshComponent` carries after its property block.
//!
//! An instanced component names its mesh through an ordinary reflected
//! property, but its placements are not reflected at all: they are written by
//! the class's own `Serialize` into the bytes that follow, which the property
//! decoder hands back as the export's tail.
//!
//! The layout was established from Campaign Evolved's own data rather than from
//! engine source, so it is anchored on the one thing that can be checked: the
//! array announces itself as `[Stride=128][Count]`, and 128 bytes is an
//! `FMatrix` of sixteen doubles. Measured over 403 components of C10, 400 parse
//! and every one of the 622 matrices sampled is exactly affine — a last column
//! of `(0, 0, 0, 1)` to within 1e-6. Bytes that happened to be something else
//! would not land on that, which is why [`read_instance_transforms`] enforces
//! it: it is the only structural check available, so a tail that fails it is
//! reporting that this reading is wrong, not that the data is unusual.
//!
//! Anything after the matrices — per-instance custom floats and lightmap data —
//! is not geometry and is skipped. It is commonly 12 or 28 bytes and
//! occasionally several kilobytes.

use anyhow::{Result, bail};

/// Where the `[Stride][Count]` header sits in the tail.
const ARRAY_HEADER_OFFSET: usize = 24;
/// `FMatrix`: sixteen doubles, in the large-world-coordinate build.
const MATRIX_STRIDE: usize = 128;
/// How far a matrix's last column may drift from `(0, 0, 0, 1)`.
const AFFINE_EPSILON: f64 = 1e-6;

/// One instance's placement: an `FMatrix` in row-major order, so `[12..15]` is
/// the translation.
pub type InstanceTransform = [f64; 16];

/// Read every instance placement out of an instanced component's export tail.
///
/// Returns an empty vector for a component that declares no instances. Errors
/// describe what did not match, because the caller's only sensible response is
/// to skip that component and say so rather than emit geometry at the origin.
pub fn read_instance_transforms(tail: &[u8]) -> Result<Vec<InstanceTransform>> {
    let header_end = ARRAY_HEADER_OFFSET + 8;
    if tail.len() < header_end {
        bail!(
            "instanced component tail is {} bytes, too short to hold an instance array header",
            tail.len()
        );
    }
    let stride = i32::from_le_bytes(tail[ARRAY_HEADER_OFFSET..ARRAY_HEADER_OFFSET + 4].try_into()?);
    let count = i32::from_le_bytes(tail[ARRAY_HEADER_OFFSET + 4..header_end].try_into()?);
    if stride as usize != MATRIX_STRIDE {
        bail!("instance array declares a stride of {stride}, not {MATRIX_STRIDE}");
    }
    if count < 0 {
        bail!("instance array declares {count} instances");
    }
    let count = count as usize;
    let end = header_end
        .checked_add(count.checked_mul(MATRIX_STRIDE).unwrap_or(usize::MAX))
        .unwrap_or(usize::MAX);
    if end > tail.len() {
        bail!(
            "instance array wants {count} transforms ({} bytes) but the tail holds {}",
            end - header_end,
            tail.len() - header_end
        );
    }

    let mut transforms = Vec::with_capacity(count);
    for index in 0..count {
        let at = header_end + index * MATRIX_STRIDE;
        let mut matrix = [0f64; 16];
        for (element, slot) in matrix.iter_mut().enumerate() {
            let start = at + element * 8;
            *slot = f64::from_le_bytes(tail[start..start + 8].try_into()?);
        }
        if !is_affine(&matrix) {
            bail!(
                "instance {index} is not an affine transform; this tail is not an instance array"
            );
        }
        transforms.push(matrix);
    }
    Ok(transforms)
}

/// Whether a matrix's last column is `(0, 0, 0, 1)`, as every placement's is.
fn is_affine(matrix: &InstanceTransform) -> bool {
    matrix[3].abs() < AFFINE_EPSILON
        && matrix[7].abs() < AFFINE_EPSILON
        && matrix[11].abs() < AFFINE_EPSILON
        && (matrix[15] - 1.0).abs() < AFFINE_EPSILON
}

/// The world position an instance transform places its mesh at.
pub fn instance_translation(matrix: &InstanceTransform) -> [f64; 3] {
    [matrix[12], matrix[13], matrix[14]]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tail_with(matrices: &[InstanceTransform], trailer: &[u8]) -> Vec<u8> {
        let mut tail = vec![0u8; ARRAY_HEADER_OFFSET];
        tail.extend_from_slice(&(MATRIX_STRIDE as i32).to_le_bytes());
        tail.extend_from_slice(&(matrices.len() as i32).to_le_bytes());
        for matrix in matrices {
            for element in matrix {
                tail.extend_from_slice(&element.to_le_bytes());
            }
        }
        tail.extend_from_slice(trailer);
        tail
    }

    fn placed_at(x: f64, y: f64, z: f64) -> InstanceTransform {
        let mut matrix = [0f64; 16];
        matrix[0] = 1.0;
        matrix[5] = 1.0;
        matrix[10] = 1.0;
        matrix[15] = 1.0;
        matrix[12] = x;
        matrix[13] = y;
        matrix[14] = z;
        matrix
    }

    #[test]
    fn instances_are_read_and_the_trailer_is_ignored() {
        let placements = [
            placed_at(-16036.1, -25553.6, 13539.7),
            placed_at(-62640.3, -2783.5, 19900.4),
        ];
        // Custom-instance data follows the matrices; it is not geometry, and
        // its size varies from a few bytes to a few kilobytes.
        for trailer in [vec![], vec![0u8; 12], vec![7u8; 28], vec![3u8; 4140]] {
            let read = read_instance_transforms(&tail_with(&placements, &trailer)).unwrap();
            assert_eq!(read.len(), 2);
            assert_eq!(instance_translation(&read[0]), [-16036.1, -25553.6, 13539.7]);
            assert_eq!(instance_translation(&read[1]), [-62640.3, -2783.5, 19900.4]);
        }
    }

    #[test]
    fn a_component_with_no_instances_reads_as_empty() {
        let read = read_instance_transforms(&tail_with(&[], &[0u8; 12])).unwrap();
        assert!(read.is_empty());
    }

    #[test]
    fn a_tail_that_is_not_an_instance_array_is_refused() {
        // Too short to hold the header at all.
        assert!(read_instance_transforms(&[0u8; 16]).is_err());

        // A stride that is not an FMatrix: this tail is something else.
        let mut wrong_stride = tail_with(&[placed_at(1.0, 2.0, 3.0)], &[]);
        wrong_stride[ARRAY_HEADER_OFFSET..ARRAY_HEADER_OFFSET + 4]
            .copy_from_slice(&64i32.to_le_bytes());
        assert!(read_instance_transforms(&wrong_stride).is_err());

        // A count the tail cannot possibly hold.
        let mut overlong = tail_with(&[placed_at(1.0, 2.0, 3.0)], &[]);
        overlong[ARRAY_HEADER_OFFSET + 4..ARRAY_HEADER_OFFSET + 8]
            .copy_from_slice(&4096i32.to_le_bytes());
        assert!(read_instance_transforms(&overlong).is_err());

        let mut negative = tail_with(&[], &[]);
        negative[ARRAY_HEADER_OFFSET + 4..ARRAY_HEADER_OFFSET + 8]
            .copy_from_slice(&(-3i32).to_le_bytes());
        assert!(read_instance_transforms(&negative).is_err());
    }

    #[test]
    fn a_non_affine_matrix_means_the_layout_is_wrong() {
        // The affine check is the only evidence that these bytes really are
        // transforms, so failing it has to be an error rather than a placement
        // at some arbitrary point in the level.
        let mut projective = placed_at(10.0, 20.0, 30.0);
        projective[3] = 0.5;
        assert!(read_instance_transforms(&tail_with(&[projective], &[])).is_err());

        let mut unscaled_w = placed_at(10.0, 20.0, 30.0);
        unscaled_w[15] = 2.0;
        assert!(read_instance_transforms(&tail_with(&[unscaled_w], &[])).is_err());
    }

    #[test]
    fn a_rotated_and_scaled_placement_survives_the_round_trip() {
        // A real placement is rarely axis-aligned; the affine gate must not
        // reject an ordinary rotation with scale baked into the basis.
        let matrix = [
            0.0, 3.0, 0.0, 0.0, //
            -3.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 3.0, 0.0, //
            -38869.7, 19286.6, 12838.1, 1.0,
        ];
        let read = read_instance_transforms(&tail_with(&[matrix], &[0u8; 28])).unwrap();
        assert_eq!(read[0], matrix);
        assert_eq!(instance_translation(&read[0]), [-38869.7, 19286.6, 12838.1]);
    }
}
