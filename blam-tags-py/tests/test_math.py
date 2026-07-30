"""Exercises the generated `math` bindings.

Focus is on the properties that generation could plausibly get *wrong* —
point-vs-vector operator semantics, per-instantiation method sets on a
generic type, fixed-size array arity — rather than on re-testing arithmetic
that `blam-tags` already covers on the Rust side.
"""

import copy

import blam_tags as bt


def test_fields_and_construction():
    v = bt.RealVector3d(1.0, 2.0, 3.0)
    assert (v.i, v.j, v.k) == (1.0, 2.0, 3.0)
    v.i = 10.0
    assert v.i == 10.0


def test_associated_constants_are_class_attributes():
    assert (bt.RealVector3d.ZERO.i, bt.RealVector3d.ZERO.j) == (0.0, 0.0)
    q = bt.RealQuaternion.IDENTITY
    assert (q.i, q.j, q.k, q.w) == (0.0, 0.0, 0.0, 1.0)


def test_methods_round_trip_through_rust():
    a = bt.RealVector3d(1.0, 0.0, 0.0)
    b = bt.RealVector3d(0.0, 1.0, 0.0)
    assert a.dot(b) == 0.0
    assert a.cross(b).k == 1.0
    assert bt.RealVector3d(3.0, 4.0, 0.0).length() == 5.0
    assert bt.RealVector3d(3.0, 4.0, 0.0).to_array() == [3.0, 4.0, 0.0]


def test_point_vector_algebra_is_preserved():
    """The Rust API deliberately refuses point+point and distinguishes
    point-point (a displacement) from point-vector (a translation). Both
    `Sub` impls collapse onto one Python `__sub__`, so the return *type*
    is the only thing that still encodes the distinction."""
    p = bt.RealPoint3d(3.0, 0.0, 0.0)
    q = bt.RealPoint3d(1.0, 0.0, 0.0)
    v = bt.RealVector3d(1.0, 0.0, 0.0)

    displacement = p - q
    assert isinstance(displacement, bt.RealVector3d)
    assert displacement.i == 2.0

    translated = p - v
    assert isinstance(translated, bt.RealPoint3d)
    assert translated.x == 2.0

    assert isinstance(p + v, bt.RealPoint3d)


def test_unsupported_operand_raises_typeerror():
    p = bt.RealPoint3d(1.0, 2.0, 3.0)
    try:
        p - "not a point"
    except TypeError:
        pass
    else:
        raise AssertionError("expected TypeError for unsupported operand")


def test_scalar_multiply_and_negate():
    v = bt.RealVector3d(1.0, -2.0, 3.0)
    assert (v * 2.0).j == -4.0
    assert (-v).j == 2.0


def test_generic_monomorphizations_have_distinct_method_sets():
    """`contains`/`range` exist only on `impl Bounds<f32>`. If the generator
    ignored the impl's type arguments, `ShortBounds` would advertise methods
    that do not exist for `Bounds<i16>`."""
    assert hasattr(bt.RealBounds, "contains")
    assert hasattr(bt.RealBounds, "range")
    assert not hasattr(bt.ShortBounds, "contains")
    assert not hasattr(bt.ShortBounds, "range")

    rb = bt.RealBounds(0.0, 10.0)
    assert rb.contains(5.0) and not rb.contains(11.0)
    assert rb.range() == 10.0

    sb = bt.ShortBounds(-5, 5)
    assert (sb.lower, sb.upper) == (-5, 5)


def test_aliases_point_at_the_same_class():
    assert bt.AngleBounds is bt.RealBounds
    assert bt.FractionBounds is bt.RealBounds


def test_fixed_size_array_arity_is_checked():
    """`Matrix4.m` is `[[f32; 4]; 4]`. Python can hand over a list of any
    length, so the boundary must reject the wrong shape as a ValueError
    instead of panicking across the FFI edge."""
    rows = [[float(r * 4 + c) for c in range(4)] for r in range(4)]
    m = bt.Matrix4(rows)
    assert m.m == rows

    for bad in ([[0.0] * 4] * 3, [[0.0] * 3] * 4):
        try:
            bt.Matrix4(bad)
        except ValueError:
            pass
        else:
            raise AssertionError(f"expected ValueError for shape {bad!r}")


def test_tuple_struct_exposes_value():
    c = bt.RgbColor(0x00FF8040)
    assert c.value == 0x00FF8040


def test_derived_protocols():
    v = bt.RealVector3d(1.0, 2.0, 3.0)
    assert v == bt.RealVector3d(1.0, 2.0, 3.0)
    assert v != bt.RealVector3d(9.0, 2.0, 3.0)
    assert "RealVector3d" in repr(v)
    assert copy.copy(v) == v


def test_associated_functions_become_staticmethods():
    """Alternative constructors carry no `self`. Binding them as
    `@staticmethod` is what keeps them reachable at all."""
    q = bt.RealQuaternion.shortest_arc(
        bt.RealVector3d(1.0, 0.0, 0.0), bt.RealVector3d(0.0, 1.0, 0.0)
    )
    # A 90° rotation about +Z: (0, 0, sin45, cos45).
    assert abs(q.k - 0.70710677) < 1e-6
    assert abs(q.w - 0.70710677) < 1e-6

    m = bt.Matrix4.from_loc_rot_scale(
        bt.RealPoint3d(1.0, 2.0, 3.0), bt.RealQuaternion.IDENTITY, 2.0
    )
    # Uniform scale on the diagonal, translation in the last column.
    assert [row[3] for row in m.m] == [1.0, 2.0, 3.0, 1.0]
    assert [m.m[i][i] for i in range(3)] == [2.0, 2.0, 2.0]


def test_docstrings_survive_generation():
    assert "Dot product" in bt.RealVector3d.dot.__doc__


if __name__ == "__main__":
    failures = 0
    for name, fn in sorted(globals().items()):
        if not name.startswith("test_"):
            continue
        try:
            fn()
            print(f"  PASS  {name}")
        except Exception as exc:  # noqa: BLE001 — this is the reporter
            failures += 1
            print(f"  FAIL  {name}: {type(exc).__name__}: {exc}")
    print(f"\n{'FAILED' if failures else 'ALL PASSED'} ({failures} failure(s))")
    raise SystemExit(1 if failures else 0)
