# blam-tags-py

Python bindings for `blam-tags`.

Most of this crate is machine-generated. `blam-tags-bindgen` reads the rustdoc
JSON for `blam-tags`, applies the policy in `bindings.toml`, and writes three
files:

| file | contents |
|---|---|
| `src/generated.rs` | the PyO3 wrapper classes |
| `blam_tags.pyi` | Python type stubs |
| `COVERAGE.md` | what was bound, what was not, and why |

All three are **checked in**. `blam-tags` itself is untouched and takes no
`pyo3` dependency.

## Regenerating

```sh
cargo +nightly rustdoc -p blam-tags --features audio -- \
    -Z unstable-options --output-format json
cargo run -p blam-tags-bindgen
```

Only the generator needs nightly — rustdoc's JSON output is an unstable
format. The wheel itself builds on stable, because it compiles the
checked-in source rather than regenerating at build time.

Generation is deterministic: the same inputs produce a byte-identical
`generated.rs`. That makes "regenerate and diff" a viable CI check — if a
`blam-tags` change alters the public API without a corresponding regeneration,
the diff is non-empty and CI fails.

## Building the wheel

```sh
maturin build --release -o dist
pip install dist/blam_tags-*.whl
python tests/test_math.py
```

`abi3-py38` means one wheel per platform covers CPython 3.8 and up, rather
than one wheel per interpreter version.

## The policy manifest

`bindings.toml` records the human decisions the generator cannot infer. Its
central rule: **every non-`auto` strategy must carry a `reason`**, enforced at
load time, and every reason is reproduced in `COVERAGE.md`. A type that goes
unbound does so visibly and on purpose, never by accident.

Three strategies:

- `auto` — generate a wrapper from the rustdoc description.
- `manual` — a hand-written wrapper is expected instead. Used for the
  lifetime-parameterized facade types (`TagStruct`, `TagField`, …), which
  borrow from the owning `TagFile` and so cannot be wrapped mechanically.
- `skip` — deliberately unbound.

Generic types are a special case. `Bounds<T>` has no single ABI to wrap, so
each instantiation the policy declares becomes its own Python class, and
several Rust aliases may collapse onto one of them:

```toml
[types.Bounds]
monomorphize = [
    { args = ["i16"], name = "ShortBounds" },
    { args = ["f32"], name = "RealBounds", aliases = ["AngleBounds", "FractionBounds"] },
]
```

## Current scope

`math`, `game`, `file`, `fields`, and the `api` facade — 32 generated classes
plus 4 hand-written ones, 53 Python tests. `error` is bound as exceptions;
`io` is skipped except for `Endian`.

```python
import blam_tags as bt

tag  = bt.TagFile.new("definitions/halo3_mcc/biped.json")
root = tag.root()

root.field("jump velocity").set(9.75)
root.field("flags").set_flag("turns without animating", True)

seats = root.field("unit").as_struct().field("seats").as_block()
seats.add()
seats[0].field("label").set("driver")

tag.write("masterchief.biped")
```

Run the tests from the repository root, since the schema paths are relative
to it:

```sh
for t in math tagfile fields facade; do python blam-tags-py/tests/test_$t.py; done
```

## The facade is hand-written

`TagStruct`, `TagField`, and `TagBlock` borrow from the `TagFile` that owns
them, and a `#[pyclass]` must be `'static`, so they cannot be wrapped. They
are re-expressed in `src/manual/handles.rs` as *path handles*: an owning file
plus an owned `TagFieldPath`, re-resolved on every access.

That re-resolution reintroduces a hazard the borrow checker prevents — hold a
handle to element 5, delete element 2, and the path silently names a different
element. `TagFile` therefore logs the path of every structural edit, and a
handle refuses to resolve if a later edit landed on a block containing it. The
containment test is what keeps it usable: adding a seat invalidates handles
*inside* `seats`, but not the tag root, not a sibling field, and not the
`seats` block handle itself.

`TagFieldData`'s 57 variants are converted in `src/manual/value.rs` under one
rule: `field.set(field.value)` is a no-op for every field type. Writes never
choose a variant — the stored value supplies the shape and Python supplies
only the payload — so an assignment can change a field's contents but never
its on-disk type.

## Deliberately unexposed

`TagFile.header` is not reachable from Python. `TagFileHeader` derives only
`Debug`, so it can be neither copied nor cloned out of a `&TagFile`, and the
generator will not emit an accessor that cannot compile. Adding `Clone`
upstream would expose it, but the header is not needed from Python — so
`blam-tags` stays untouched and this stays unbound, by decision rather than
by omission.
