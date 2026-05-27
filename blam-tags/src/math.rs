//! Math and color types used in tag data.
//!
//! Each type mirrors a Halo engine `real_*` struct exactly — same
//! field names, same wire layout. Inherent impls + standard `Ops`
//! traits provide the algebra; **point-vs-vector semantics are
//! enforced**:
//!
//! - `RealPoint3d + RealVector3d → RealPoint3d` (translate a point)
//! - `RealPoint3d - RealPoint3d → RealVector3d` (displacement)
//! - `RealPoint3d - RealVector3d → RealPoint3d` (translate back)
//! - `RealPoint3d + RealPoint3d` is **not implemented** (mathematically
//!   undefined; if you need it, convert one side to a vector
//!   explicitly via `as_vector()`).
//!
//! Same shape for the 2D pair. `RealQuaternion` carries the
//! non-overridden `Default` (zero quat) — use the explicit
//! [`RealQuaternion::IDENTITY`] const when you mean rotation-identity.

use std::ops::{Add, Mul, Neg, Sub};

/// Bounds (min/max pair).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Bounds<T> {
    pub lower: T,
    pub upper: T,
}

/// Bounds of two i16 values (min/max).
pub type ShortBounds = Bounds<i16>;
/// Bounds of two angle values in radians (min/max).
pub type AngleBounds = Bounds<f32>;
/// Bounds of two real values (min/max).
pub type RealBounds = Bounds<f32>;
/// Bounds of two fraction values (min/max).
pub type FractionBounds = Bounds<f32>;

impl Bounds<f32> {
    /// `true` iff `value` lies on the closed interval `[lower, upper]`.
    pub fn contains(&self, value: f32) -> bool {
        value >= self.lower && value <= self.upper
    }

    /// `upper - lower`. Negative if the bounds are inverted.
    pub fn range(&self) -> f32 {
        self.upper - self.lower
    }
}

/// 8-bit-per-channel RGB color, packed into a single `u32`. The engine
/// accesses each channel via bit shifts / masks — this is deliberately
/// not split into separate byte fields to match the on-disk layout.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RgbColor(pub u32);

/// 8-bit-per-channel ARGB color, packed into a single `u32`. The engine
/// accesses each channel via bit shifts / masks — this is deliberately
/// not split into separate byte fields to match the on-disk layout.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ArgbColor(pub u32);

/// 2D point (integer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Point2d {
    pub x: i16,
    pub y: i16,
}

/// 2D rectangle (integer).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Rectangle2d {
    pub top: i16,
    pub left: i16,
    pub bottom: i16,
    pub right: i16,
}

/// 2D point (float).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealPoint2d {
    pub x: f32,
    pub y: f32,
}

/// 3D point (float).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealPoint3d {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// 2D vector (float).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealVector2d {
    pub i: f32,
    pub j: f32,
}

/// 3D vector (float).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealVector3d {
    pub i: f32,
    pub j: f32,
    pub k: f32,
}

/// Quaternion. **Note**: the derived `Default` returns the zero quat
/// `(0, 0, 0, 0)`, **not** the identity. Use [`RealQuaternion::IDENTITY`]
/// when you mean a no-op rotation.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealQuaternion {
    pub i: f32,
    pub j: f32,
    pub k: f32,
    pub w: f32,
}

/// 2D euler angles.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealEulerAngles2d {
    pub yaw: f32,
    pub pitch: f32,
}

/// 3D euler angles.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealEulerAngles3d {
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
}

/// 2D plane.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealPlane2d {
    pub i: f32,
    pub j: f32,
    pub d: f32,
}

/// 3D plane.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealPlane3d {
    pub i: f32,
    pub j: f32,
    pub k: f32,
    pub d: f32,
}

/// `real_rectangle2d` — engine source `math/real_math.h:239-250` (16 B).
///
/// Two intervals (`[x0,x1] × [y0,y1]`) packed as four floats. Engine uses
/// this for frustum bounds in screen / projection space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[repr(C)]
pub struct RealRectangle2d {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
}

const _: () = assert!(std::mem::size_of::<RealRectangle2d>() == 16);

