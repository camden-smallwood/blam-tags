//! Conversion between [`TagFieldData`] and native Python values.
//!
//! This is the layer that decides how the API *feels*. The guiding rule is
//! that `field.set(field.value)` must be a no-op for every field type: what
//! a read produces, a write accepts.
//!
//! Two consequences follow. First, a field's value crosses as the most
//! natural Python type — `float`, `int`, `str`, `bytes`, a tuple — rather
//! than as a tagged wrapper object. Second, writes never *choose* a variant:
//! [`apply`] reads the field's current [`TagFieldData`] to learn its shape
//! and substitutes only the payload, so a Python value can never change a
//! field's on-disk type.
//!
//! Math composites reuse the generated wrapper classes rather than degrading
//! to anonymous tuples, so `point.x` still works after a read.

use blam_tags::fields::TagFieldData;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyList};
use pyo3::BoundObject;

use crate::generated as wrap;

/// Box any convertible value into an owned `PyObject`.
fn obj<'py, T>(py: Python<'py>, v: T) -> PyResult<Py<PyAny>>
where
    T: IntoPyObject<'py>,
    PyErr: From<T::Error>,
{
    Ok(v.into_pyobject(py)?.into_any().unbind())
}

/// Box a generated wrapper class into an owned `PyObject`.
fn cls<T: pyo3::PyClass + Into<pyo3::PyClassInitializer<T>>>(
    py: Python<'_>,
    v: T,
) -> PyResult<Py<PyAny>> {
    Ok(Py::new(py, v)?.into_any())
}

/// Render a field's value as a native Python object.
pub fn to_py(py: Python<'_>, data: &TagFieldData) -> PyResult<Py<PyAny>> {
    use TagFieldData as D;
    match data {
        // ---- text ----
        D::String(s) | D::LongString(s) => obj(py, s.as_str()),
        D::StringId(s) | D::OldStringId(s) => obj(py, s.string.as_str()),

        // ---- sub-chunk payloads ----
        // A null reference is `None`, matching `Option` on the Rust side,
        // rather than a sentinel tuple.
        D::TagReference(r) => match &r.group_tag_and_name {
            None => Ok(py.None()),
            Some((tag, path)) => obj(py, (*tag, path.as_str())),
        },
        D::Data(v) | D::Custom(v) => obj(py, PyBytes::new(py, v)),
        D::ApiInterop(a) => obj(py, PyBytes::new(py, &a.to_bytes())),

        // ---- integers ----
        D::CharInteger(v) => obj(py, *v),
        D::ShortInteger(v) => obj(py, *v),
        D::LongInteger(v) => obj(py, *v),
        D::Int64Integer(v) => obj(py, *v),
        D::ByteInteger(v) => obj(py, *v),
        D::WordInteger(v) => obj(py, *v),
        D::DwordInteger(v) => obj(py, *v),
        D::QwordInteger(v) => obj(py, *v),
        D::Tag(v) => obj(py, *v),

        // ---- enums: the resolved name if the schema has one, else the raw ----
        D::CharEnum { value, name } => match name {
            Some(n) => obj(py, n.as_str()),
            None => obj(py, *value),
        },
        D::ShortEnum { value, name } => match name {
            Some(n) => obj(py, n.as_str()),
            None => obj(py, *value),
        },
        D::LongEnum { value, name } => match name {
            Some(n) => obj(py, n.as_str()),
            None => obj(py, *value),
        },

        // ---- flags: the names of the set bits ----
        D::ByteFlags { names, .. } | D::WordFlags { names, .. } | D::LongFlags { names, .. } => {
            let set: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
            obj(py, PyList::new(py, set)?)
        }

        // Block flags carry no names, so they stay numeric.
        D::ByteBlockFlags(v) => obj(py, *v),
        D::WordBlockFlags(v) => obj(py, *v),
        D::LongBlockFlags(v) => obj(py, *v),

        // ---- block indices ----
        D::CharBlockIndex(v) | D::CustomCharBlockIndex(v) => obj(py, *v),
        D::ShortBlockIndex(v) | D::CustomShortBlockIndex(v) => obj(py, *v),
        D::LongBlockIndex(v) | D::CustomLongBlockIndex(v) => obj(py, *v),

        // ---- floats ----
        D::Angle(v) | D::Real(v) | D::RealSlider(v) | D::RealFraction(v) => obj(py, *v),

        // ---- math composites: reuse the generated wrapper classes ----
        D::Point2d(v) => cls(py, wrap::PyPoint2d(*v)),
        D::Rectangle2d(v) => cls(py, wrap::PyRectangle2d(*v)),
        D::RealPoint2d(v) => cls(py, wrap::PyRealPoint2d(*v)),
        D::RealPoint3d(v) => cls(py, wrap::PyRealPoint3d(*v)),
        D::RealVector2d(v) => cls(py, wrap::PyRealVector2d(*v)),
        D::RealVector3d(v) => cls(py, wrap::PyRealVector3d(*v)),
        D::RealQuaternion(v) => cls(py, wrap::PyRealQuaternion(*v)),
        D::RealEulerAngles2d(v) => cls(py, wrap::PyRealEulerAngles2d(*v)),
        D::RealEulerAngles3d(v) => cls(py, wrap::PyRealEulerAngles3d(*v)),
        D::RealPlane2d(v) => cls(py, wrap::PyRealPlane2d(*v)),
        D::RealPlane3d(v) => cls(py, wrap::PyRealPlane3d(*v)),
        D::RgbColor(v) => cls(py, wrap::PyRgbColor(*v)),
        D::ArgbColor(v) => cls(py, wrap::PyArgbColor(*v)),
        D::RealRgbColor(v) => cls(py, wrap::PyRealRgbColor(*v)),
        D::RealArgbColor(v) => cls(py, wrap::PyRealArgbColor(*v)),
        D::RealHsvColor(v) => cls(py, wrap::PyRealHsvColor(*v)),
        D::RealAhsvColor(v) => cls(py, wrap::PyRealAhsvColor(*v)),
        D::ShortIntegerBounds(v) => cls(py, wrap::PyShortBounds(*v)),
        D::AngleBounds(v) | D::RealBounds(v) | D::FractionBounds(v) => {
            cls(py, wrap::PyRealBounds(*v))
        }
    }
}

