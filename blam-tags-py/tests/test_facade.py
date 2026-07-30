"""Exercises the hand-written borrowing facade.

Run from the repository root — the schema path is relative to it.

The facade is the part that could not be generated: `TagStruct`, `TagField`,
and `TagBlock` all borrow from the owning `TagFile` in Rust, so here they are
path handles that re-resolve on every access. These tests cover what that
design can plausibly get wrong — values that do not reach the bytes, writes
that change a field's type, and handles that outlive the element they name.
"""

import blam_tags as bt

SCHEMA = "definitions/halo3_mcc/biped.json"


def _tag():
    return bt.TagFile.new(SCHEMA)


def _seats(tag):
    return tag.root().field("unit").as_struct().field("seats").as_block()


def test_root_and_navigation():
    tag = _tag()
    root = tag.root()
    assert root.name == "biped_group"
    assert root.path == ""
    assert len(root.field_names()) > 30
    assert root.field("no such field") is None


def test_field_metadata():
    f = _tag().root().field("jump velocity")
    assert f.clean_name == "jump velocity"
    assert f.type_name == "real"
    assert f.field_type == bt.TagFieldType.Real
    # The path is ordinal-qualified so it stays valid amongst same-named
    # siblings.
    assert f.path == "jump velocity#7"


def test_edit_reaches_the_serialized_bytes():
    """A value that reads back correctly but never reaches `write_to_bytes`
    would be indistinguishable from a working edit at the REPL."""
    tag = _tag()
    before = tag.write_to_bytes()
    tag.root().field("jump velocity").set(9.75)
    after = tag.write_to_bytes()

    assert before != after
    assert len(before) == len(after)
    assert bt.TagFile.read_from_bytes(after).root().field("jump velocity").value == 9.75


def test_set_of_value_is_a_round_trip():
    """The design rule for the value layer: what a read produces, a write
    accepts, for every field type present."""
    tag = _tag()
    root = tag.root()
    before = tag.write_to_bytes()
    for name in root.field_names():
        field = root.field(name)
        value = field.value
        if value is None or isinstance(value, list):
            continue  # structural fields and flag lists are set differently
        field.set(value)
    assert tag.write_to_bytes() == before


def test_writes_cannot_change_a_fields_type():
    """The variant comes from what is stored, never from the Python value."""
    field = _tag().root().field("jump velocity")
    try:
        field.set("not a float")
    except TypeError:
        pass
    else:
        raise AssertionError("expected TypeError assigning a str to a real field")


def test_flags_read_as_names_and_set_by_name():
    field = _tag().root().field("flags")
    assert field.value == []
    field.set_flag("turns without animating", True)
    assert field.get_flag("turns without animating") is True
    assert field.value == ["turns without animating"]
    field.set_flag("turns without animating", False)
    assert field.value == []


def test_unknown_flag_raises():
    field = _tag().root().field("flags")
    try:
        field.get_flag("no such flag")
    except KeyError:
        pass
    else:
        raise AssertionError("expected KeyError for an unknown flag")


def test_block_growth_and_indexing():
    tag = _tag()
    block = _seats(tag)
    assert len(block) == 0

    assert block.add() == 0
    block.add()
    assert len(block) == 2
    assert block[0].name == "unit_seat_block"
    assert block[-1].path == "unit#0/seats#40[1]"

    try:
        block[99]
    except IndexError:
        pass
    else:
        raise AssertionError("expected IndexError past the end")


def test_block_element_edits_are_independent():
    tag = _tag()
    block = _seats(tag)
    block.add()
    block.add()
    block[0].field("label").set("driver")
    block[1].field("label").set("gunner")
    assert [block[i].field("label").value for i in range(2)] == ["driver", "gunner"]


def test_duplicate_inserts_a_copy_after_the_source():
    tag = _tag()
    block = _seats(tag)
    block.add()
    block.add()
    block[0].field("label").set("driver")
    block[1].field("label").set("gunner")

    assert block.duplicate(0) == 1
    assert [block[i].field("label").value for i in range(3)] == [
        "driver",
        "driver",
        "gunner",
    ]


def test_block_delete_and_clear():
    tag = _tag()
    block = _seats(tag)
    for _ in range(3):
        block.add()
    block.delete(1)
    assert len(block) == 2
    block.clear()
    assert len(block) == 0


def test_block_edits_reach_the_bytes():
    tag = _tag()
    before = len(tag.write_to_bytes())
    block = _seats(tag)
    block.add()
    block[0].field("label").set("driver")
    grown = tag.write_to_bytes()
    assert len(grown) > before
    assert (
        bt.TagFile.read_from_bytes(grown)
        .root()
        .field("unit")
        .as_struct()
        .field("seats")
        .as_block()[0]
        .field("label")
        .value
        == "driver"
    )


def test_handles_inside_an_edited_block_go_stale():
    """Path handles re-resolve, so without this guard a handle to element 0
    would silently start addressing a *different* element after an insert."""
    tag = _tag()
    block = _seats(tag)
    block.add()
    block.add()
    element = block[0]
    field = element.field("label")

    block.insert(0)

    for label, thunk in (("element", lambda: element.name), ("field", lambda: field.value)):
        try:
            thunk()
        except bt.StaleHandleError:
            pass
        else:
            raise AssertionError(f"{label} handle should have gone stale")


def test_handles_outside_an_edited_block_survive():
    """The guard has to be precise, or every edit would invalidate the whole
    tag and the API would be unusable."""
    tag = _tag()
    root = tag.root()
    sibling = root.field("jump velocity")
    block = _seats(tag)
    block.add()

    block.insert(0)

    assert root.name == "biped_group"       # the root
    assert len(block) == 2                  # the edited block itself
    sibling.set(1.5)                        # an unrelated field
    assert sibling.value == 1.5


def test_re_resolving_after_a_structural_edit_works():
    tag = _tag()
    block = _seats(tag)
    block.add()
    block[0].field("label").set("driver")
    block.insert(0)
    # The old handle is stale, but the data moved rather than vanished.
    assert block[1].field("label").value == "driver"


def test_nested_struct_access():
    unit = _tag().root().field("unit").as_struct()
    assert unit is not None
    assert unit.path == "unit#0"
    assert "seats" in unit.field_names()


def test_non_block_field_is_not_a_block():
    assert _tag().root().field("jump velocity").as_block() is None


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