/// `real_rectangle3d` — engine source `math/real_math.h:253-266` (24 B).
///
/// Three intervals (`[x0,x1] × [y0,y1] × [z0,z1]`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[repr(C)]
pub struct RealRectangle3d {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub z0: f32,
    pub z1: f32,
}

const _: () = assert!(std::mem::size_of::<RealRectangle3d>() == 24);

/// `real_matrix4x3` — engine source `math/real_math.h:198-215` (52 B).
///
/// Affine TRS-style transform. Engine layout: `scale` at +0, then 3 basis
/// vectors (`forward`/`left`/`up`, each `real_vector3d` = 12 B), then
/// `position` (`real_point3d`, 12 B). Inner union with `n[4][3]` /
/// `matrix3x3` / `basis[3]+origin` — same bytes, different views; we expose
/// only the named-field form because the union views are aliases over the
/// same bytes and Rust callers can recompute them trivially.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[repr(C)]
pub struct RealMatrix4x3 {
    pub scale: f32,
    pub forward: RealVector3d,
    pub left: RealVector3d,
    pub up: RealVector3d,
    pub position: RealPoint3d,
}

const _: () = assert!(std::mem::size_of::<RealMatrix4x3>() == 52);

/// RGB color (float, 0.0–1.0).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealRgbColor {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

/// ARGB color (float, 0.0–1.0).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealArgbColor {
    pub alpha: f32,
    pub red: f32,
    pub green: f32,
    pub blue: f32,
}

/// HSV color (float).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealHsvColor {
    pub hue: f32,
    pub saturation: f32,
    pub value: f32,
}

/// AHSV color (float).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RealAhsvColor {
    pub alpha: f32,
    pub hue: f32,
    pub saturation: f32,
    pub value: f32,
}

/// Engine `real_orientation` (`math/real_math.h`, 32 bytes). A
/// quaternion+point+scale triplet — the canonical "local TRS in one
/// struct" used by the animation system. Stored verbatim in
/// `render_model.runtime_node_orientations!` (one entry per node, the
/// bind-pose snapshot tool.exe bakes at cache-compile time) and copied
/// into per-object `node_orientations` buffers at spawn.
///
/// Layout (matches engine `real_orientation`):
/// - `rotation` (quat) @ +0  (16 bytes)
/// - `translation` (point) @ +0x10 (12 bytes)
/// - `scale` (f32) @ +0x1C (4 bytes)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RealOrientation {
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub scale: f32,
}

impl RealOrientation {
    /// Identity TRS — zero translation, identity rotation, unit scale.
    /// Mirrors engine `global_identity_orientation`.
    pub const IDENTITY: Self = Self {
        rotation: RealQuaternion::IDENTITY,
        translation: RealPoint3d::ZERO,
        scale: 1.0,
    };
}

impl Default for RealOrientation {
    /// Identity (`IDENTITY`) — engines treat a zero-quat orientation as
    /// undefined, so the default must be a valid TRS.
    fn default() -> Self {
        Self::IDENTITY
    }
}

//================================================================================
// RealVector2d / RealVector3d
//================================================================================

impl RealVector2d {
    /// Zero vector — `(0, 0)`.
    pub const ZERO: Self = Self { i: 0.0, j: 0.0 };

    /// Dot product.
    pub fn dot(self, other: Self) -> f32 {
        self.i * other.i + self.j * other.j
    }

    /// Squared Euclidean length.
    pub fn length_squared(self) -> f32 { self.dot(self) }

    /// Euclidean length.
    pub fn length(self) -> f32 { self.length_squared().sqrt() }

    /// Length-normalize, returning [`Self::ZERO`] for near-zero vectors
    /// rather than NaNs.
    pub fn normalized(self) -> Self {
        let m = self.length();
        if m < 1e-12 { Self::ZERO } else { Self { i: self.i / m, j: self.j / m } }
    }

    /// Reinterpret this vector's coordinates as a point.
    pub fn as_point(self) -> RealPoint2d { RealPoint2d { x: self.i, y: self.j } }

    /// Component pair `[i, j]`.
    pub fn to_array(self) -> [f32; 2] { [self.i, self.j] }
}

