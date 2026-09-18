"""Exercises the facade methods added for parity with the Rust `api` surface:
enum/flag option discovery, enum-set-by-name, array/resource/function access,
struct/block extras, and the dependency-list / import-info anchors.

Run from the repository root — the schema path is relative to it.
"""

import blam_tags as bt

SCHEMA = "definitions/halo3_mcc/biped.json"


def _tag():
    return bt.TagFile.new(SCHEMA)


def _find_enum(struct, depth=0):
    """First enum-typed field anywhere in the tree, as a `TagField` handle."""
    if depth > 5:
        return None
    for name in struct.field_names():
        field = struct.field(name)
        opts = field.options()
        if opts is not None and opts.is_enum and len(opts.names) > 1:
            return field
        nested = field.as_struct()
        if nested is not None:
            found = _find_enum(nested, depth + 1)
            if found is not None:
                return found
    return None


# --- option discovery -------------------------------------------------------


def test_flags_options_enumerate_every_defined_bit():
    """`flag_names` only lists set bits; `options` must list the whole catalog
    so a caller can discover what `set_flag` accepts on a zero-valued field."""
    flags = _tag().root().field("flags")
    opts = flags.options()
    assert opts is not None
    assert opts.is_flags and not opts.is_enum
    assert len(opts.flags) > 0
    # every entry is (bit, name, is_set); bits are the declaration order
    bits = [bit for (bit, _name, _set) in opts.flags]
    assert bits == list(range(len(bits)))
    # names view agrees with the flags view
    assert opts.names == [name for (_bit, name, _set) in opts.flags]


def test_enum_options_and_current():
    field = _find_enum(_tag().root())
    assert field is not None, "expected at least one enum field in a biped"
    opts = field.options()
    assert opts.is_enum and not opts.is_flags
    assert len(opts.names) > 1
    assert opts.current == 0
    assert opts.current_name == opts.names[0]


def test_options_is_none_for_plain_fields():
    # A plain real field is neither enum nor flags.
    root = _tag().root()
    plain = next(
        root.field(n)
        for n in root.field_names()
        if root.field(n).type_name == "real"
    )
    assert plain.options() is None


# --- enum set by name -------------------------------------------------------


def test_enum_can_be_set_by_name():
    field = _find_enum(_tag().root())
    names = field.options().names
    target = names[2]
    field.set(target)
    assert field.value == target
    assert field.options().current == 2
    # numeric assignment still works
    field.set(0)
    assert field.value == names[0]


def test_enum_set_rejects_unknown_name():
    field = _find_enum(_tag().root())
    try:
        field.set("definitely not a variant")
        assert False, "expected a ValueError"
    except ValueError as e:
        assert "unknown enum name" in str(e)


# --- struct extras ----------------------------------------------------------


def test_fields_all_includes_padding():
    root = _tag().root()
    assert len(root.fields_all()) >= len(root.field_names())


def test_raw_length_matches_size():
    root = _tag().root()
    assert len(root.raw) == root.size


def test_descend_reaches_a_nested_struct():
    unit = _tag().root().descend("unit")
    assert unit is not None
    assert unit.path == "unit"
    assert _tag().root().descend("no/such/path") is None


# --- block snapshot / paste -------------------------------------------------


def test_block_snapshot_and_paste():
    tag = _tag()
    seats = tag.root().field("unit").as_struct().field("seats").as_block()
    assert seats.is_empty()
    seats.add()
    seats[0].field("label").set("driver")
    snap = seats.snapshot(0)
    pasted = seats.paste(seats.__len__(), snap)
    # re-resolve after the structural edit
    seats = tag.root().field("unit").as_struct().field("seats").as_block()
    assert seats.__len__() == 2
    assert seats[pasted].field("label").value == "driver"


# --- array / resource / function access -------------------------------------


def test_as_array_resource_function_are_none_for_scalars():
    root = _tag().root()
    flags = root.field("flags")
    assert flags.as_array() is None
    assert flags.as_resource() is None
    assert flags.as_function() is None
    assert flags.is_function_data() is False


# --- anchors ----------------------------------------------------------------


def test_anchors_are_none_on_a_bare_tag():
    tag = _tag()
    assert tag.dependency_list() is None
    assert tag.import_info() is None
    assert tag.asset_depot_storage() is None


def test_dependency_list_anchor_is_navigable_and_editable():
    tag = _tag()
    tag.add_dependency_list(SCHEMA)
    dep = tag.dependency_list()
    assert dep is not None
    # the anchor resolves its own field tree, independent of root()
    names = dep.field_names()
    assert len(names) > 0
    # a scalar edit through the anchor round-trips
    scalar = next(
        dep.field(n)
        for n in names
        if isinstance(dep.field(n).value, (int, float))
    )
    original = scalar.value
    scalar.set(original)
    assert tag.dependency_list().field(scalar.name).value == original
    # and the whole file still serializes
    assert isinstance(tag.write_to_bytes(), bytes)
    tag.remove_dependency_list()
    assert tag.dependency_list() is None
