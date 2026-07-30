"""Exercises the generated `fields` bindings and module-level free functions.

New shapes here: functions that belong to a module rather than a type, a
71-variant fieldless enum, and sub-chunk payload structs that round-trip
through bytes with an explicit wire endianness.
"""

import blam_tags as bt

BIPD = 0x62697064


def test_module_level_functions():
    """`format_group_tag` and `parse_group_tag` are free functions, not
    methods — they are collected from the module rather than an impl block."""
    assert bt.format_group_tag(BIPD) == "bipd"
    assert bt.parse_group_tag("bipd") == BIPD
    assert bt.parse_group_tag("too long to be a group tag") is None


def test_group_tag_functions_are_inverse():
    for tag in ("bipd", "weap", "scnr", "bitm"):
        assert bt.format_group_tag(bt.parse_group_tag(tag)) == tag


def test_large_fieldless_enum():
    """71 variants, all fieldless — the shape the generator handles without
    any policy input."""
    variants = [n for n in dir(bt.TagFieldType) if n[0].isupper()]
    assert len(variants) == 71
    assert bt.TagFieldType.Real != bt.TagFieldType.Angle
    assert bt.TagFieldType.Real == bt.TagFieldType.Real


def test_string_id_payload_round_trip():
    s = bt.StringIdData("my_string")
    assert s.string == "my_string"
    assert bytes(s.to_bytes()) == b"my_string"
    assert bt.StringIdData.from_bytes(s.to_bytes()).string == "my_string"


def test_tag_reference_round_trip():
    path = "objects\\characters\\masterchief"
    ref = bt.TagReferenceData((BIPD, path))
    raw = ref.to_bytes(bt.Endian.Le)
    assert bt.TagReferenceData.from_bytes(raw, bt.Endian.Le).group_tag_and_name == (
        BIPD,
        path,
    )


def test_null_tag_reference_is_none():
    """A null reference is an empty payload, not a zero-filled one."""
    null = bt.TagReferenceData(None)
    assert bytes(null.to_bytes(bt.Endian.Le)) == b""
    assert (
        bt.TagReferenceData.from_bytes(null.to_bytes(bt.Endian.Le), bt.Endian.Le
        ).group_tag_and_name
        is None
    )


def test_endian_parameter_changes_the_bytes():
    """`Endian` is pulled in from the otherwise-skipped `io` module purely
    because these payload methods take it. If it crossed as a no-op the two
    encodings would be identical."""
    ref = bt.TagReferenceData((BIPD, "x"))
    le = bytes(ref.to_bytes(bt.Endian.Le))[:4]
    be = bytes(ref.to_bytes(bt.Endian.Be))[:4]
    assert le != be
    assert be == b"bipd"
    assert le == b"dpib"


def test_tag_reference_survives_both_endians():
    for endian in (bt.Endian.Le, bt.Endian.Be):
        ref = bt.TagReferenceData((BIPD, "objects\\vehicles\\warthog"))
        back = bt.TagReferenceData.from_bytes(ref.to_bytes(endian), endian)
        assert back.group_tag_and_name == ref.group_tag_and_name


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