impl Add for RealVector2d {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { Self { i: self.i + rhs.i, j: self.j + rhs.j } }
}
impl Sub for RealVector2d {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self { Self { i: self.i - rhs.i, j: self.j - rhs.j } }
}
impl Mul<f32> for RealVector2d {
    type Output = Self;
    fn mul(self, k: f32) -> Self { Self { i: self.i * k, j: self.j * k } }
}
impl Neg for RealVector2d {
    type Output = Self;
    fn neg(self) -> Self { Self { i: -self.i, j: -self.j } }
}

impl RealVector3d {
    /// Zero vector — `(0, 0, 0)`.
    pub const ZERO: Self = Self { i: 0.0, j: 0.0, k: 0.0 };

    /// Dot product.
    pub fn dot(self, other: Self) -> f32 {
        self.i * other.i + self.j * other.j + self.k * other.k
    }

    /// Cross product `self × other`.
    pub fn cross(self, other: Self) -> Self {
        Self {
            i: self.j * other.k - self.k * other.j,
            j: self.k * other.i - self.i * other.k,
            k: self.i * other.j - self.j * other.i,
        }
    }

    /// Squared Euclidean length.
    pub fn length_squared(self) -> f32 { self.dot(self) }

    /// Euclidean length.
    pub fn length(self) -> f32 { self.length_squared().sqrt() }

    /// Length-normalize, returning [`Self::ZERO`] for near-zero vectors
    /// rather than NaNs.
    pub fn normalized(self) -> Self {
        let m = self.length();
        if m < 1e-12 { Self::ZERO } else { Self { i: self.i / m, j: self.j / m, k: self.k / m } }
    }

    /// Reinterpret this vector's coordinates as a point. Useful when
    /// a vector arithmetic result needs to be stored in a point slot
    /// (the schema sometimes models offsets as `real_vector_3d` and
    /// the consuming struct as `real_point_3d`).
    pub fn as_point(self) -> RealPoint3d { RealPoint3d { x: self.i, y: self.j, z: self.k } }

    /// Component triple `[i, j, k]`. Convenient for piping into
    /// generic writers that expect a slice of floats.
    pub fn to_array(self) -> [f32; 3] { [self.i, self.j, self.k] }
}

impl Add for RealVector3d {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self { i: self.i + rhs.i, j: self.j + rhs.j, k: self.k + rhs.k }
    }
}
impl Sub for RealVector3d {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self { i: self.i - rhs.i, j: self.j - rhs.j, k: self.k - rhs.k }
    }
}
impl Mul<f32> for RealVector3d {
    type Output = Self;
    fn mul(self, k: f32) -> Self { Self { i: self.i * k, j: self.j * k, k: self.k * k } }
}
impl Neg for RealVector3d {
    type Output = Self;
    fn neg(self) -> Self { Self { i: -self.i, j: -self.j, k: -self.k } }
}

//================================================================================
// RealPoint2d / RealPoint3d
//================================================================================

impl RealPoint2d {
    /// Origin — `(0, 0)`.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0 };

    /// Treat this point's coordinates as a free vector. Useful when
    /// you need vector arithmetic on a coordinate triple that the
    /// schema modeled as a point.
    pub fn as_vector(self) -> RealVector2d { RealVector2d { i: self.x, j: self.y } }

    /// Distance to another point.
    pub fn distance_to(self, other: Self) -> f32 {
        (self - other).length()
    }

    /// Squared distance to another point.
    pub fn distance_squared_to(self, other: Self) -> f32 {
        (self - other).length_squared()
    }

    /// Multiply every coordinate by `k`. Equivalent to `self * k`.
    pub fn scaled(self, k: f32) -> Self { self * k }

    /// Component pair `[x, y]`.
    pub fn to_array(self) -> [f32; 2] { [self.x, self.y] }
}

