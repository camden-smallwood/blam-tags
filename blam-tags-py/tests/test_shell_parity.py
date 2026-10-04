"""Checks the Python side of `parity.toml` at the repository root.

The shell's own tests keep that file's list of commands complete, and the
engine's tests check its engine symbols; this one checks that every attribute
it names under `python`, for a shell command or an engine area, exists on the
module, so the file cannot claim Python covers something it does not.
"""

import os
import sys

import pytest

import blam_tags as bt

if sys.version_info >= (3, 11):
    import tomllib
else:
    tomllib = pytest.importorskip("tomli")

PARITY = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "parity.toml")


def _entries():
    with open(PARITY, "rb") as f:
        doc = tomllib.load(f)
    for kind in ("cli", "repl", "engine"):
        for name, entry in doc[kind].items():
            yield kind, name, entry


def _resolves(dotted):
    """Whether `dotted` (e.g. `TagFile.read`) names an attribute of the module."""
    obj = bt
    for part in dotted.split("."):
        if not hasattr(obj, part):
            return False
        obj = getattr(obj, part)
    return True


CLAIMS = [
    (f"{kind}.{name}", attr)
    for kind, name, entry in _entries()
    for attr in entry.get("python", [])
]


def test_parity_file_claims_something():
    assert len(CLAIMS) > 20


@pytest.mark.parametrize("entry,attr", CLAIMS)
def test_claimed_attribute_exists(entry, attr):
    assert _resolves(attr), f"parity.toml [{entry}] names bt.{attr}, which does not exist"


def test_resolver_can_say_no():
    # A check that always agrees is not a check.
    assert _resolves("TagFile.read")
    assert not _resolves("TagFile.no_such_method")
    assert not _resolves("NoSuchClass.read")
