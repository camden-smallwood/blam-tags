"""Exercises the generated `file`, `game`, and `error` bindings.

Run from the repository root — the schema paths are relative to it.

This covers the shapes `math` could not: `Result` returns surfacing as Python
exceptions, `P: AsRef<Path>` parameters, byte payloads, a non-`Clone` wrapper
that can only be passed by reference, and a fieldless enum.
"""

import os
import tempfile

import blam_tags as bt

SCHEMA = "definitions/halo3_mcc/biped.json"


def _fresh():
    return bt.TagFile.new(SCHEMA)


def test_new_from_schema_and_serialize():
    tag = _fresh()
    data = tag.write_to_bytes()
    assert len(data) > 0


def test_bytes_round_trip_is_identical():
    """The whole point of the crate is a byte-exact read/write path; the
    binding must not perturb it."""
    original = _fresh().write_to_bytes()
    assert bt.TagFile.read_from_bytes(original).write_to_bytes() == original


def test_file_round_trip_matches_in_memory():
    tag = _fresh()
    expected = tag.write_to_bytes()
    path = os.path.join(tempfile.mkdtemp(), "out.biped")

    tag.write(path)
    assert os.path.getsize(path) == len(expected)
    assert bt.TagFile.read(path).write_to_bytes() == expected


def test_write_actually_writes():
    """`write` returns `Result<(), io::Error>`. Mapping the unit ok-type by
    rebuilding it from its (zero) tuple fields silently discarded the call
    itself, producing a method that reported success and wrote nothing."""
    path = os.path.join(tempfile.mkdtemp(), "out.biped")
    _fresh().write(path)
    assert os.path.exists(path) and os.path.getsize(path) > 0


def test_write_atomic_also_writes():
    path = os.path.join(tempfile.mkdtemp(), "atomic.biped")
    tag = _fresh()
    tag.write_atomic(path)
    assert os.path.getsize(path) == len(tag.write_to_bytes())


def test_errors_surface_as_distinct_exceptions():
    """Each Rust error type maps to its own Python exception, so callers can
    catch precisely what `blam-tags` raises."""
    try:
        bt.TagFile.read("/nonexistent/tag.biped")
    except bt.TagReadError:
        pass
    else:
        raise AssertionError("expected TagReadError")

    # `TagFile::new` returns `Box<dyn Error>`, which has no name of its own
    # and falls back to the catch-all rather than an exception called `Box`.
    try:
        bt.TagFile.new("/nonexistent/schema.json")
    except bt.BlamTagsError:
        pass
    else:
        raise AssertionError("expected BlamTagsError")


def test_corrupt_input_raises_rather_than_panics():
    try:
        bt.TagFile.read_from_bytes(b"not a tag file at all")
    except bt.TagReadError:
        pass
    else:
        raise AssertionError("expected TagReadError for malformed input")


def test_path_arguments_accept_pathlike():
    """`P: AsRef<Path>` maps to `PathBuf`, which PyO3 accepts from `str` and
    from any `os.PathLike`."""
    import pathlib

    tag = _fresh()
    path = pathlib.Path(tempfile.mkdtemp()) / "pathlike.biped"
    tag.write(path)
    assert bt.TagFile.read(path).write_to_bytes() == tag.write_to_bytes()


def test_tagfile_crosses_as_a_reference():
    """`TagFile` is not `Clone`, so its wrapper cannot be passed by value.
    `Game.of` takes `&TagFile` in Rust and must do the same here."""
    assert bt.Game.of(_fresh()) == bt.Game.Halo3


def test_mutating_method_takes_mut_self():
    tag = _fresh()
    tag.recompute_checksum()  # `&mut self` in Rust
    assert len(tag.write_to_bytes()) > 0


def test_large_types_get_a_concise_repr():
    """`Debug` on `TagFile` renders the entire parsed layout — tens of
    kilobytes. The generated fallback is `<Name object>`; `TagFile` is
    hand-written and names its group instead."""
    assert repr(_fresh()) == "<TagFile bipd>"
    assert len(repr(_fresh())) < 100


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