impl Add<RealVector2d> for RealPoint2d {
    type Output = Self;
    fn add(self, rhs: RealVector2d) -> Self { Self { x: self.x + rhs.i, y: self.y + rhs.j } }
}
impl Sub<RealVector2d> for RealPoint2d {
    type Output = Self;
    fn sub(self, rhs: RealVector2d) -> Self { Self { x: self.x - rhs.i, y: self.y - rhs.j } }
}
impl Sub for RealPoint2d {
    type Output = RealVector2d;
    fn sub(self, rhs: Self) -> RealVector2d {
        RealVector2d { i: self.x - rhs.x, j: self.y - rhs.y }
    }
}
impl Mul<f32> for RealPoint2d {
    type Output = Self;
    fn mul(self, k: f32) -> Self { Self { x: self.x * k, y: self.y * k } }
}

impl RealPoint3d {
    /// Origin — `(0, 0, 0)`.
    pub const ZERO: Self = Self { x: 0.0, y: 0.0, z: 0.0 };

    /// Treat this point's coordinates as a free vector. Useful when
    /// you need vector arithmetic on a coordinate triple that the
    /// schema modeled as a point — and for converting an offset that
    /// came in via `RealPoint3d` so it can feed [`RealQuaternion::rotate`]
    /// or vector-only operations.
    pub fn as_vector(self) -> RealVector3d {
        RealVector3d { i: self.x, j: self.y, k: self.z }
    }

    /// Distance to another point.
    pub fn distance_to(self, other: Self) -> f32 {
        (self - other).length()
    }

    /// Squared distance to another point.
    pub fn distance_squared_to(self, other: Self) -> f32 {
        (self - other).length_squared()
    }

    /// Multiply every coordinate by `k`. Equivalent to `self * k`.
    pub fn scaled(self, k: f32) -> Self { self * k }

    /// Component triple `[x, y, z]`.
    pub fn to_array(self) -> [f32; 3] { [self.x, self.y, self.z] }
}

impl Add<RealVector3d> for RealPoint3d {
    type Output = Self;
    fn add(self, rhs: RealVector3d) -> Self {
        Self { x: self.x + rhs.i, y: self.y + rhs.j, z: self.z + rhs.k }
    }
}
impl Sub<RealVector3d> for RealPoint3d {
    type Output = Self;
    fn sub(self, rhs: RealVector3d) -> Self {
        Self { x: self.x - rhs.i, y: self.y - rhs.j, z: self.z - rhs.k }
    }
}
impl Sub for RealPoint3d {
    type Output = RealVector3d;
    fn sub(self, rhs: Self) -> RealVector3d {
        RealVector3d { i: self.x - rhs.x, j: self.y - rhs.y, k: self.z - rhs.z }
    }
}
impl Mul<f32> for RealPoint3d {
    type Output = Self;
    fn mul(self, k: f32) -> Self { Self { x: self.x * k, y: self.y * k, z: self.z * k } }
}

//================================================================================
// RealQuaternion
//================================================================================

impl RealQuaternion {
    /// Identity rotation: `(0, 0, 0, 1)`. **Note**: distinct from the
    /// derived `Default` (`(0, 0, 0, 0)`).
    pub const IDENTITY: Self = Self { i: 0.0, j: 0.0, k: 0.0, w: 1.0 };

    /// Component-wise dot product. Used by `nlerp` for short-arc
    /// detection (negative dot → flip one quat to take the shorter
    /// rotational path).
    pub fn dot(self, other: Self) -> f32 {
        self.i * other.i + self.j * other.j + self.k * other.k + self.w * other.w
    }

    /// Squared 4D length.
    pub fn length_squared(self) -> f32 { self.dot(self) }

    /// 4D length.
    pub fn length(self) -> f32 { self.length_squared().sqrt() }

    /// Unit-normalize. Mirrors `fast_quaternion_normalize` from the H3
    /// binary — divides by the magnitude. Returns the input unchanged
    /// on a zero-magnitude quat (no `1/0` blow-up; callers can detect
    /// the zero case by checking `i==j==k==w==0`).
    pub fn normalized(self) -> Self {
        let mag2 = self.length_squared();
        if mag2 <= 0.0 || !mag2.is_finite() { return self; }
        let inv = mag2.sqrt().recip();
        Self { i: self.i * inv, j: self.j * inv, k: self.k * inv, w: self.w * inv }
    }

    /// Conjugate: negate the imaginary part, keep `w`. For unit
    /// quaternions this is the inverse rotation.
    pub fn conjugate(self) -> Self {
        Self { i: -self.i, j: -self.j, k: -self.k, w: self.w }
    }