/// Substitute a Python value into an existing [`TagFieldData`], preserving
/// its variant.
///
/// The variant is never chosen from the Python value — it comes from what is
/// already stored — so writing can change a field's contents but never its
/// type.
pub fn apply(data: &mut TagFieldData, value: &Bound<'_, PyAny>) -> PyResult<()> {
    use TagFieldData as D;

    /// Extract, reporting the field's expected shape rather than a bare
    /// "argument error".
    macro_rules! get {
        ($t:ty, $what:literal) => {
            value.extract::<$t>().map_err(|_| {
                pyo3::exceptions::PyTypeError::new_err(format!(
                    "this field expects {}, got {}",
                    $what,
                    value.get_type().name().map(|n| n.to_string()).unwrap_or_default()
                ))
            })?
        };
    }

    match data {
        D::String(s) | D::LongString(s) => *s = get!(String, "a str"),
        D::StringId(s) | D::OldStringId(s) => s.string = get!(String, "a str"),

        D::TagReference(r) => {
            r.group_tag_and_name = if value.is_none() {
                None
            } else {
                Some(get!((u32, String), "None or a (group_tag, path) tuple"))
            };
        }
        D::Data(v) | D::Custom(v) => *v = get!(Vec<u8>, "bytes"),
        D::ApiInterop(_) => {
            return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                "api_interop fields are runtime pointers and are not writable",
            ));
        }

        D::CharInteger(v) => *v = get!(i8, "an int"),
        D::ShortInteger(v) => *v = get!(i16, "an int"),
        D::LongInteger(v) => *v = get!(i32, "an int"),
        D::Int64Integer(v) => *v = get!(i64, "an int"),
        D::ByteInteger(v) => *v = get!(u8, "an int"),
        D::WordInteger(v) => *v = get!(u16, "an int"),
        D::DwordInteger(v) => *v = get!(u32, "an int"),
        D::QwordInteger(v) => *v = get!(u64, "an int"),
        D::Tag(v) => *v = get!(u32, "an int"),

        // An enum accepts either the raw value or a variant name, matching
        // what `to_py` produced.
        D::CharEnum { value: v, name } => set_enum(value, v, name)?,
        D::ShortEnum { value: v, name } => set_enum(value, v, name)?,
        D::LongEnum { value: v, name } => set_enum(value, v, name)?,

        D::ByteFlags { .. } | D::WordFlags { .. } | D::LongFlags { .. } => {
            return Err(pyo3::exceptions::PyNotImplementedError::new_err(
                "assign flags through `set_flag(name, on)`; a bare list cannot \
                 name bits the schema does not define",
            ));
        }
        D::ByteBlockFlags(v) => *v = get!(u8, "an int"),
        D::WordBlockFlags(v) => *v = get!(u16, "an int"),
        D::LongBlockFlags(v) => *v = get!(i32, "an int"),

        D::CharBlockIndex(v) | D::CustomCharBlockIndex(v) => *v = get!(i8, "an int"),
        D::ShortBlockIndex(v) | D::CustomShortBlockIndex(v) => *v = get!(i16, "an int"),
        D::LongBlockIndex(v) | D::CustomLongBlockIndex(v) => *v = get!(i32, "an int"),

        D::Angle(v) | D::Real(v) | D::RealSlider(v) | D::RealFraction(v) => {
            *v = get!(f32, "a float")
        }

        D::Point2d(v) => *v = get!(wrap::PyPoint2d, "a Point2d").0,
        D::Rectangle2d(v) => *v = get!(wrap::PyRectangle2d, "a Rectangle2d").0,
        D::RealPoint2d(v) => *v = get!(wrap::PyRealPoint2d, "a RealPoint2d").0,
        D::RealPoint3d(v) => *v = get!(wrap::PyRealPoint3d, "a RealPoint3d").0,
        D::RealVector2d(v) => *v = get!(wrap::PyRealVector2d, "a RealVector2d").0,
        D::RealVector3d(v) => *v = get!(wrap::PyRealVector3d, "a RealVector3d").0,
        D::RealQuaternion(v) => *v = get!(wrap::PyRealQuaternion, "a RealQuaternion").0,
        D::RealEulerAngles2d(v) => *v = get!(wrap::PyRealEulerAngles2d, "a RealEulerAngles2d").0,
        D::RealEulerAngles3d(v) => *v = get!(wrap::PyRealEulerAngles3d, "a RealEulerAngles3d").0,
        D::RealPlane2d(v) => *v = get!(wrap::PyRealPlane2d, "a RealPlane2d").0,
        D::RealPlane3d(v) => *v = get!(wrap::PyRealPlane3d, "a RealPlane3d").0,
        D::RgbColor(v) => *v = get!(wrap::PyRgbColor, "an RgbColor").0,
        D::ArgbColor(v) => *v = get!(wrap::PyArgbColor, "an ArgbColor").0,
        D::RealRgbColor(v) => *v = get!(wrap::PyRealRgbColor, "a RealRgbColor").0,
        D::RealArgbColor(v) => *v = get!(wrap::PyRealArgbColor, "a RealArgbColor").0,
        D::RealHsvColor(v) => *v = get!(wrap::PyRealHsvColor, "a RealHsvColor").0,
        D::RealAhsvColor(v) => *v = get!(wrap::PyRealAhsvColor, "a RealAhsvColor").0,
        D::ShortIntegerBounds(v) => *v = get!(wrap::PyShortBounds, "a ShortBounds").0,
        D::AngleBounds(v) | D::RealBounds(v) | D::FractionBounds(v) => {
            *v = get!(wrap::PyRealBounds, "a RealBounds").0
        }
    }
    Ok(())
}

/// Assign an enum field from either its raw value or its schema name.
fn set_enum<T>(value: &Bound<'_, PyAny>, slot: &mut T, name: &Option<String>) -> PyResult<()>
where
    T: for<'a, 'p> FromPyObject<'a, 'p>,
{
    if let Ok(raw) = value.extract::<T>() {
        *slot = raw;
        return Ok(());
    }
    let Ok(wanted) = value.extract::<String>() else {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "an enum field expects an int or a variant name",
        ));
    };
    // Only the *current* variant's name is carried in the parsed value, so a
    // name-based assignment can confirm a no-op but cannot resolve a
    // different variant without the schema's option list.
    match name {
        Some(current) if *current == wanted => Ok(()),
        _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "cannot resolve enum name {wanted:?} from the parsed value; \
             assign the numeric option index instead"
        ))),
    }
}
