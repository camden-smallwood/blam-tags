# Writing Python with `blam_tags`

A task-oriented guide to the `blam_tags` extension module. For how the bindings
are *built* (the generator, the policy manifest, regeneration), see
[`README.md`](README.md). This document is for people writing Python against
the library.

- [Install and import](#install-and-import)
- [The object model](#the-object-model)
- [Opening, creating, and saving tags](#opening-creating-and-saving-tags)
- [Navigating a tag](#navigating-a-tag)
- [Reading and writing field values](#reading-and-writing-field-values)
- [Value type mapping](#value-type-mapping)
- [Enums](#enums)
- [Flags](#flags)
- [Blocks (repeating structs)](#blocks-repeating-structs)
- [Nested structs](#nested-structs)
- [The staleness rule](#the-staleness-rule) ← read this before writing loops
- [Math types](#math-types)
- [Group tags, checksums, dependency lists](#group-tags-checksums-dependency-lists)
- [Games and titles](#games-and-titles)
- [Payload structs and endianness](#payload-structs-and-endianness)
- [Exceptions](#exceptions)
- [A complete example](#a-complete-example)
- [Gotchas at a glance](#gotchas-at-a-glance)

---

## Install and import

Build a wheel with [maturin](https://www.maturin.rs/) and install it:

```sh
maturin build --release -o dist
pip install dist/blam_tags-*.whl
```

Or, for a local dev loop, `maturin develop` drops the module straight into the
active virtualenv. On macOS a plain `cargo build` also works thanks to the
repo's `.cargo/config.toml` (it supplies the `-undefined dynamic_lookup` linker
args PyO3's `extension-module` feature needs).

The `abi3-py38` build means one wheel per platform covers CPython 3.8 and up.

```python
import blam_tags as bt
```

Everything below assumes `import blam_tags as bt`. Schema paths in the examples
are relative to the repository root, so run scripts from there (or pass absolute
paths).

---

## The object model

Four handle types, arranged as a tree:

```
TagFile            a parsed tag file — owns the bytes
 └─ TagStruct      a struct instance: the root, or one block element, or a nested struct
     └─ TagField   one field of a struct
         ├─ TagStruct   (if the field is a nested struct)   via .as_struct()
         └─ TagBlock    (if the field is a block)           via .as_block()
             └─ TagStruct   each element                    via block[i]
```

`TagStruct`, `TagField`, and `TagBlock` are **path handles**, not owning
objects. Each one remembers *where it is* (an owning `TagFile` plus a field
path) and re-resolves from the root on every access. This is cheap at scripting
speed and is what lets the same navigation style work for reading and editing.

The one consequence you must know about is [staleness](#the-staleness-rule):
after a structural edit to a block, handles pointing *inside* that block stop
working and must be re-fetched. It's a deliberate guard against silently
addressing the wrong element — jump to that section before writing edit loops.

---

## Opening, creating, and saving tags

```python
# Create a fresh tag from a JSON schema, with one default-initialized root.
tag = bt.TagFile.new("definitions/halo3_mcc/biped.json")

# Read and fully parse an existing tag file.
tag = bt.TagFile.read("path/to/masterchief.biped")

# Parse a tag already in memory.
tag = bt.TagFile.read_from_bytes(some_bytes)
```

`read` and `write` accept a `str` or any `os.PathLike` (e.g. `pathlib.Path`).

```python
tag.write("out.biped")            # serialize to a path
tag.write_atomic("out.biped")     # serialize via temp file + rename
data = tag.write_to_bytes()       # serialize to a bytes object

tag.group_tag                     # the four-char group, e.g. "bipd"
```

Read/write is **byte-exact**: `read_from_bytes(b).write_to_bytes() == b`. The
bindings do not perturb the round-trip.

---

## Navigating a tag

Start at the root and walk down by field name:

```python
root = tag.root()               # a TagStruct

root.name                       # struct schema name, e.g. "biped_group"
root.path                       # "" for the root
root.size                       # size in bytes of one instance
root.field_names()              # ["flags", "jump velocity", "unit", ...]

field = root.field("jump velocity")     # a TagField, or None if absent
```

`field(name)` returns `None` for an unknown name rather than raising — check for
it. To resolve a deeper path in one call, use `field_path` with `/`-separated
names:

```python
seats = root.field_path("unit/seats")    # TagField, or None
```

Field paths are **ordinal-qualified** so they stay unambiguous among same-named
siblings — you'll see forms like `jump velocity#8` and `unit#0/seats#40[1]` in
`.path` (the `#N` counts preceding fields, so the exact number depends on the
schema). You don't construct these yourself; navigate by name and read `.path`
when you need an identifier.

Field metadata:

```python
f = root.field("flags")
f.name          # raw schema name, markup included
f.clean_name    # name with schema markup stripped
f.type_name     # schema type string, e.g. "real", "long flags"
f.field_type    # a bt.TagFieldType enum member, e.g. bt.TagFieldType.Real
f.path          # this field's ordinal-qualified path from the root
```

---

## Reading and writing field values

A scalar field's value crosses as the most natural Python type. Read it with the
`.value` property; write it with `.set(...)`:

```python
root.field("jump velocity").set(9.75)
assert root.field("jump velocity").value == 9.75
```

**Structural fields** (blocks, arrays, nested structs) have no scalar value —
`.value` returns `None`. Reach into them with `.as_struct()`, `.as_block()`, or
`.as_data()` instead:

```python
field.as_struct()   # -> TagStruct, or None if the field isn't a struct
field.as_block()    # -> TagBlock,  or None if the field isn't a block
field.as_data()     # -> bytes,     or None if the field isn't a data field
```

Two rules make `.set` safe and predictable:

1. **`field.set(field.value)` is always a no-op.** Whatever a read produces, a
   write of the same value accepts, for every field type.
2. **A write never changes a field's type.** `.set` substitutes the *payload*
   into the field's existing on-disk shape; it never picks a new variant.
   Assigning the wrong Python type raises `TypeError` with a message naming the
   shape the field expected:

   ```python
   root.field("jump velocity").set("not a float")   # TypeError: expects a float
   ```

Edits reach the serialized bytes immediately — there is no separate "commit"
step beyond `write`/`write_to_bytes`.

---

## Value type mapping

What `.value` returns, and what `.set(...)` accepts, per field kind:

| Field kind | `.value` returns | `.set(...)` accepts |
|---|---|---|
| string, long string | `str` | `str` |
| string id, old string id | `str` | `str` |
| char/short/long/int64/byte/word/dword/qword integer | `int` | `int` (range-checked to the field's width) |
| tag (group) | `int` | `int` |
| angle, real, real slider, real fraction | `float` | `float` |
| enum (char/short/long) | `str` variant name if the schema names it, else `int` | `int` index or `str` variant name (see [Enums](#enums)) |
| flags (byte/word/long) | `list[str]` of set-bit names | use `set_flag` — see [Flags](#flags) |
| block flags (byte/word/long) | `int` | `int` |
| block index (char/short/long, incl. custom) | `int` | `int` |
| tag reference | `None`, or `(group_tag: int, path: str)` | `None`, or `(group_tag: int, path: str)` |
| data, custom | `bytes` | `bytes` |
| api interop | `bytes` | **not writable** — raises `NotImplementedError` |
| point2d, rectangle2d | `bt.Point2d` / `bt.Rectangle2d` | the same class |
| real point/vector 2d & 3d | `bt.RealPoint2d` / `RealPoint3d` / `RealVector2d` / `RealVector3d` | the same class |
| real quaternion | `bt.RealQuaternion` | the same class |
| real euler angles 2d/3d | `bt.RealEulerAngles2d` / `3d` | the same class |
| real plane 2d/3d | `bt.RealPlane2d` / `3d` | the same class |
| rgb/argb color (packed) | `bt.RgbColor` / `bt.ArgbColor` | the same class |
| real rgb/argb/hsv/ahsv color | `bt.RealRgbColor` / `RealArgbColor` / `RealHsvColor` / `RealAhsvColor` | the same class |
| short integer bounds | `bt.ShortBounds` | `bt.ShortBounds` |
| real / angle / fraction bounds | `bt.RealBounds` | `bt.RealBounds` |
| block, array, struct, pointer, pad, etc. | `None` | not assignable — use `.as_struct()` / `.as_block()` |

For the math composites, construct the matching class and pass it to `.set`:

```python
node = tag.root().field_path("some/translation")   # a real point 3d field
node.set(bt.RealPoint3d(1.0, 2.0, 3.0))
```

---

## Enums

Enum fields read back as the schema's variant *name* when it has one:

```python
f = root.field("some enum")
f.value                     # e.g. "standing"
```

Writing an enum works **by numeric index or by variant name**:

```python
f.set(2)                    # by index — the third option
f.set("standing")           # by name — resolved against the schema's options
```

An unknown name raises `ValueError`. To discover the valid options, use
`.options()` (see below).

### Discovering options

`field.options()` returns the field's full option catalog — every defined
variant, not just the current one — or `None` for non-enum/flags fields:

```python
o = f.options()
o.is_enum         # True
o.names           # ['normal', 'slaved to primary', ...] — index == stored value
o.current         # 0 (the stored index), or None if it didn't resolve
o.current_name    # 'normal'
```

---

## Flags

Flags read back as a `list[str]` of the currently set bits, and are written
one bit at a time by name — never by assigning a list (a bare list can't name
bits the schema doesn't define, so `.set([...])` on a flags field raises
`NotImplementedError`):

```python
f = root.field("flags")

f.value                                       # [] when nothing is set
f.set_flag("turns without animating", True)   # set one bit
f.get_flag("turns without animating")         # True
f.value                                        # ["turns without animating"]
f.set_flag("turns without animating", False)  # clear it
```

`flag_names()` lists only the bits that are currently **set**. To enumerate
*every* bit the schema defines — which is what you need to know what
`set_flag` accepts — use `.options()`:

```python
o = f.options()
o.is_flags        # True
o.flags           # [(bit, name, is_set), ...] — every defined bit
o.names           # just the names, in bit order
```

An unknown flag name raises `KeyError` (from `get_flag`/`set_flag`).

---

## Blocks (repeating structs)

A block field becomes a `TagBlock` via `.as_block()`. It behaves like a Python
sequence for reads, with explicit methods for structural edits.

```python
seats = root.field("unit").as_struct().field("seats").as_block()

len(seats)            # element count
seats[0]              # a TagStruct (negative indices work; out of range -> IndexError)
seats.element_size    # size in bytes of one element
seats.path            # this block's path from the root
```

Structural edits:

```python
i = seats.add()              # append a default element, returns its index
seats.insert(0)              # insert a default element at index 0
j = seats.duplicate(0)       # copy element 0 in place, returns the new index
seats.delete(1)              # remove element 1
seats.swap(0, 2)             # exchange two elements
seats.move_element(3, 0)     # move an element from one index to another
seats.clear()                # remove every element
```

Editing elements is ordinary navigation:

```python
seats.add()
seats[0].field("label").set("driver")
```

> **Every structural edit invalidates handles pointing inside the block** —
> including element and field handles you fetched earlier. See the next
> section.

---

## Nested structs

A struct-typed field becomes a `TagStruct` via `.as_struct()`:

```python
unit = root.field("unit").as_struct()   # TagStruct, or None if not a struct
unit.path                                 # e.g. "unit#0"
"seats" in unit.field_names()             # True
```

From there it's the same `field` / `field_path` / `as_block` navigation as the
root. A struct also exposes:

```python
unit.descend("object")        # a nested TagStruct by path (not a field), or None
unit.fields_all()             # like fields(), but including padding/skip entries
unit.raw                      # the struct's raw on-disk bytes
```

---

## Arrays, resources, and functions

Beyond blocks and structs, a field can be a **fixed-count array**, a
**pageable resource**, or a **tag function** (`mapping_function`). Each has its
own accessor, returning `None` when the field is not that shape:

```python
arr = field.as_array()        # TagArray — like a block but fixed count
len(arr); arr[0]              # indexable; swap(i, j) / replace(i, snap) to edit

res = field.as_resource()     # TagResource
res.kind                      # "null" | "exploded" | "xsync"
res.inline_bytes              # the 8 inline engine bytes
res.exploded_payload          # the tgdt payload bytes, or None

if field.is_function_data():
    fn = field.as_function()  # TagFunction
    fn.function_type          # e.g. "Constant", "Linear"
    fn.evaluate(0.5, 1.0)     # sample the curve
```

### Copying block/array elements

`snapshot(i)` captures an element; `paste(i, snap)` (blocks) or
`replace(i, snap)` (arrays) writes it back — including across different blocks
of the same element type:

```python
snap = src_block.snapshot(0)
dst_block.paste(len(dst_block), snap)
```

---

## The staleness rule

Because handles re-resolve by path, a structural edit could otherwise make an
old handle silently address a *different* element. The library prevents this:
**a handle refuses to resolve if a later structural edit landed on a block that
contains it**, raising `StaleHandleError`.

```python
seats.add(); seats.add()
elem  = seats[0]
label = elem.field("label")

seats.insert(0)          # a structural edit to `seats`

elem.name                # raises bt.StaleHandleError
label.value              # raises bt.StaleHandleError
```

The guard is precise, so the API stays usable — an edit to `seats` invalidates
handles *inside* `seats`, but **not**:

- the tag root,
- unrelated sibling fields,
- the `seats` block handle itself (its contents shifted, its identity didn't).

```python
seats.insert(0)
root.name                    # fine
len(seats)                   # fine — the block handle survives
root.field("jump velocity").set(1.5)   # fine — unrelated field
```

The data isn't gone, just re-indexed — **re-fetch from a surviving handle**:

```python
seats.insert(0)
seats[1].field("label").value   # the element that was [0] is now at [1]
```

Practical rule: **after any `add`/`insert`/`duplicate`/`delete`/`swap`/
`move_element`/`clear`, re-fetch element and field handles from the block or
root.** Editing a field's *value* is not structural and invalidates nothing.

---

## Math types

The math primitives are standalone value classes — useful on their own, and the
currency for reading/writing math-typed fields. They mirror the Rust API,
including operator overloads and alternative constructors.

```python
v = bt.RealVector3d(1.0, 2.0, 3.0)
v.i, v.j, v.k            # component access (mutable)
v.length(); v.normalized(); v.dot(other); v.cross(other); v.to_array()

bt.RealVector3d.ZERO     # associated constants are class attributes
bt.RealQuaternion.IDENTITY

# Point vs. vector algebra is preserved via return type:
p, q = bt.RealPoint3d(3, 0, 0), bt.RealPoint3d(1, 0, 0)
p - q                    # -> RealVector3d (a displacement)
p - bt.RealVector3d(1, 0, 0)   # -> RealPoint3d (a translation)

# Alternative constructors are static methods:
bt.RealQuaternion.shortest_arc(a, b)
bt.RealQuaternion.from_basis_columns(c0, c1, c2)
m = bt.Matrix4.from_loc_rot_scale(bt.RealPoint3d(1,2,3), bt.RealQuaternion.IDENTITY, 2.0)
m.decompose()            # -> (RealPoint3d, RealQuaternion, float)
```

The bounds type is monomorphized: `bt.RealBounds` (with `.contains`, `.range`)
and `bt.ShortBounds` are distinct classes, and `bt.AngleBounds` /
`bt.FractionBounds` are aliases *for the same class* as `RealBounds`:

```python
bt.AngleBounds is bt.RealBounds        # True
bt.FractionBounds is bt.RealBounds     # True
```

Value classes support `==`, `repr()`, and `copy.copy()`. Fixed-size matrix
constructors validate arity and raise `ValueError` on the wrong shape.

The full list of classes and their methods/attributes is in
[`blam_tags.pyi`](blam_tags.pyi), which your editor and type checker read
directly.

---

## Group tags, checksums, dependency lists

```python
bt.parse_group_tag("bipd")     # -> 0x62697064  (or None if not 4 chars)
bt.format_group_tag(0x62697064)  # -> "bipd"

tag.recompute_checksum()       # recompute and store the header checksum

tag.add_dependency_list("definitions/halo3_mcc/biped.json")  # attach a `want` stream
tag.remove_dependency_list()   # drop it, if present

tag.group_version              # the group's version number
tag.endian                     # bt.Endian.Le / bt.Endian.Be
tag.classic_engine             # engine name for a classic tag, else None

# Read just a tag's dependency references without parsing the whole file:
bt.TagFile.read_dependency_references("some.biped")  # [(group, path), ...] or None
```

### Side structs (anchors)

Besides `root()`, a tag can carry three separate top-level structs. Each is
navigable and editable exactly like the root, or `None` when absent:

```python
tag.dependency_list()       # the `want` stream struct
tag.import_info()           # the source-asset import-info struct
tag.asset_depot_storage()   # the asset-depot-storage struct
```

Each has an `add_*` / `remove_*` counterpart on `TagFile`
(`add_import_info`, `remove_asset_depot_storage`, …).

---

## Games and titles

`Game` is the tag-format generation, derived from the tag bytes:

```python
bt.Game.of(tag)          # -> bt.Game.Halo1 / Halo2 / Halo3
game.jms_version()       # int
game.ass_version()       # int or None
game.jma_version()       # int
```

`Title` is a *different axis* — which MCC editing kit is loaded. H3, ODST,
Reach, and H4 all share `Game.Halo3` (identical tag structures) but are distinct
titles. The title isn't in the tag bytes; it comes from the loading context:

```python
bt.Title.from_game_id("halo4_mcc")   # -> bt.Title.Halo4 (or None)
# variants: HaloCe, Halo2, Halo2A, Halo3, Halo3Odst, HaloReach, Halo4
```

---

## Payload structs and endianness

Sub-chunk payloads can be parsed and re-emitted on their own. Byte order is
explicit where it matters (`bt.Endian.Le` / `bt.Endian.Be`):

```python
# string id
s = bt.StringIdData("my_string")
s.string                       # "my_string"
s.to_bytes()                   # list[int]; wrap with bytes(...) for a bytes object
bt.StringIdData.from_bytes(s.to_bytes())

# tag reference — None for a null reference, else a (group_tag, path) tuple
ref = bt.TagReferenceData((0x62697064, "objects\\characters\\masterchief"))
raw = ref.to_bytes(bt.Endian.Le)
bt.TagReferenceData.from_bytes(raw, bt.Endian.Le).group_tag_and_name
bt.TagReferenceData(None).to_bytes(bt.Endian.Le)   # b"" — null is an empty payload

# api interop (runtime pointer slot; the canonical reset is {0, 0xFFFFFFFF, 0})
a = bt.ApiInteropData.reset()
a.descriptor(); a.address(); a.definition_address()
```

`to_bytes()` returns a `list[int]`; call `bytes(...)` on it if you need a
`bytes` object.

---

## Exceptions

All bindings raise from one hierarchy, so you can catch broadly or precisely:

```
BlamTagsError                (base — also the catch-all for unnamed Rust errors)
 ├─ TagReadError             a tag file could not be parsed
 └─ StaleHandleError         a handle outlived a structural edit to its block
```

```python
try:
    tag = bt.TagFile.read("missing.biped")
except bt.TagReadError:
    ...
except bt.BlamTagsError:      # catches everything the library raises
    ...
```

Ordinary Python exceptions surface where they fit: `TypeError` (wrong value type
for a field), `ValueError` (bad enum name, wrong array shape), `KeyError`
(unknown flag), `IndexError` (block index out of range), `LookupError` (a path
that no longer resolves), `NotImplementedError` (writing flags via `.set`, or an
api-interop field).

---

## A complete example

```python
import blam_tags as bt

# Start from a schema, or use bt.TagFile.read(...) on an existing tag.
tag  = bt.TagFile.new("definitions/halo3_mcc/biped.json")
root = tag.root()

# Scalar edits.
root.field("jump velocity").set(9.75)
root.field("flags").set_flag("turns without animating", True)

# Grow a nested block and fill an element.
seats = root.field("unit").as_struct().field("seats").as_block()
seats.add()
seats[0].field("label").set("driver")

# After a structural edit, re-fetch handles from the block/root.
seats.add()
seats[1].field("label").set("gunner")

# Read everything back.
for i in range(len(seats)):
    print(i, seats[i].field("label").value)

tag.recompute_checksum()
tag.write("masterchief.biped")
```

---

## Gotchas at a glance

- **Re-fetch handles after structural block edits.** `add`/`insert`/`delete`/…
  invalidate handles *inside* that block; using a stale one raises
  `StaleHandleError`. Value edits are safe.
- **`.value` is `None` for structural fields.** Use `.as_struct()` /
  `.as_block()` / `.as_data()`.
- **`.set` never changes a field's type.** Pass the type the field already
  holds; a mismatch is a `TypeError`.
- **Flags don't take a list.** Use `set_flag(name, on)` / `get_flag(name)`.
- **Enums switch by index, not by name.** A name only confirms the current
  value.
- **api-interop fields are read-only** (runtime pointers).
- **`field(name)` returns `None`, not an exception, for a missing field.**
- **`to_bytes()` on payload structs returns `list[int]`** — wrap with
  `bytes(...)` if you want a `bytes` object.
- **Run scripts from the repo root** when using the relative schema paths, or
  pass absolute paths.
```