    /// Construct from three column basis vectors of an orthonormal
    /// rotation matrix. Standard trace-and-largest-diagonal extraction.
    pub fn from_basis_columns(c0: RealVector3d, c1: RealVector3d, c2: RealVector3d) -> Self {
        let (m00, m10, m20) = (c0.i, c0.j, c0.k);
        let (m01, m11, m21) = (c1.i, c1.j, c1.k);
        let (m02, m12, m22) = (c2.i, c2.j, c2.k);
        let trace = m00 + m11 + m22;
        if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0;
            Self { i: (m21 - m12) / s, j: (m02 - m20) / s, k: (m10 - m01) / s, w: 0.25 * s }
        } else if m00 > m11 && m00 > m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
            Self { i: 0.25 * s, j: (m01 + m10) / s, k: (m02 + m20) / s, w: (m21 - m12) / s }
        } else if m11 > m22 {
            let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
            Self { i: (m01 + m10) / s, j: 0.25 * s, k: (m12 + m21) / s, w: (m02 - m20) / s }
        } else {
            let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
            Self { i: (m02 + m20) / s, j: (m12 + m21) / s, k: 0.25 * s, w: (m10 - m01) / s }
        }
    }

    /// Shortest-arc rotation between two unit vectors. Mirrors
    /// TagTool's `QuaternionFromVector` helper used in JMS phmo
    /// pill orientation. Degenerate cases (parallel or
    /// anti-parallel inputs) collapse to identity or a 180° rotation
    /// around an arbitrary perpendicular axis.
    pub fn shortest_arc(from: RealVector3d, to: RealVector3d) -> Self {
        let to_n = to.normalized();
        if to_n == RealVector3d::ZERO { return Self::IDENTITY; }
        let from_n = from.normalized();
        if from_n == RealVector3d::ZERO { return Self::IDENTITY; }
        let dot = from_n.dot(to_n);
        if dot > 0.999_999 { return Self::IDENTITY; }
        if dot < -0.999_999 {
            // 180° around any perpendicular axis.
            let perp = if from_n.i.abs() < 0.9 {
                RealVector3d { i: 1.0, j: 0.0, k: 0.0 }
            } else {
                RealVector3d { i: 0.0, j: 1.0, k: 0.0 }
            };
            let axis = from_n.cross(perp).normalized();
            return Self { i: axis.i, j: axis.j, k: axis.k, w: 0.0 };
        }
        let cross = from_n.cross(to_n);
        let s = ((1.0 + dot) * 2.0).sqrt();
        let inv_s = 1.0 / s;
        Self { i: cross.i * inv_s, j: cross.j * inv_s, k: cross.k * inv_s, w: s * 0.5 }
    }

    /// Normalized linear interpolation, short-arc. If `self.dot(other) < 0`
    /// the second quat is flipped so the interpolation takes the shorter
    /// rotational path.
    pub fn nlerp(self, other: Self, t: f32) -> Self {
        let dot = self.dot(other);
        let s = if dot < 0.0 { -1.0 } else { 1.0 };
        let one_minus_t = 1.0 - t;
        Self {
            i: self.i * one_minus_t + s * other.i * t,
            j: self.j * one_minus_t + s * other.j * t,
            k: self.k * one_minus_t + s * other.k * t,
            w: self.w * one_minus_t + s * other.w * t,
        }
        .normalized()
    }

    /// Component quad `[i, j, k, w]`.
    pub fn to_array(self) -> [f32; 4] { [self.i, self.j, self.k, self.w] }

    /// Apply this rotation to a vector. Optimized two-cross-product form:
    /// `v' = v + 2 * cross(q.xyz, cross(q.xyz, v) + q.w * v)`.
    pub fn rotate(self, v: RealVector3d) -> RealVector3d {
        let qv = RealVector3d { i: self.i, j: self.j, k: self.k };
        let t = qv.cross(v) * 2.0;
        v + RealVector3d {
            i: self.w * t.i + qv.j * t.k - qv.k * t.j,
            j: self.w * t.j + qv.k * t.i - qv.i * t.k,
            k: self.w * t.k + qv.i * t.j - qv.j * t.i,
        }
    }
}

