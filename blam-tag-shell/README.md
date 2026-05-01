# blam-tag-shell

## About

A command-line tool and interactive REPL for inspecting and editing Halo tag files. Built on the [`blam-tags`](../blam-tags/) library — no ManagedBlam, no .NET, no engine required.

See the workspace [root README](../README.md) for build instructions and an overview of the two crates.

## Install

```sh
cargo install --path blam-tag-shell
```

## Usage

```
blam-tag-shell --game <GAME> <COMMAND> [ARGS...] [FLAGS]
```

Every tag-bound command takes a `<FILE>` argument as its first positional. In the [REPL](#repl--interactive-shell) that argument is filled in automatically from the currently-loaded tag, so you can type `get "jump velocity"` without repeating the path.

### Required: `--game` / `-g`

Every invocation requires the global `--game <GAME>` flag (alias `-g`). The shell uses it for two things:

- **Schema lookup** — schemas are read from `definitions/<GAME>/<group>.json`. Required for `new`, the optional-stream attach commands, and any command that needs to know how to interpret a tag's contents.
- **Tag-reference rendering** — group-tag → group-name resolution comes from `definitions/<GAME>/_meta.json`, which is loaded eagerly at startup. Bad/missing path errors out before any command runs.

`<GAME>` is the directory name under `definitions/` — currently `halo3_mcc`, `halo3odst_mcc`, `haloreach_mcc`, `halo4_mcc`, and `halo2amp_mcc`. The flag is global, so it can appear anywhere on the command line:

```sh
blam-tag-shell --game halo3_mcc header masterchief.biped
blam-tag-shell -g haloreach_mcc list /path/to/reach/tags --group bipd
blam-tag-shell get masterchief.biped "jump velocity" --game halo3_mcc
```

The example commands below elide `--game` for readability — add it to every invocation.

### Commands

| Command | Description |
|-|-|
| [`repl`](#repl--interactive-shell) | Interactive shell against a loaded tag |
| [`new`](#new--create-a-fresh-tag-from-a-schema) | Create a fresh tag from a schema JSON |
| [`inspect`](#inspect--show-field-tree) | Show the field tree |
| [`get`](#get--read-a-field-value) | Read a field value |
| [`set`](#set--write-a-field-value) | Write a field value |
| [`flag`](#flag--get-or-set-a-flag-bit) | Get or set a flag bit by name |
| [`options`](#options--list-enumflag-options) | List enum/flag options for a field |
| [`block`](#block--block-element-operations) | Block element operations (count, add, insert, duplicate, delete, clear, swap, move) |
| [`deps`](#deps--list-tag-references) | List every `tag_reference` in a tag |
| [`list`](#list--walk-a-directory-for-tags) | Walk a directory for tags; filter + list, or summarize by group |
| [`find`](#find--search-values-across-a-directory) | Search a directory of tags for fields whose value matches a query |
| [`check`](#check--integrity-validator) | Integrity check — flag enum / flag / real / reference anomalies |
| [`header`](#header--file-metadata) | Show tag/cache file header metadata |
| [`layout-diff`](#layout-diff--compare-tag-schemas) | Diff the **schemas** of two tag files |
| [`data-diff`](#data-diff--compare-two-tag-values) | Diff the **values** of two tag files |
| [`export`](#export--dump-tag-state-as-replay-commands) | Dump a tag's state as replayable `set` commands |
| [`extract-bitmap`](#extract-bitmap--bitmap-to-tiff--dds) | Extract a `.bitmap` tag's images as TIFF (default, Tool-importable) or DDS files (one per image) |
| [`extract-geometry`](#extract-geometry--tag-to-source-tree-jms--ass-files) | Extract geometry source files. Accepts `.model` (render/coll/phys, render auto JMS-or-ASS), `.scenario` (one ASS per structure BSP, lighting baked in), or `.scenario_structure_bsp` (single ASS). |
| [`extract-data`](#extract-data--dump-a-tag_data-field) | Write the bytes of a single `tag_data` field to a file |
| [`extract-import-info`](#extract-import-info--unzip-source-files-from-the-info-stream) | Decompress and write out the source files baked into a tag's `info` stream (zlib-compressed JMS / JMA / TIFF / ASS that the importer consumed) |
| [`list-animations`](#list-animations--enumerate-jmad-animations) | List the animations in a `model_animation_graph` tag |
| [`extract-animation`](#extract-animation--decode-and-export-an-animation) | Decode one or every jmad animation; write JMA-family text or JSON |
| [`add-dependency-list`](#optional-stream-commands) | Attach an empty dependency-list stream |
| [`remove-dependency-list`](#optional-stream-commands) | Drop the dependency-list stream |
| [`rebuild-dependency-list`](#optional-stream-commands) | Repopulate the dependency list from the tag's own tag_references |
| [`add-import-info`](#optional-stream-commands) | Attach an empty import-info stream |
| [`remove-import-info`](#optional-stream-commands) | Drop the import-info stream |
| [`add-asset-depot-storage`](#optional-stream-commands) | Attach an empty asset-depot-storage stream |
| [`remove-asset-depot-storage`](#optional-stream-commands) | Drop the asset-depot-storage stream |

---

### `repl` — Interactive shell

| Argument | Description |
|-|-|
| `[FILE]` | Optional tag to load at startup. |

Opens a persistent session against a loaded tag. Tag-bound commands always operate on the loaded tag — the file positional is filled in automatically, so `inspect materials[0]` drills into the loaded tag's `materials[0]` element rather than being misread as `inspect materials[0] (no path)`. To work on a different tag, use `open <path>` to switch. Dirty-tracking means the REPL asks for confirmation before discarding unsaved edits. History is persisted to `~/.blam-tag-shell-history`.

**Session verbs:**

| | |
|-|-|
| `open <path>` | Load a tag. |
| `close` | Close the current tag. |
| `save [path]` | Write the tag (back to source, or to `path`). |
| `revert` | Reload from disk, discarding edits. |
| `exit` / `quit` | Leave the REPL (prompts on unsaved changes; `exit --force` skips the prompt). |
| `help` / `?` | Show inline help. |

**Navigation** (Unix-cd semantics — leading `/` resets to absolute):

| | |
|-|-|
| `edit-block <path>` | Push a sub-struct / block-element / array-element onto the nav stack. Alias: `cd`. |
| `back` | Pop one level. |
| `exit-to <segment>` | Pop until the named segment is the tail; `exit-to root` clears. |
| `pwd` | Show the current nav path. |

The prompt reflects the current `--game`, currently-loaded tag, dirty state (`*` after the tag name), and nav stack:

```
halo3_mcc> open masterchief.biped
halo3_mcc :: masterchief.biped> cd unit/seats[0]
halo3_mcc :: masterchief.biped/unit/seats[0]> flag "flags" "invisible" toggle
set unit/seats[0]/flags.invisible = on (was off)
halo3_mcc :: masterchief.biped*/unit/seats[0]> save
saved to masterchief.biped
halo3_mcc :: masterchief.biped/unit/seats[0]> exit
```

---

### `new` — Create a fresh tag from a schema

| Argument | Description |
|-|-|
| `<GROUP>` | Group name — matches the JSON filename under `definitions/<GAME>/` (e.g. `biped`, `render_model`, `scenario`). |

| Long | Description |
|-|-|
| `--output <FILE>` | Write to this path. Default: `./<GROUP>.<GROUP>` in the cwd. Refuses to overwrite an existing file. |

Game comes from the global `--game` flag. Resolves the schema at `definitions/<GAME>/<GROUP>.json`, builds a zero-filled tag (one default-initialized root element, no optional streams), and writes it. Header gets `group_tag` / `group_version` from the schema, `signature = 'BLAM'`, and `checksum = 0`.

Run from the workspace root (or wherever your `definitions/` tree lives) — paths are resolved relative to cwd.

```sh
$ blam-tag-shell --game halo3_mcc new render_model
created render_model.render_model from definitions/halo3_mcc/render_model.json

$ blam-tag-shell --game halo3_mcc new biped --output characters/chief.biped
created characters/chief.biped from definitions/halo3_mcc/biped.json
```

---

### `inspect` — Show field tree

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `[PATH]` | Field path to start from (optional — defaults to the root). |

| Long | Description |
|-|-|
| `--all` | Include schema padding / explanation / skip / unknown fields. |
| `--full` | Recursively expand everything — including block elements. Default (flat): walk through structs / arrays / pageable_resources, but stop at blocks (display the count only). Drill into a single block element with `<path>[<index>]` instead. |
| `--json` | Emit JSON. |
| `--filter <S,...>` | Only show fields whose name contains any of the comma-separated substrings. |
| `--filter-not <S,...>` | Skip fields whose name contains any of these. |
| `--filter-value <S>` | Only leaves whose rendered value contains `S`. |

Two modes — flat (default) and full. **Flat** recurses through structs, arrays, and pageable_resources (the resource header struct walks like any other container) but stops at blocks: each block shows its `[<n> elements]` count and nothing more. **Full** recurses through blocks too. Arrays always expand regardless, since they're fixed-count from the schema.

When a block or array element collapses to a single scalar leaf (e.g. spherical-harmonic coefficient arrays where each element is a one-field `coefficient: real`), the `[i]` header and its leaf are merged onto a single line:

```
default lightprobe r: array [16 elements]
  [0] coefficient: real = 0.8951472
  [1] coefficient: real = -1.0852382
  ...
```

`--all` opts out of the collapse (every padding / explanation field gets its own line).

```sh
# Top-level fields — blocks show as count only
$ blam-tag-shell inspect masterchief.biped
unit: struct
  object: struct
  flags: long flags = 0x0046C708 [fires from camera, melee attackers cannot attach, ...]
  default team: short enum = 1 (player)
  attachments: block [3 elements]
  ...
jump velocity: real = 3.08

# Drill into a single element — blocks below it still gated by --full
$ blam-tag-shell inspect masterchief.biped "unit/seats[0]"

# Recursively expand everything, including blocks
$ blam-tag-shell inspect masterchief.biped --full

# Filter by name + value
$ blam-tag-shell inspect masterchief.biped --full \
    --filter velocity --filter-value 3

# Full tree as JSON
$ blam-tag-shell inspect masterchief.biped --full --json
```

A `pageable_resource` field renders the same way a struct does — its header struct walks like a normal container, so `cd "tag resource groups[0]/tag_resource"` lands inside the resource and `inspect --full` prints its fields and any nested blocks / data. Null and Xsync resources have no parsed tree, so they print just the kind summary line and stop.

---

### `get` — Read a field value

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `<PATH>` | Field path. |

| Long | Description |
|-|-|
| `--raw` | Output raw value only (no label). |
| `--json` | Emit JSON. |
| `--hex` | Output numeric values in hex. |

```sh
$ blam-tag-shell get masterchief.biped "jump velocity"
jump velocity: real = 3.08

$ blam-tag-shell get masterchief.biped "jump velocity" --raw
3.08

$ blam-tag-shell get masterchief.biped "unit/flags" --hex
```

---

### `set` — Write a field value

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `<PATH>` | Field path. |
| `<VALUE>` | Value to set. |

| Long | Description |
|-|-|
| `--output <FILE>` | Write to a different file instead of overwriting the source. |
| `--dry-run` | Preview changes without writing. |

The output reports `(was X)` so dry-runs and real runs are self-documenting.

```sh
# Float
$ blam-tag-shell set masterchief.biped "jump velocity" 99.5
set jump velocity = 99.5 (was 3.08)

# Enum by name (case-insensitive)
$ blam-tag-shell set masterchief.biped "unit/default team" covenant

# Tag reference: `GROUP:path` or `none`
$ blam-tag-shell set masterchief.biped "unit/melee damage" "jpt!:globals/melee_damage"

# Block index: integer or `none` (= -1)
$ blam-tag-shell set some.tag "block index field" none

# api_interop: `reset` writes BCS's canonical {0, UINT_MAX, 0}
#              or a comma triple (decimal or 0x…)
$ blam-tag-shell set hunter.render_model "vertex buffer interop" reset
$ blam-tag-shell set hunter.render_model "vertex buffer interop" 0xDEADBEEF,0x12345678,0

# Preview without writing
$ blam-tag-shell set masterchief.biped "jump velocity" 0.5 --dry-run
(dry run) would set jump velocity = 0.5 (was 3.08)

# Write to a different file
$ blam-tag-shell set masterchief.biped "jump velocity" 0.5 --output modified.biped
```

---

### `flag` — Get or set a flag bit

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `<PATH>` | Field path to a flags field. |
| `<FLAG_NAME>` | Flag name. |
| `[ACTION]` | `on`, `off`, or `toggle`. Omit to read. |

| Long | Description |
|-|-|
| `--output <FILE>` | Write to a different file. |
| `--dry-run` | Preview without writing. |

```sh
$ blam-tag-shell flag masterchief.biped "unit/flags" "fires from camera"
unit/flags.fires from camera = on

$ blam-tag-shell flag masterchief.biped "unit/flags" "fires from camera" off
set unit/flags.fires from camera = off (was on)

$ blam-tag-shell flag masterchief.biped "unit/flags" "fires from camera" toggle --dry-run
```

---

### `options` — List enum/flag options

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `<PATH>` | Field path to an enum or flags field. |

| Long | Description |
|-|-|
| `--json` | Emit JSON. |

```sh
# Enum — current value marked with an arrow
$ blam-tag-shell options masterchief.biped "unit/default team"
Enum options for 'unit/default team':
  0: default
  1: player <-
  2: human
  3: covenant

# Flags — checkboxes for each bit
$ blam-tag-shell options masterchief.biped "unit/flags"
Flag options for 'unit/flags':
  0: [ ] circular aiming
  1: [ ] destroyed after dying
  3: [x] fires from camera
  ...
```

---

### `block` — Block element operations

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `<PATH>` | Field path to a block. |
| `<ACTION>` | `count`, `add`, `insert`, `duplicate`, `delete`, `clear`, `swap`, or `move`. |
| `[INDEX]` | First index (`insert`/`duplicate`/`delete`, first of `swap`, from for `move`). |
| `[INDEX2]` | Second index (`swap` second, `move` to). |

| Long | Description |
|-|-|
| `--output <FILE>` | Write to a different file. |
| `--dry-run` | Preview without writing. |
| `--json` | Emit JSON (only meaningful for `count`). |

Unknown actions are rejected at parse time — `blam-tag-shell block … foo` fails with clap's `invalid value` listing the valid set.

```sh
$ blam-tag-shell block masterchief.biped "contact points" count
2

$ blam-tag-shell block masterchief.biped "contact points" add
$ blam-tag-shell block masterchief.biped "contact points" insert 0
$ blam-tag-shell block masterchief.biped "contact points" duplicate 1
$ blam-tag-shell block masterchief.biped "contact points" swap 0 1
$ blam-tag-shell block masterchief.biped "contact points" move 3 0
$ blam-tag-shell block masterchief.biped "contact points" delete 0
$ blam-tag-shell block masterchief.biped "contact points" clear --dry-run
```

---

### `deps` — List tag references

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |

| Long | Description |
|-|-|
| `--unique` | De-duplicate repeated references. |
| `--json` | Emit JSON. |

```sh
$ blam-tag-shell deps masterchief.biped
unit/object/model: objects\characters\masterchief\masterchief.model
unit/object/collision damage: globals\collision_damage\biped_player.collision_damage
…
```

References render as `<path>.<group_name>` (canonical filename form) when the group resolves in the loaded tag index, falling back to `<group_tag>:<path>` (legacy form) otherwise.

---

### `list` — Walk a directory for tags

| Argument | Description |
|-|-|
| `<DIR>` | Directory to walk (recursive). |

| Long | Description |
|-|-|
| `--group <TAG>` | Filter by group. Accepts either form: long name (`render_model`) or 4-byte group tag (`mode`). |
| `--starts-with <S>` | Only filenames starting with `S`. |
| `--contains <S>` | Only paths containing `S`. |
| `--ends-with <S>` | Only filenames ending with `S` (e.g. extension matching). |
| `--regex <PAT>` | Only full paths matching `PAT`. |
| `--from-file <F>` | Read candidate paths from `F` instead of walking. |
| `--summary` | Group tally instead of a path list. |
| `--sort-by-count` | Sort summary rows by count (desc) instead of name. |
| `--json` | Emit JSON. |

`list` is path-only — it never opens a tag file. For standalone tag files the file extension *is* the group name, so a full corpus walk runs in well under a second.

```sh
# Plain list of every biped under a tags root (long name or 4cc both work)
$ blam-tag-shell list /path/to/tags --group biped
$ blam-tag-shell list /path/to/tags --group bipd

# Group tally
$ blam-tag-shell list /path/to/tags/globals --summary
GROUP                               COUNT
--------------------------------------------
breakable_surface                       2
collision_damage                       22
damage_effect                          10
globals                                 1
wind                                    1
--------------------------------------------
14 types                               99

# JSON-sorted by most common
$ blam-tag-shell list /path/to/tags --summary --sort-by-count --json
```

---

### `find` — Search values across a directory

| Argument | Description |
|-|-|
| `<DIR>` | Directory to walk. |
| `<VALUE>` | Substring (or regex with `--regex`) to search for. |

| Long | Description |
|-|-|
| `--group <TAG>` | Only search tags of this group. |
| `--field-name <PAT>` | Only check fields whose name matches `PAT` (regex). |
| `--regex` | Interpret `<VALUE>` as a regex. |
| `--json` | Emit JSON. |
| `--strict` | Fail on any unreadable tag. |

```sh
# Which weapons reference a specific sound tag?
$ blam-tag-shell find /path/to/tags 'weapons/fire_sound' \
    --group weap --field-name 'sound'

# Regex match — all numeric velocities > 5
$ blam-tag-shell find /path/to/tags '^[5-9]\.' \
    --field-name 'velocity' --regex
```

---

### `check` — Integrity validator

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |

| Long | Description |
|-|-|
| `--tags-root <DIR>` | Tags root directory; required for tag-reference existence checks. |
| `--only <KINDS>` | Comma-separated subset: `enum`, `flag`, `real`, `reference` (default: all). |
| `--json` | Emit JSON. |
| `--strict` | Non-zero exit status on any finding (for CI). |

Surfaces:

- **Enum out of range** — the stored int didn't resolve to a named variant.
- **Unknown flag bits** — bits set without a declared name.
- **Non-finite reals** — `NaN` / `±inf` in any real field.
- **Missing tag references** — with `--tags-root`, references that don't resolve to a file on disk.

```sh
$ blam-tag-shell check floodcombat_elite.biped --tags-root /path/to/tags
[reference] unit/dialogue variants[0]/dialogue: no file with stem 'sound\dialog\combat\floodcombat_elite'

1 finding(s)

# Fail the build if anything shows up
$ blam-tag-shell check masterchief.biped --tags-root /path/to/tags --strict
```

---

### `header` — File metadata

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag or cache file. |

| Long | Description |
|-|-|
| `--json` | Emit JSON. |

```sh
$ blam-tag-shell header masterchief.biped
Tag File
  Group:         bipd
  Group version: 3
  Build:         1.1
  Version:       11707
  Checksum:      0x2858868A
  File size:     35301 bytes
  Streams:       tag!, want
```

---

### `layout-diff` — Compare tag schemas

| Argument | Description |
|-|-|
| `<FILE_A>` | First tag file. |
| `<FILE_B>` | Second tag file. |

Reports field adds / removes / type changes between two tags' schemas. For value comparison use [`data-diff`](#data-diff--compare-two-tag-values).

```sh
$ blam-tag-shell layout-diff h3/masterchief.biped reach/masterchief.biped
```

---

### `data-diff` — Compare two tag values

| Argument | Description |
|-|-|
| `<FILE_A>` | First tag file. |
| `<FILE_B>` | Second tag file. |

| Long | Description |
|-|-|
| `--only <PATH>` | Restrict both walks to a subtree. |
| `--json` | Emit JSON. |

Walks every leaf in both tags and reports `~ changed`, `- only in a`, `+ only in b`, plus a summary.

```sh
$ blam-tag-shell data-diff h3/masterchief.biped h3/floodcombat_elite.biped
~ jump velocity: 3.08 -> 4.8
~ physics/dead material name: "hard_metal_thin_hum_masterchief" -> "tough_floodflesh_combatform"
…
73 changed, 76 only in a, 6 only in b

$ blam-tag-shell data-diff a.biped b.biped --only 'unit/unit camera'
```

---

### `export` — Dump tag state as replay commands

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `[SUBTREE]` | Optional field path; only export fields under this subtree. |

| Long | Description |
|-|-|
| `--output <FILE>` | Write to a file instead of stdout. |

Emits one `set <file> <path> <value>` line per round-trippable leaf, plus a trailing comment block listing skipped types (data blobs, math composites, colors, bounds, api_interop runtime handles, etc.). Useful for diffing tag states, committing tag edits as reviewable patches, and reproducible authoring pipelines.

```sh
# Dump all settable leaves
$ blam-tag-shell export masterchief.biped > mc.cmds

# Diff two tags at field level (strip the tag-path column first)
$ blam-tag-shell export a.biped > /tmp/a.cmds
$ blam-tag-shell export b.biped > /tmp/b.cmds
$ awk '{$2="";print}' /tmp/a.cmds > /tmp/a.clean
$ awk '{$2="";print}' /tmp/b.cmds > /tmp/b.clean
$ diff -u /tmp/a.clean /tmp/b.clean

# Scope to a subtree
$ blam-tag-shell export masterchief.biped 'unit/unit camera' --output cam.cmds
```

---

### `extract-bitmap` — Bitmap to TIFF / DDS

| Argument | Description |
|-|-|
| `<FILE>` | Path to a `.bitmap` tag file. |

| Long | Description |
|-|-|
| `--format <FORMAT>` | `tif` (default — Tool-importable RGBA8 TIFF) or `dds` (debug — original pixel bytes wrapped in a DDS container). |
| `--output <PATH>` | Where to write. Path ending in `.tif` / `.tiff` / `.dds` → that exact file, with the extension picking the format and overriding `--format` (single-image tags only). Otherwise treated as a directory. Default: current directory. |

Reads pixel bytes straight from the tag's `processed pixel data` blob and emits one image per `bitmaps[]` entry. No resource cache files needed — halo3_mcc / haloreach_mcc bitmaps keep their pixels inline. Validated against 25,908 / 25,908 bitmap-tag images across both corpora — **0 failures on either output path.**

**TIFF (default)** decompresses block-compressed formats (BC1–5 + Halo's `dxn_mono_alpha`) and writes RGBA8 with the SnowyMouse libtiff field profile (`EXTRASAMPLES=UNASSALPHA`, `Photometric=RGB`, `Orientation=TOPLEFT`, contig planar). Cube maps emit a 4×3 horizontal cross (`top=+Y`, middle `+X +Z -X -Z`, `bottom=-Y`, magic-blue fill on empty cells); 2D arrays emit a vertical strip. Mip 0 only — Tool regenerates the chain on import. HDR formats (`abgrfp16`, `abgrfp32`) currently clamp `[0, 1]` × 255 → 8-bit; float-TIFF emission is deferred pending Tool acceptance verification.

**DDS (`--format dds`)** preserves original pixel bytes for inspection / debugging. Not re-importable through `tool bitmaps`.

Output naming:

- `--output <FILE>.tif|.tiff|.dds` → writes to that exact filename and uses the file's extension to pick the format. Errors on multi-image tags (one filename can't hold all of them).
- `--output <DIR>` (anything else):
  - 1-image tag → `<DIR>/<tag_stem>.<ext>`.
  - N-image tag → `<DIR>/<tag_stem>/<i>.<ext>` (per-tag subdirectory).
- Omitted → directory target = current working directory.

Format coverage:

- **TIFF**: every format below decoded → RGBA8.
- **DDS legacy** (fourcc / pixelformat masks): `dxt1`, `dxt3`, `dxt5`, `dxt5a`, `dxn`, `a8`, `y8`, `r8`, `ay8`, `a8y8`, `a4r4g4b4`, `x8r8g8b8`, `a8r8g8b8`, `v8u8`, `q8w8v8u8`, `abgrfp16`, `abgrfp32`, `a16b16g16r16`.
- **DDS DXT10 extension**: array textures of any of the above + `signedr16g16b16a16`.
- **DDS CPU-decoded to A8R8G8B8**: `dxn_mono_alpha` (BC5-shaped layout with luminance + alpha sub-blocks — port of TagTool's `DecompressDXNMonoAlpha`).

```sh
# Default — Tool-importable TIFF in cwd.
$ blam-tag-shell extract-bitmap masterchief.bitmap
masterchief.tif: 256×256 a8r8g8b8 (2D texture, 9 mips)

# Cube map → 4×3 horizontal cross TIFF.
$ blam-tag-shell extract-bitmap envmap.bitmap --output extracted/
extracted/envmap.tif: 256×256 dxt5 (cube map, 9 mips)

# Specific filename — extension picks format.
$ blam-tag-shell extract-bitmap masterchief.bitmap --output ~/Downloads/chief.tif
/Users/.../Downloads/chief.tif: 256×256 a8r8g8b8 (2D texture, 9 mips)

# Debug DDS dump.
$ blam-tag-shell extract-bitmap masterchief.bitmap --format dds --output extracted/
extracted/masterchief.dds: 256×256 a8r8g8b8 (2D texture, 9 mips)

# Multi-image (sprite atlas) → directory of TIFFs, one per atlas page.
$ blam-tag-shell extract-bitmap weapon_atlas.bitmap --output extracted/
extracted/weapon_atlas/0.tif: 512×512 dxt1 (2D texture, 10 mips)
extracted/weapon_atlas/1.tif: 256×256 dxt5 (2D texture, 9 mips)
…
```

---

### `extract-geometry` — tag to source-tree JMS / ASS files

| Argument | Description |
|-|-|
| `<FILE>` | Path to a `.model` (hlmt), `.scenario` (scnr), or `.scenario_structure_bsp` (sbsp) tag. Other groups rejected. |
| `[KINDS...]` | **`.model`-only.** Filters: `render`, `collision`, `physics`, or `all`. Default `all`. Rejected for scenario / sbsp inputs. |

| Long | Description |
|-|-|
| `--output <DIR>` | Output root directory (default: current directory). |
| `--flat` | Flatten the layout. `.model` → `<DIR>/<stem>.<kind>.<ext>`. `.scenario` → `<DIR>/<scenario>.<bsp>.ass`. No effect on direct sbsp (single file). |
| `--force <FORMAT>` | **`.model`-only.** Force render-side output to `jms` or `ass`. Defaults to content-based dispatch. Rejected for scenario / sbsp inputs (those always emit ASS). No effect on collision / physics — both always emit JMS. |

Single verb for "extract geometry source files from a tag", dispatched on input group. Replaces both the old `extract-jms` (`.model` → JMS only) and `extract-ass` (`.scenario` → ASS) verbs.

#### Inputs

**`.model` (hlmt)** — render / collision / physics children in the H3EK source-tree layout. **Render side auto-dispatches**: emits ASS when the render_model carries `instance mesh index >= 0` + populated `instance placements[]` (modular characters like the brute, decorators, level objects); JMS otherwise. Empirically across halo3_mcc: 141 / 1869 render_models trigger the ASS path under auto-dispatch.

Why two formats: ASS carries the `INSTANCES` section that round-trips back through Tool to recreate `instance mesh index` + `instance placements[]` on recompile. JMS has no `INSTANCES` section — choosing JMS for an instance-bearing render_model bakes each placement inline as N copies of the geometry, which still imports but loses the per-placement structure. ASS is structurally faithful; JMS is universally supported.

Default layout:

```
<DIR>/<stem>/render/<stem>.{JMS|ASS}      ← format auto-picked or forced
<DIR>/<stem>/collision/<stem>.JMS         ← collision BSP triangles, world-space
<DIR>/<stem>/physics/<stem>.JMS           ← Havok primitives + constraints
```

Per-format contents:

- **render JMS**: NODES + MATERIALS + MARKERS + VERTICES + TRIANGLES. Walks `regions × permutations × meshes × parts`; emits one material per `(shader, perm, region)` cell. When `instance placements[]` is populated and JMS is forced, each placement's slice of the instance mesh is baked inline as additional triangles weighted to the placement's bone (lossy round-trip vs ASS).
- **render ASS**: HEADER + MATERIALS + OBJECTS + INSTANCES (version 7). One MESH OBJECT per region/permutation cell, one MESH for the instance mesh, one SPHERE per marker, one frame INSTANCE per render_model node (bones), one INSTANCE per region/permutation MESH, **one INSTANCE per placement** (the round-trip-critical bit), one INSTANCE per marker.
- **collision JMS**: NODES + MATERIALS + VERTICES + TRIANGLES (world-space via skeleton).
- **physics JMS**: NODES + CAPSULES / BOXES / CONVEX / RAGDOLLS / HINGES (no triangle mesh).

The skeleton is built once from the model's render_model and shared across all three children. Coll and phmo place their data in world space via bone-name lookups against that skeleton.

Missing references in the .model are silently skipped with a status note.

```sh
# Auto-dispatch — brute has instance geometry → ASS render, JMS coll/phys
$ blam-tag-shell extract-geometry brute.model --output extracted/
extracted/brute/render/brute.ASS: [render: ASS]  37 mats, 96 objects, 184 instances
extracted/brute/collision/brute.JMS: [collision] 50 nodes, 7 mats, 2478 verts, 826 tris
extracted/brute/physics/brute.JMS: [physics]   50 nodes, 1 mats, 7 capsules, 3 convex, 5 ragdolls, 4 hinges

# Auto-dispatch — masterchief has no instances → JMS everywhere
$ blam-tag-shell extract-geometry masterchief.model --output extracted/
extracted/masterchief/render/masterchief.JMS: [render: JMS]  51 nodes, 20 mats, 37 markers, 18702 verts, 6234 tris
…

# Force JMS on the brute (lossy: instance pieces baked as triangles)
$ blam-tag-shell extract-geometry brute.model --force jms --output extracted/
extracted/brute/render/brute.JMS: [render: JMS]  50 nodes, 71 mats, 79 markers, 51960 verts, 17320 tris

# Force ASS on a non-instance model (valid but trivial INSTANCES section)
$ blam-tag-shell extract-geometry masterchief.model --force ass --output extracted/

# Just physics, flat layout
$ blam-tag-shell extract-geometry brute.model physics --flat --output extracted/
extracted/brute.physics.jms: [physics] 50 nodes, 1 mats, …
```

Render-side pipeline (JMS path): walks `regions × permutations × meshes × parts`, decompresses bounds-quantized positions/UVs against `compression info[0]`, converts triangle strips to lists with restart-aware parity + degenerate filtering. **Plus `instance placements[]`**: when a render_model has `instance mesh index >= 0`, each `instance placements[i]` is paired with `meshes[N].subparts[i]`, transformed by the placement's `(forward, left, up, position) × scale` basis, weighted to the placement's `node_index`, and emitted as additional triangles with a unique `(slot) <placement_name>` material. Foundry mirrors this; TagTool drops it for non-decorator types.

Render-side pipeline (ASS path): same regions/permutations walk emits one MESH OBJECT per cell. One frame INSTANCE per render_model node provides the bone armature scaffolding (parent_id chains through the node hierarchy). The instance mesh becomes a single MESH OBJECT and each placement spawns an INSTANCE of it with the placement's basis matrix, parented to the placement's bone — preserving the round-trip back through Tool. Markers emit as SPHERE OBJECTs with `#<group>` instance names parented to their bone.

Collision-side pipeline: walks each BSP's `surfaces[]` via the edge-ring algorithm, fan-triangulates each ring, emits world-space vertices through the render_model skeleton. Physics-side pipeline: emits Havok shape primitives (sphere / box / pill / polyhedron) plus ragdoll / hinge constraints in world-space.

Validated across the H3 MCC corpus: 4354 / 4354 JMS reconstructions; 89.7% bbox match against embedded source JMS, 86.8% with ≥99% position coverage at 10 cm precision.

Notes:

- ASS render-side output is structurally compile-time-equivalent to the runtime tag, not the artist source. The original `.ass` source files have many more named MESH OBJECTs because each artist piece kept its own name; Tool consolidates these into the runtime tag's region/permutation grouping. We can reconstruct the consolidated form, not the artist-level granularity.
- Material `material_name` follows the `(slot) <perm> <region>` H3 Blender exporter convention. The `(N)` slot value is a deterministic 1-based counter — the original artist counter from `bpy.data.materials` is round-trip metadata only and unrecoverable from the tag.
- Transparent parts (`part_type=4`) on the JMS path emit both face windings, matching what MCC's importer baked from the artist's `%` two-sided directive. Same as TagTool.
- For tag-direct extraction (skip the .model wrapper), call the library functions `JmsFile::from_render_model` / `AssFile::from_render_model` / `JmsFile::from_collision_model_with_skeleton` / `from_physics_model_with_skeleton` directly.

#### `.scenario` (scnr) input

Walks `scenario.structure_bsps[]` and emits one ASS per entry, pairing each `scenario_structure_bsp` with its `scenario_structure_lighting_info`. Always ASS — JMS has no representation for level/BSP geometry. `[KINDS...]` and `--force` are rejected.

Default layout: `<DIR>/<scenario_stem>/structure/<bsp_stem>.ASS` (re-importable as artist source).

Sections emitted per BSP:
- **HEADER** — version 7 + tool/user/machine placeholders
- **MATERIALS** — one per `materials[]` entry on the sbsp, with `BM_FLAGS` / `BM_LMRES` (real lightmap-resolution from the material `properties[type=0]`) plus `BM_LIGHTING_BASIC` / `_ATTEN` / `_FRUS` strings layered in for emissive materials from the paired `.stli`. Auto-appended `+portal` / `+weather` / `@collision_only` marker materials so Tool.exe re-extracts the recompile-only categories back into their proper tag blocks.
- **OBJECTS** — cluster MESHes (one per `clusters[]`), per-IGD-def MESHes (one per `instanced geometries definitions[]`, content-deduped, each in its own per-definition compression bounds), `+portal_N` MESHes (one per cluster portal, fan-triangulated), `+weather_N` MESHes (convex hull recovered from the polyhedron plane set), `@CollideOnly` MESH (merged structure collision BSP via the same edge-ring walker as the model path), SPHERE primitives for sbsp markers (matching the H3 source-tree `frame construct` convention), GENERIC_LIGHT (SPOT/DIRECT/OMNI/AMBIENT) entries from the `.stli` definitions, and xref-only OBJECTs for `environment_objects[]` palette entries.
- **INSTANCES** — Scene Root parent (object_index = -1), one identity-transform instance per cluster, one instance per `instanced geometry instances[]` placement (3-vec3 rotation matrix → quaternion, position × 100 cm, uniform scale), one instance per portal / weather polyhedron / marker / collision BSP / environment_object placement, and one instance per stli `generic_light_instances[]` entry (forward+up → quat, position × 100 cm).

Validated across 147 / 147 BSPs across 49 H3 scenarios — every BSP produces a clean ASS file. Source ASS files have a different mesh granularity (artist-named meshes vs cluster aggregates); that's compile-time information the tag doesn't carry.

```sh
$ blam-tag-shell extract-geometry levels/multi/construct/construct.scenario --output extracted/
extracted/construct/structure/construct.ASS: [bsp0] 97 mats, 203 objects (14 lights), 687 instances, 111617 verts, 67625 tris
```

#### `.scenario_structure_bsp` (sbsp) input

Direct sbsp input emits a single ASS for that BSP. **No paired stli** — lighting is unreachable without scenario context, so `GENERIC_LIGHT` objects and `BM_LIGHTING_*` material metadata are absent. Use `.scenario` input if you need lights. Output: `<DIR>/<sbsp_stem>.ASS` (no nesting, single file). `--flat` is a no-op.

```sh
$ blam-tag-shell extract-geometry levels/multi/construct/construct.scenario_structure_bsp --output /tmp/
/tmp/construct.ASS: [sbsp] 97 mats, 189 objects, 673 instances, 111617 verts, 67625 tris (no lighting — pass scenario for lights)
```

---

### `extract-data` — Dump a `tag_data` field

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file. |
| `<PATH>` | Field path resolving to a `tag_data` leaf. |

| Long | Description |
|-|-|
| `--output <PATH>` | Output file path. Default: `<tag_stem>.<field_name>.bin` in cwd (field-name non-alphanumerics replaced with `_`). |

Writes the raw bytes of one `tag_data` field to a file for inspection. Errors with the field's actual type when the path resolves to a non-`tag_data` leaf, and uses the same "did you mean?" suggester as `get` for unknown paths.

```sh
$ blam-tag-shell extract-data masterchief.bitmap "processed pixel data"
masterchief.processed_pixel_data.bin: 32760 bytes

# Pull the raw codec stream out of one animation's resource group_member
$ blam-tag-shell extract-data elite.model_animation_graph \
    "tag resource groups[0]/tag_resource/group_members[0]/animation_data" \
    --output /tmp/elite0.bin
/tmp/elite0.bin: 106860 bytes

# Wrong-type leaf surfaces clearly
$ blam-tag-shell extract-data elite.model_animation_graph "definitions/animations"
Error: field 'definitions/animations' is block (not a `tag_data` field)
```

---

### `extract-import-info` — Unzip source files from the `info` stream

| Argument | Description |
|-|-|
| `<FILE>` | Path to a tag file with an `info` (tag_import_information) stream. |

| Long | Description |
|-|-|
| `--output <DIR>` | Output directory. Default: `./<tag_stem>/import_info/`. |
| `--list` | Print the manifest only — don't decompress or write. |

The `info` stream's `files[]` block carries one entry per source file Bungie's importer consumed when the tag was built — original on-disk path, modification date, CRC32, byte size, plus the source file's bytes zlib-compressed (`tag_import_file_zipped_data_definition`, capped at 160 MiB / file). This command walks `files[]` and writes each entry's decompressed payload to disk under a sanitized form of its original path (drive letter stripped, backslashes normalized).

Useful for verifying which artist source actually fed a runtime tag, recovering source-of-truth fidelity that runtime-tag extractors can't reach (e.g. comparing `extract-geometry` output to the original `.ass` the render_model was built from), or pulling the importer log out of `events[]`.

What carries an `info` stream in the H3 corpus:

| Tag group | Source files |
|---|---|
| `render_model` | typically 1 `.ass` (full Max scene). E.g. `brute.render_model` → `brute.ass` (~5 MB). |
| `collision_model` | 1 `.jms`. |
| `physics_model` | 1 `.jms` (e.g. `brute_ragdoll.jms`). |
| `bitmap` | source TIFF/DDS — but MCC ships `source data: 0 bytes`, so usually empty. |
| `model_animation_graph` / `biped` / `model` / `weapon` / etc. | none — composite tags built from refs to other tags. |

Rule of thumb: leaf-asset tags (mesh / coll / phys / bitmap) carry their source files; composite tags don't.

```sh
# List manifest only
$ blam-tag-shell --game halo3_mcc extract-import-info /path/to/brute.render_model --list
tag built tool  test pc 11138.07.07.06.0905.main  Jul  6 2007 09:14:12 by shikaiw (build=Some(11138))
  [  0] data\objects\characters\brute\render\brute.ass (862380/4850221 bytes, crc32=c478cbe6)
1 files: 0 written, 0 failed; 862380 bytes compressed → 0 bytes decompressed

# Default — decompress + write to ./<tag_stem>/import_info/<sanitized path>
$ blam-tag-shell --game halo3_mcc extract-import-info /path/to/brute.render_model
  brute.render_model/import_info/data/objects/characters/brute/render/brute.ass (862380 → 4850221 bytes)
1 files: 1 written, 0 failed; 862380 bytes compressed → 4850221 bytes decompressed

# Pick output dir — drops the brute.ass into /tmp directly
$ blam-tag-shell --game halo3_mcc extract-import-info /path/to/brute.render_model --output /tmp/brute_src
```

Errors out cleanly when the tag has no `info` stream (most composite tags). Use `add-import-info` to attach an empty one if you're authoring a tag from scratch and intend to populate the manifest.

---

### `list-animations` — Enumerate jmad animations

| Argument | Description |
|-|-|
| `<FILE>` | Path to a `.model_animation_graph` tag file. |

| Long | Description |
|-|-|
| `--json` | Emit JSON (full per-animation metadata + `data sizes` breakdown). |

Walks `definitions/animations` (or `resources/animations` for older inline-layout tags) and prints one row per animation. Header-only — no codec decode. Inheriting jmads (zero local animations + non-null `parent animation graph`) print a one-line "(no animations — inherits from …)" notice.

Columns: `idx` (animation index), `cdc` (first byte of the static codec stream), `frame` (engine-recorded frame count), `node` (engine-recorded node count), `type` (animation type — `base` / `overlay` / `replacement` / …), `movement` (frame info type — `none` / `dx,dy` / `dx,dy,dyaw` / …), `blob` (raw `animation_data` byte count), `name` (resolved string-id name).

```sh
$ blam-tag-shell list-animations masterchief.model_animation_graph
  idx  cdc  frame node  type        movement             blob  name
    0    1    121   55  base        dx,dy                106860  any:any:any:morph
    1    1    143   55  base        none                 115276  any:any:infection_wrestle
    …

# Inheriting tag
$ blam-tag-shell list-animations objects/.../foo.model_animation_graph
(no animations — inherits from objects/.../parent.model_animation_graph)
```

---

### `extract-animation` — Decode and export an animation

| Argument | Description |
|-|-|
| `<FILE>` | A `.model_animation_graph` (jmad), a `.model` (hlmt), or any object-inheriting tag (`.biped`, `.weapon`, `.scenery`, `.equipment`, `.vehicle`, …) that points at a `.model`. |
| `[ANIM]` | Animation index (`definitions/animations[N]`) or resolved string-id name. **Optional** — omit to extract every animation in the tag. |

| Long | Description |
|-|-|
| `--output <PATH>` | Source-tree root. Files land at `<root>/<tag_stem>/animations/<anim_name>.<EXT>`. Default root is `.`. See semantics below. |
| `--flat` | Flatten the layout: emit `<root>/<tag_stem>.<anim_name>.<EXT>` instead of nested subdirs. Mirrors [`extract-geometry --flat`](#extract-geometry--model-to-source-tree-jms--ass-files). Ignored when `--output` is an exact filename. |
| `--format <FMT>` | `jma` (default) or `json`. |

Decodes one or every animation's static + animated codec streams + per-bone flag bitarrays + per-frame movement, composes them against the tag's skeleton, and writes the result.

The output layout matches what [Tool's `model-animations` command](https://c20.reclaimers.net/h3/h3-ek/h3-tool/#model-animations) consumes — `<source-directory>/animations/*.JM*` — so the result drops straight into an H3EK source tree alongside [`extract-geometry`](#extract-geometry--model-to-source-tree-jms--ass-files)'s `render/` output. Pointing both verbs at the same `--output <root>` produces a Tool-importable tree at `<root>/<tag_stem>/`:

```text
<root>/<tag_stem>/
  render/<tag_stem>.{JMS|ASS}   # extract-geometry
  collision/<tag_stem>.JMS      # extract-geometry
  physics/<tag_stem>.JMS        # extract-geometry
  animations/<anim_name>.<EXT>  # extract-animation, one file per anim
```

`--format jma` writes a JMA-family text file (`.JMM/.JMA/.JMT/.JMZ/.JMO/.JMR/.JMW`), kind picked from the animation's `animation type` × `frame info type` × `internal flags / world relative` (JMW = base + world-relative bit). Translations are scaled `× 100` (cm convention) and quaternions written as the conjugate `(-i, -j, -k, w)`. The H3 JMA spec (version 16392) has **no separate per-frame movement section** — for movement-bearing kinds (JMA/JMT/JMZ) the writer folds accumulated `dx/dy/dz/dyaw` deltas into the root bone (index 0) at each frame, with `dx/dy` rotated from local space into world space by the accumulated yaw per Foundry commit `850d680d`. Verified against `General-101/Halo-Asset-Blender-Development-Toolset` reader/writer and TagTool's `Animation.Process()` / `Export()`.

Each on-disk JMA carries `codec_count + 1` frames per Tool's importer convention:
- **Base (JMM/JMA/JMT/JMZ) and JMW**: codec frames + a duplicated trailing held frame (Tool's blend-into-next-anim convention).
- **Replacement (JMR)**: a leading rest-pose frame, then codec frames.
- **Overlay (JMO)**: a leading rest-pose frame, then per-frame composed `(rest_rotation × codec_delta_rotation, rest_translation + codec_delta_translation, rest_scale + codec_delta_scale)`.

`--format json` dumps both static and animated tracks plus the animated-stream status — useful for diagnostics and for codecs not yet wired into the JMA writer.

#### Input dispatch

| Input | Resolution |
|-|-|
| `.model_animation_graph` (jmad) | Used directly. Rest pose comes from the jmad's own `additional node data` block — the only source available without a render_model in scope. |
| `.model` (hlmt) | Follow the `animation` ref to load the jmad and the `render model` ref to load the render_model. Rest pose is built from `render_model.nodes[]` (authoritative), with `additional node data` filling in synthetic nodes the render_model lacks (e.g. `camera_control`). |
| Object-inheriting (`.biped`, `.vehicle`, `.weapon`, `.equipment`, `.scenery`, `.crate`, …) | Follow the inherited `model` ref to a `.model`, then the `.model` case. |

For first-person weapon animations specifically, pass the FP `.model_animation_graph` directly — its `additional node data` block already merges arms + gun rest poses. (Resolving FP via `.weapon` would only give you the gun render_model, missing the arms.)

Source priority for each bone's rest pose: render_model nodes (when available) → `additional node data` → identity. Per the Foundry maintainer, render_model values are authoritative; additional_node_data is a denormalized cache that can drift on rare occasions.

Verified across 36,270 / 36,270 H3 + Reach MCC animations.

#### `--output` semantics

| `--output` | Behavior |
|-|-|
| omitted | Root is `.` — files land at `./<tag_stem>/animations/<anim_name>.<EXT>`. Single-anim `json` still prints to stdout for piping into `jq`. |
| ends in `.jmm/.jma/.jmt/.jmz/.jmo/.jmr/.jmw/.json` | Treated as an exact filename, bypassing the source-tree layout. **Single-anim only** — multi-anim + filename is rejected. |
| any other path (existing dir, trailing `/`, or unrecognized extension) | Becomes the source-tree root. Files land at `<root>/<tag_stem>/animations/<anim_name>.<EXT>`. |

When two animations would resolve to the same output path (e.g. `walk fast` and `walk-fast` both sanitize to `walk_fast.JMA`), the command bails with both anim indexes instead of silently overwriting.

```sh
# Bulk default → ./masterchief/animations/
$ blam-tag-shell extract-animation masterchief.model_animation_graph
masterchief/animations/any_any_any_morph.JMA: 122 frames (121+1) × 55 bones [JMA]  movement=DxDy
masterchief/animations/any_idle.JMM: 31 frames (30+1) × 55 bones [JMM]  movement=None
...

# Single anim by index → same source-tree layout
$ blam-tag-shell extract-animation masterchief.model_animation_graph 0
masterchief/animations/any_any_any_morph.JMA: 122 frames (121+1) × 55 bones [JMA]  movement=DxDy

# Single anim by string-id name
$ blam-tag-shell extract-animation masterchief.model_animation_graph "any:any:any:morph"
masterchief/animations/any_any_any_morph.JMA: 122 frames (121+1) × 55 bones [JMA]  movement=DxDy

# Exact output filename (bypasses the source-tree layout; single-anim only)
$ blam-tag-shell extract-animation brute.model_animation_graph 139 \
    --output /tmp/brute_melee.JMT
/tmp/brute_melee.JMT: 38 frames (37+1) × 50 bones [JMT]  movement=DxDyDyaw

# Choose a different source-tree root
$ blam-tag-shell extract-animation brute.model_animation_graph --output build/
build/brute/animations/idle.JMM: 31 frames (30+1) × 50 bones [JMM]  movement=None
build/brute/animations/melee.JMT: 38 frames (37+1) × 50 bones [JMT]  movement=DxDyDyaw
...

# Tool round-trip: extract render + animations into the same source tree, import.
$ blam-tag-shell extract-geometry  brute.model            --output data/
$ blam-tag-shell extract-animation brute.model_animation_graph --output data/
$ tool model-animations brute    # consumes data/brute/render/ + data/brute/animations/

# Single-anim JSON → stdout (pipe-friendly)
$ blam-tag-shell extract-animation elite.model_animation_graph 0 --format json | jq '.frame_count'
121

# --flat for ad-hoc inspection — drop everything next to each other
$ blam-tag-shell extract-animation brute.model_animation_graph --flat
brute.idle.JMM: 31 frames (30+1) × 50 bones [JMM]  movement=None
brute.melee.JMT: 38 frames (37+1) × 50 bones [JMT]  movement=DxDyDyaw
...
```

---

### Optional-stream commands

Six commands manage the three optional streams (`want`, `info`, `assd`) that can hang off a tag file after the mandatory `tag!` stream. All follow the same shape: load the tag, attach/remove/rebuild the stream, write back (or to `--output`).

The attach/rebuild commands need the matching stream schema, which they resolve from the global `--game` flag:

- `tag_dependency_list.json` for `want`
- `tag_import_information.json` for `info`
- `asset_depot_storage.json` for `assd`

| Command | Positionals | Description |
|-|-|-|
| `add-dependency-list` | `<FILE>` | Attach an empty `want` stream. No-op if already present. |
| `remove-dependency-list` | `<FILE>` | Drop `want` if present. |
| `rebuild-dependency-list` | `<FILE>` | Walk the tag's data, collect every non-null non-`impo` `tag_reference`, and write one entry per ref (flags=0) into `want`. Creates the stream first if missing. Matches authoring-toolset output exactly for 98.8% of real tags. |
| `add-import-info` | `<FILE>` | Attach an empty `info` stream. Caller populates build / version / culprit / import date / files / events fields via `set` if needed. |
| `remove-import-info` | `<FILE>` | Drop `info` if present. |
| `add-asset-depot-storage` | `<FILE>` | Attach an empty `assd` stream (tag-editor icon pixel data). Zero presence in the observed H3/Reach corpus. |
| `remove-asset-depot-storage` | `<FILE>` | Drop `assd` if present. |

Every command takes `--output <FILE>` to write elsewhere instead of overwriting the source.

```sh
$ blam-tag-shell --game halo3_mcc new biped
created biped.biped from definitions/halo3_mcc/biped.json

$ blam-tag-shell --game halo3_mcc add-dependency-list biped.biped
attached empty dependency-list stream

$ blam-tag-shell --game halo3_mcc rebuild-dependency-list biped.biped
rebuilt dependency-list (0 entries)

$ blam-tag-shell --game halo3_mcc header biped.biped
  ...
  Streams:       tag!, want
```

Typical authoring flow:

```sh
# Set --game once via the alias to keep examples short.
shopt -s expand_aliases
alias bts='blam-tag-shell --game halo3_mcc'

# 1. Create the tag.
bts new biped --output mychief.biped

# 2. Populate the data via `set` / `flag` / `block`.
bts set mychief.biped "jump velocity" 3.2
bts set mychief.biped "unit/melee damage" \
    "jpt!:globals/melee_damage"
# …

# 3. Build the dependency list from the references we just wrote.
bts rebuild-dependency-list mychief.biped
```

---

## Field paths

Fields are addressed by `/`-separated paths. Block and array elements use `[index]` notation:

```
"jump velocity"                      # root-level field
"unit/flags"                         # inline struct -> field
"unit/seats[0]/flags"                # struct -> block element -> field
"regions[2]/permutations[0]/name"    # nested block elements
```

An optional `Type:` prefix on a segment disambiguates fields that share a name across types (`Block:regions`, `Struct:definitions[0]`). Type filters are case-insensitive; field names are case-sensitive. Element indices default to `0` when omitted.

## Fuzzy matching

Mistyped field names get suggestions:

```sh
$ blam-tag-shell get masterchief.biped "jmup velocity"
Error: field 'jmup velocity' not found. Did you mean 'jump velocity'?
```

## JSON output

Most commands support `--json` for machine-readable output, compatible with `jq`:

```sh
$ blam-tag-shell inspect masterchief.biped --json --depth 2 | jq '.[0].name'
"unit"

$ blam-tag-shell list /path/to/tags --summary --json | jq '.[] | select(.count > 100)'

$ blam-tag-shell check floodcombat_elite.biped --tags-root /path/to/tags --json \
    | jq '.[] | select(.kind == "reference")'
```