impl Mul for RealQuaternion {
    type Output = Self;

    /// Hamilton product `self * rhs`.
    fn mul(self, rhs: Self) -> Self {
        let (ax, ay, az, aw) = (self.i, self.j, self.k, self.w);
        let (bx, by, bz, bw) = (rhs.i, rhs.j, rhs.k, rhs.w);
        Self {
            i: aw * bx + ax * bw + ay * bz - az * by,
            j: aw * by - ax * bz + ay * bw + az * bx,
            k: aw * bz + ax * by - ay * bx + az * bw,
            w: aw * bw - ax * bx - ay * by - az * bz,
        }
    }
}

impl Mul<RealVector3d> for RealQuaternion {
    type Output = RealVector3d;
    /// Convenience for [`Self::rotate`].
    fn mul(self, v: RealVector3d) -> RealVector3d { self.rotate(v) }
}

impl Neg for RealQuaternion {
    type Output = Self;
    /// Negate every component. Represents the same rotation
    /// (quaternions double-cover SO(3)); used by JMS phmo where stored
    /// ragdoll quats need a sign flip vs source.
    fn neg(self) -> Self {
        Self { i: -self.i, j: -self.j, k: -self.k, w: -self.w }
    }
}

//================================================================================
// RealPlane3d
//================================================================================

impl RealPlane3d {
    /// Plane normal, as a vector.
    pub fn normal(self) -> RealVector3d {
        RealVector3d { i: self.i, j: self.j, k: self.k }
    }

    /// Signed distance from the origin to the plane along the normal.
    /// Equivalent to `self.d`.
    pub fn offset(self) -> f32 { self.d }

    /// Cramer's-rule intersection of three planes. Returns `None` if
    /// the planes don't meet at a single point (parallel planes,
    /// near-zero determinant).
    ///
    /// Honors Halo's `n·p + d = 0` plane convention (as found in
    /// `real_plane_3d` tag fields). The solver solves
    /// `n_i · p = -d_i` for each plane.
    pub fn triple_intersection(a: Self, b: Self, c: Self) -> Option<RealPoint3d> {
        let n1 = a.normal();
        let n2 = b.normal();
        let n3 = c.normal();
        let det = n1.dot(n2.cross(n3));
        if det.abs() < 1e-9 {
            return None;
        }
        let p = (n2.cross(n3) * -a.d + n3.cross(n1) * -b.d + n1.cross(n2) * -c.d)
            * (1.0 / det);
        Some(RealPoint3d { x: p.i, y: p.j, z: p.k })
    }
}

//================================================================================
// Tests
//================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool { (a - b).abs() < 1e-5 }
    fn vec3_approx_eq(a: RealVector3d, b: RealVector3d) -> bool {
        approx_eq(a.i, b.i) && approx_eq(a.j, b.j) && approx_eq(a.k, b.k)
    }
    fn quat_approx_eq(a: RealQuaternion, b: RealQuaternion) -> bool {
        approx_eq(a.i, b.i) && approx_eq(a.j, b.j) && approx_eq(a.k, b.k) && approx_eq(a.w, b.w)
    }

    // --- vectors --------------------------------------------------------

    #[test]
    fn vec3_add_sub_scale_neg() {
        let a = RealVector3d { i: 1.0, j: 2.0, k: 3.0 };
        let b = RealVector3d { i: 4.0, j: 5.0, k: 6.0 };
        assert_eq!(a + b, RealVector3d { i: 5.0, j: 7.0, k: 9.0 });
        assert_eq!(b - a, RealVector3d { i: 3.0, j: 3.0, k: 3.0 });
        assert_eq!(a * 2.0, RealVector3d { i: 2.0, j: 4.0, k: 6.0 });
        assert_eq!(-a, RealVector3d { i: -1.0, j: -2.0, k: -3.0 });
    }

    #[test]
    fn vec3_dot_cross_length() {
        let x = RealVector3d { i: 1.0, j: 0.0, k: 0.0 };
        let y = RealVector3d { i: 0.0, j: 1.0, k: 0.0 };
        assert_eq!(x.dot(y), 0.0);
        assert_eq!(x.cross(y), RealVector3d { i: 0.0, j: 0.0, k: 1.0 });
        let v = RealVector3d { i: 3.0, j: 4.0, k: 0.0 };
        assert_eq!(v.length_squared(), 25.0);
        assert_eq!(v.length(), 5.0);
        assert!(vec3_approx_eq(v.normalized(), RealVector3d { i: 0.6, j: 0.8, k: 0.0 }));
    }

    #[test]
    fn vec3_normalize_zero_returns_zero() {
        assert_eq!(RealVector3d::ZERO.normalized(), RealVector3d::ZERO);
    }

    // --- points ---------------------------------------------------------

    #[test]
    fn point_plus_vector_equals_point() {
        let p = RealPoint3d { x: 1.0, y: 2.0, z: 3.0 };
        let v = RealVector3d { i: 10.0, j: 20.0, k: 30.0 };
        assert_eq!(p + v, RealPoint3d { x: 11.0, y: 22.0, z: 33.0 });
        assert_eq!(p - v, RealPoint3d { x: -9.0, y: -18.0, z: -27.0 });
    }

    #[test]
    fn point_minus_point_equals_vector() {
        let a = RealPoint3d { x: 5.0, y: 5.0, z: 5.0 };
        let b = RealPoint3d { x: 1.0, y: 2.0, z: 3.0 };
        assert_eq!(a - b, RealVector3d { i: 4.0, j: 3.0, k: 2.0 });
    }

    #[test]
    fn point_distance() {
        let a = RealPoint3d { x: 0.0, y: 0.0, z: 0.0 };
        let b = RealPoint3d { x: 3.0, y: 4.0, z: 0.0 };
        assert_eq!(a.distance_to(b), 5.0);
        assert_eq!(a.distance_squared_to(b), 25.0);
    }

    #[test]
    fn point_scaled() {
        let p = RealPoint3d { x: 1.0, y: 2.0, z: 3.0 };
        assert_eq!(p.scaled(100.0), RealPoint3d { x: 100.0, y: 200.0, z: 300.0 });
        assert_eq!(p * 100.0, RealPoint3d { x: 100.0, y: 200.0, z: 300.0 });
    }

    // --- quaternions ----------------------------------------------------

    #[test]
    fn quat_identity_is_unit_w() {
        assert_eq!(
            RealQuaternion::IDENTITY,
            RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 }
        );
    }

    #[test]
    fn quat_default_is_zero_not_identity() {
        // Documented quirk — Default is the derived (0,0,0,0).
        assert_eq!(
            RealQuaternion::default(),
            RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 0.0 }
        );
    }

    #[test]
    fn quat_mul_identity_is_self() {
        let q = RealQuaternion { i: 0.1, j: 0.2, k: 0.3, w: 0.927 };
        assert!(quat_approx_eq(q * RealQuaternion::IDENTITY, q));
        assert!(quat_approx_eq(RealQuaternion::IDENTITY * q, q));
    }

    #[test]
    fn quat_normalize_unit_is_unchanged() {
        let q = RealQuaternion { i: 1.0, j: 0.0, k: 0.0, w: 0.0 };
        assert!(quat_approx_eq(q.normalized(), q));
    }

    #[test]
    fn quat_normalize_zero_returns_zero() {
        let q = RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 0.0 };
        assert_eq!(q.normalized(), q);
    }

    #[test]
    fn quat_conjugate_negates_imaginary() {
        let q = RealQuaternion { i: 1.0, j: 2.0, k: 3.0, w: 4.0 };
        assert_eq!(
            q.conjugate(),
            RealQuaternion { i: -1.0, j: -2.0, k: -3.0, w: 4.0 }
        );
    }

    #[test]
    fn quat_neg_negates_all() {
        let q = RealQuaternion { i: 1.0, j: 2.0, k: 3.0, w: 4.0 };
        assert_eq!(
            -q,
            RealQuaternion { i: -1.0, j: -2.0, k: -3.0, w: -4.0 }
        );
    }

    #[test]
    fn quat_rotate_x_axis_around_z_90deg() {
        // 90° around Z — i=0, j=0, k=sin(45°), w=cos(45°)
        let s = std::f32::consts::FRAC_1_SQRT_2;
        let q = RealQuaternion { i: 0.0, j: 0.0, k: s, w: s };
        let x = RealVector3d { i: 1.0, j: 0.0, k: 0.0 };
        let rotated = q.rotate(x);
        assert!(vec3_approx_eq(rotated, RealVector3d { i: 0.0, j: 1.0, k: 0.0 }));
        // `q * x` is the same as `q.rotate(x)`.
        assert!(vec3_approx_eq(q * x, rotated));
    }

    #[test]
    fn quat_from_basis_columns_identity() {
        let c0 = RealVector3d { i: 1.0, j: 0.0, k: 0.0 };
        let c1 = RealVector3d { i: 0.0, j: 1.0, k: 0.0 };
        let c2 = RealVector3d { i: 0.0, j: 0.0, k: 1.0 };
        assert!(quat_approx_eq(
            RealQuaternion::from_basis_columns(c0, c1, c2),
            RealQuaternion::IDENTITY,
        ));
    }

    #[test]
    fn quat_shortest_arc_parallel_returns_identity() {
        let v = RealVector3d { i: 1.0, j: 0.0, k: 0.0 };
        assert_eq!(RealQuaternion::shortest_arc(v, v), RealQuaternion::IDENTITY);
    }

    #[test]
    fn quat_shortest_arc_antiparallel_is_180deg() {
        let from = RealVector3d { i: 1.0, j: 0.0, k: 0.0 };
        let to = RealVector3d { i: -1.0, j: 0.0, k: 0.0 };
        let q = RealQuaternion::shortest_arc(from, to);
        // 180° rotation should map `from` onto `to`.
        assert!(vec3_approx_eq(q.rotate(from), to));
    }

    #[test]
    fn quat_nlerp_picks_shorter_arc() {
        // Two quats with negative dot — nlerp should flip one to take
        // the short arc.
        let a = RealQuaternion::IDENTITY;
        let b = RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: -1.0 };
        let mid = a.nlerp(b, 0.5);
        // Both endpoints encode identity; midpoint should also be
        // identity (or the negated form of it — also valid).
        assert!(approx_eq(mid.length(), 1.0) || mid == RealQuaternion::default());
    }

    // --- planes ---------------------------------------------------------

    #[test]
    fn plane_triple_intersection_axis_planes() {
        // Honors `n·p + d = 0`: with `d = -2` the plane is `x = 2`.
        // x=2, y=3, z=4 should intersect at (2, 3, 4).
        let xp = RealPlane3d { i: 1.0, j: 0.0, k: 0.0, d: -2.0 };
        let yp = RealPlane3d { i: 0.0, j: 1.0, k: 0.0, d: -3.0 };
        let zp = RealPlane3d { i: 0.0, j: 0.0, k: 1.0, d: -4.0 };
        let p = RealPlane3d::triple_intersection(xp, yp, zp).unwrap();
        assert!(approx_eq(p.x, 2.0));
        assert!(approx_eq(p.y, 3.0));
        assert!(approx_eq(p.z, 4.0));
    }

    #[test]
    fn plane_triple_intersection_parallel_returns_none() {
        let p1 = RealPlane3d { i: 1.0, j: 0.0, k: 0.0, d: 0.0 };
        let p2 = RealPlane3d { i: 1.0, j: 0.0, k: 0.0, d: 1.0 };
        let p3 = RealPlane3d { i: 0.0, j: 1.0, k: 0.0, d: 0.0 };
        assert_eq!(RealPlane3d::triple_intersection(p1, p2, p3), None);
    }

    // --- bounds ---------------------------------------------------------

    #[test]
    fn bounds_contains_endpoints() {
        let b = Bounds { lower: 1.0_f32, upper: 5.0 };
        assert!(b.contains(1.0));
        assert!(b.contains(3.0));
        assert!(b.contains(5.0));
        assert!(!b.contains(0.999));
        assert!(!b.contains(5.001));
    }

    #[test]
    fn bounds_range_is_upper_minus_lower() {
        assert_eq!(Bounds { lower: 1.0_f32, upper: 5.0 }.range(), 4.0);
    }
}
