# blam-tags

A Rust library for reading, writing, and editing Halo 3 / Reach tag files.
No ManagedBlam, no .NET, no engine needed — the parser reads each tag's
embedded layout chunk and interprets its bytes directly.

**Byte-exact roundtrip validated across every tag in the Halo 3, Halo 3:
ODST, Halo Reach, Halo 4, and Halo 2: Anniversary MP MCC corpora.** Read a
tag, write it back, md5-compare — zero differences. Locally verified on
the 119,432-tag H3 + Reach subset; full-corpus validation (including H4
and H2A MP) contributed by the community.

Four group-specific extractors sit on top of the generic tag tree:
- [`bitmap`](#bitmap--tiff--dds-extraction): `.bitmap` → TIFF (Tool-importable, default) or DDS (debug).
- [`animation`](#animation-jmad--jma-family): `.model_animation_graph` → JMA-family text files.
- [`jms`](#jms-render--collision--physics--jms): `.render_model` / `.collision_model` / `.physics_model` → JMS files.
- [`ass`](#ass-scenario_structure_bsp--ass): `.scenario_structure_bsp` (+ `.scenario_structure_lighting_info`) → ASS files.

## Quick start

```rust
use blam_tags::TagFile;

let mut tag = TagFile::read("masterchief.biped")?;

// Read a field by slash-separated path. `value()` returns the
// per-variant `TagFieldData` enum — pattern-match on it for typed
// access, or use `{:?}` for a Debug dump. The library does **not**
// ship a Display impl; UI rendering is the caller's job.
let jump = tag.root().field_path("jump velocity").unwrap();
println!("{} ({}): {:?}", jump.name(), jump.type_name(), jump.value().unwrap());

// Toggle a flag by name.
tag.root_mut()
    .field_path_mut("unit/flags").unwrap()
    .flag_mut("has_hull").unwrap()
    .toggle();

tag.write("masterchief.biped.edited")?;
```

## Common tasks

### Read a field

```rust
use blam_tags::TagFieldData;

let tag = TagFile::read("path/to/tag.biped")?;
let field = tag.root().field_path("jump velocity").unwrap();

// Schema metadata.
println!("{} : {}", field.name(), field.type_name());

// Parsed value. Returns None for container / padding fields.
// The library has no Display impl on TagFieldData — pattern-match
// for typed access, or use `{:?}` for a Debug dump.
match field.value() {
    Some(TagFieldData::Real(v)) => println!("  value = {v}"),
    Some(TagFieldData::LongInteger(v)) => println!("  value = {v} (0x{v:08X})"),
    Some(other) => println!("  value = {other:?}"),
    None => println!("  (container or padding)"),
}
```

### Walk all fields of the root struct

```rust
for field in tag.root().fields() {
    println!("{}: {}", field.name(), field.type_name());
}
```

`fields()` skips padding / explanations / terminators / unknowns. Use
`TagStructDefinition::fields()` (see below) if you need the raw walk.

### Mutate a scalar field

```rust
use blam_tags::TagFieldData;

tag.root_mut()
    .field_path_mut("jump velocity").unwrap()
    .set(TagFieldData::Real(3.14))?;

tag.write("edited.biped")?;
```

### Toggle, set, and read flag bits by name

```rust
let mut field = tag.root_mut().field_path_mut("unit/flags").unwrap();

// Per-bit operations.
field.flag_mut("has_hull").unwrap().set(true);
field.flag_mut("airborne").unwrap().toggle();

// Enumerate all bits (names + state).
if let Some(blam_tags::TagOptions::Flags(bits)) = field.as_ref().options() {
    for bit in bits {
        println!("  [{}] {}  (bit {})", if bit.is_set { "x" } else { " " }, bit.name, bit.bit);
    }
}
```

### Block element operations

```rust
let mut seats = tag.root_mut()
    .field_path_mut("unit/seats").unwrap()
    .as_block_mut().unwrap();

let new_index = seats.add_element();        // append default-initialized element
seats.insert_element(0)?;                   // insert default element at index 0
seats.duplicate_element(0)?;                // copy element 0, placed at index 1
seats.swap_elements(0, 3)?;                 // exchange elements 0 and 3
seats.move_element(5, 1)?;                  // relocate element 5 to index 1
seats.delete_element(2)?;                   // remove element 2
seats.clear();                              // remove all
println!("now have {} seats", seats.len());
```

### Walk all elements of a block, mutating as you go

```rust
let mut regions = tag.root_mut()
    .field_path_mut("regions").unwrap()
    .as_block_mut().unwrap();

regions.for_each_element_mut(|mut region| {
    if let Some(mut name) = region.field_mut("name") {
        // …inspect, edit, whatever.
    }
});
```

Visitor-closure form because each yielded handle reborrows through `self` — Rust's borrow checker rules out simultaneous mutable iterators. `TagStructMut::for_each_field_mut` and `TagArrayMut::for_each_element_mut` follow the same shape.

### Read or scrub an api_interop field

`api_interop` leaves carry a 12-byte runtime handle — BCS zeros them on save to `{ descriptor: 0, address: UINT_MAX, definition_address: 0 }`.
Typically you'll either read them for introspection or reset them before committing a tag.

```rust
use blam_tags::{ApiInteropData, TagFieldData};

// Read.
let field = tag.root().field_path("vertex buffer interop").unwrap();
if let Some(TagFieldData::ApiInterop(i)) = field.value() {
    println!("descriptor=0x{:08X} address=0x{:08X} defaddr=0x{:08X}",
        i.descriptor().unwrap_or(0),
        i.address().unwrap_or(0),
        i.definition_address().unwrap_or(0));
}

// Scrub to BCS's reset pattern before saving.
tag.root_mut()
    .field_path_mut("vertex buffer interop").unwrap()
    .set(TagFieldData::ApiInterop(ApiInteropData::reset()))?;
```

### Inspect the schema (definitions)

The library exposes a second facade rooted at `tag.definitions()` for schema traversal without going through instance data.

```rust
let root = tag.definitions().root_struct();
println!("root struct: {} ({} bytes)", root.name(), root.size());

for field in root.fields() {
    println!("  {} @ {} : {}", field.name(), field.offset(), field.type_name());
    if let Some(block_def) = field.as_block() {
        println!("    block of {} (max {})",
            block_def.struct_definition().name(),
            block_def.max_count());
    }
}
```

From an instance you can always jump to its schema — `tag_struct.definition()`, `tag_field.definition()`, `tag_block.definition()`, `tag_array.definition()`.
`TagFieldDefinition::as_api_interop()` returns the `TagApiInteropDefinition` for interop fields, exposing the linked descriptor struct, a stable 16-byte guid, and the declared type name.

### Create a new tag from a schema

Schemas live under `definitions/<game>/<group>.json`, dumped from the engine DLLs by IDAPython scripts. Each dump's `_meta.json` records which tool produced it:

- guerilla.exe: `halo3_mcc`, `halo3odst_mcc`
- sapien.exe: `haloreach_mcc`, `halo4_mcc`, `halo2amp_mcc`

Fresh sapien dumps need two idempotent post-processing passes before they'll load — see [`examples/dedupe_definitions.rs`](./examples/dedupe_definitions.rs) and [`examples/inline_parent_struct.rs`](./examples/inline_parent_struct.rs).

The library builds a zero-filled tag directly from a schema:

```rust
use blam_tags::TagFile;

let mut tag = TagFile::new("definitions/halo3_mcc/biped.json")?;
// tag has: header with group_tag='bipd', signature='BLAM', checksum=0.
// tag_stream has: one zero-filled root element with default sub-chunks
// (empty blocks, null tag_references, reset api_interops).

tag.write("my_biped.biped")?;
```

`TagFile::new` validates every struct's computed size against the dumped `size` field. If the computed sum is short, it resolves any `tmpl` custom fields in that struct by loading the target group's sibling JSON, walks the target's parent chain, and adds each ancestor's root-struct size — matching Reach's factored shader layout (where `shader_decal_struct_definition` is 4 bytes of decal-specific data and `render_method_struct_definition` is inlined via the `tmpl` custom to supply the 100 bytes of common shader fields). H3 schemas keep the common fields inlined directly, so no expansion kicks in and the size check passes as-is.

A helper example validates every dumped schema against a sample real tag:

```sh
cargo run --release -p blam-tags --example schema_match -- \
    definitions/halo3_mcc /path/to/halo3_mcc/tags
```

### Bitmap → TIFF / DDS extraction

`Bitmap::new(&tag)` wraps a parsed `.bitmap` tag and exposes per-image metadata + a sliced view of the inline pixel buffer. Each `BitmapImage` writes itself out as either a Tool-importable TIFF (default) or a debug DDS:

```rust
use blam_tags::{Bitmap, TagFile};
use std::fs::File;
use std::io::BufWriter;

let tag = TagFile::read("masterchief.bitmap")?;
let bitmap = Bitmap::new(&tag)?;

for (i, image) in bitmap.iter().enumerate() {
    // RGBA8 TIFF — re-importable through `tool bitmaps`. Cube maps
    // emit a 4×3 horizontal cross; arrays emit a vertical strip.
    let mut tif = BufWriter::new(File::create(format!("face_{i}.tif"))?);
    image.write_tiff(&mut tif)?;

    // Or the original-bytes DDS for inspection.
    let mut dds = BufWriter::new(File::create(format!("face_{i}.dds"))?);
    image.write_dds(&mut dds)?;
}
```

The submodules under `blam_tags::bitmap` separate the concerns:

| Module | Role |
|---|---|
| `bitmap::format` | `BitmapFormat` enum (20 variants), `BitmapCurve`, predicates (`is_compressed`, `is_signed`, `is_hdr`), bits-per-pixel + mip-chain math. |
| `bitmap::decode` | `decode_to_rgba8(format, w, h, &input) -> Vec<u8>` — single-mip RGBA8 decode for every supported format. BC1–5 via `bcdec_rs`; uncompressed via hand-rolled byte-order + signed-bias + half-float paths. |
| `bitmap::layout` | `compose_cube_cross` (DX horizontal-cross with magic-blue fill on empty cells) + `compose_layer_strip` (vertical strip for arrays / 3D). |
| `bitmap::tiff` | `write_rgba8_tiff` with the SnowyMouse libtiff profile (`EXTRASAMPLES=UNASSALPHA`, `Photometric=RGB`, `Orientation=TOPLEFT`, etc). |
| `bitmap::dds` | Legacy DDS writer — kept for `--format dds` inspection. |

Format coverage (validated against 25,908 / 25,908 bitmap-tag images across halo3_mcc + haloreach_mcc; **0 failures** on either output path):

| Path | Formats |
|---|---|
| **TIFF (default)** decoded RGBA8 | `dxt1` `dxt3` `dxt5` `dxt5a` `dxn` `dxn_mono_alpha` `a8` `y8` `r8` `ay8` `a8y8` `a4r4g4b4` `x8r8g8b8` `a8r8g8b8` `v8u8` `q8w8v8u8` `abgrfp16` `abgrfp32` `a16b16g16r16` `signedr16g16b16a16` (HDR formats clamp `[0, 1]`) |
| **DDS legacy** (fourcc / pixelformat masks) | every uncompressed + DXT format above |
| **DDS DXT10 extension** (`arraySize`, DXGI format) | array textures of any of the above + `signedr16g16b16a16` |
| **DDS CPU-decoded → A8R8G8B8** | `dxn_mono_alpha` (BC5-shaped, mono+alpha semantics — port of TagTool's `DecompressDXNMonoAlpha`) |

Pure-tag-file: pixels come from the top-level `processed pixel data` blob, no resource cache lookup. Errors surface as `BitmapError::PixelSliceOutOfBounds` / `FormatNotSupported` / `UnsupportedTextureType` / `Tiff` / `TiffLayoutDeferred`.

Corpus-wide validators in [`examples/extract_bitmap_sweep.rs`](examples/extract_bitmap_sweep.rs) (DDS path) and [`examples/extract_tiff_sweep.rs`](examples/extract_tiff_sweep.rs) (TIFF path):

```sh
cargo run --release -p blam-tags --example extract_tiff_sweep -- \
    /path/to/halo3_mcc/tags /path/to/haloreach_mcc/tags
```

#### Mip / layout caveats

- **Mip 0 only.** Tool re-imports re-generate the mip chain; emitting all mips would be redundant.
- **Cube maps** → 4×3 horizontal cross with face order `top=+Y`, middle `+X +Z -X -Z`, `bottom=-Y`. Empty cells are Bungie's color-plate magic blue (`R=0, G=0, B=255`).
- **Sprite-sheet tags** preserve atlas pages verbatim — one TIFF per `bitmaps[]` entry. Tool round-trip via color-plate reconstruction is not implemented; sprite metadata in `manual_sequences[]` is preserved by the source tag, not in the TIFF.
- **HDR** float formats (`abgrfp16`, `abgrfp32`) currently clamp `[0, 1]` × 255 → 8-bit. Float-TIFF emission is on the deferred list pending Tool acceptance verification.

### Animation (jmad → JMA-family)

`Animation::new(&tag)` walks a `model_animation_graph` and pairs each user-facing entry in `definitions/animations` with its `tag resource groups[r]/tag_resource/group_members[m]` runtime payload. Each `AnimationGroup` carries header metadata + the raw `animation_data` blob; call `decode()` to turn the blob into an `AnimationClip` with separately decoded static + animated tracks, per-bone flag bitarrays, and per-frame movement deltas. Compose against a `Skeleton` via `clip.pose(skel, defaults)` (where `defaults` is the per-bone rest pose, typically read from the render_model's `nodes[]` block) and emit a `.JMM/.JMA/.JMT/.JMZ/.JMO/.JMR/.JMW` text file with `pose.write_jma`:

```rust
use blam_tags::{Animation, JmaKind, NodeTransform, Skeleton, TagFile};
use std::fs::File;
use std::io::BufWriter;

let tag = TagFile::read("masterchief.model_animation_graph")?;
let animation = Animation::new(&tag)?;
let skeleton = Skeleton::from_tag(&tag);
// Caller supplies per-bone rest pose (render_model defaults +
// jmad's `additional node data` fallback). Identity stand-in here
// for brevity — see `blam-tag-shell/src/commands/extract_animation.rs`
// for a full builder.
let defaults: Vec<_> = (0..skeleton.len()).map(|_| NodeTransform::IDENTITY).collect();

for group in animation.iter() {
    let clip = group.decode()?;
    let kind = JmaKind::from_metadata(
        group.animation_type.as_deref(),
        group.frame_info_type.as_deref(),
        group.world_relative,  // base + this bit → JMW
    );
    // Overlay anims should compose `rest × codec_delta`; pass `None`
    // so unflagged bones decode as identity, then the writer's
    // overlay composition produces the rest pose.
    let pose_defaults = if kind.composes_overlay() { None } else { Some(&defaults[..]) };
    let pose = clip.pose(&skeleton, pose_defaults);
    let path = format!("{}.{}", group.name.as_deref().unwrap_or("anim"), kind.extension());
    let mut out = BufWriter::new(File::create(path)?);
    pose.write_jma(&mut out, &skeleton, &defaults, group.node_list_checksum, kind, "actor", Some(&clip.movement))?;
}
```

The writer applies the type-specific JMA layout Tool's importer expects:
- **Base (JMM/JMA/JMT/JMZ) + JMW**: codec frames + a duplicated trailing held frame.
- **Replacement (JMR)**: a leading rest-pose frame, then codec frames.
- **Overlay (JMO)**: a leading rest-pose frame, then per-frame composed `(rest_rotation × codec_delta_rotation, rest_translation + codec_delta_translation)`.

Movement deltas (JMA/JMT/JMZ) are folded into the **root bone** (index 0) — the H3 JMA spec has no separate per-frame movement section. Verified against `General-101/Halo-Asset-Blender-Development-Toolset` reader/writer and TagTool's `Animation.Process()` / `Export()`.

Inheriting jmads (zero local animations, parent reference set) are a normal success: `Animation::new` returns with `len() == 0` and `parent()` non-null. Walk the parent and merge if you need the inherited animations.

Codec coverage (validated against 36,270 / 36,270 H3 + Reach MCC animations):

| Slot | Codec | Status |
|---|---|---|
| 1 | UncompressedStatic | ✓ |
| 2 | UncompressedAnimated | ✓ (= slot 8 decoder) |
| 3 | EightByteQuantizedRotationOnly | ✓ |
| 4 | ByteKeyframeLightlyQuantized | ✓ |
| 5 | WordKeyframeLightlyQuantized | ✓ |
| 6 | ReverseByteKeyframeLightlyQuantized | ✓ (= slot 4 decoder; reverse is a compressor-side variant) |
| 7 | ReverseWordKeyframeLightlyQuantized | ✓ (= slot 5) |
| 8 | BlendScreen | ✓ |
| 9 | Curve | ✓ |
| 10 | RevisedCurve (H4-era) | ✓ ("cache" rotation_layout) |
| 11 | SharedStatic (HO+) | not implemented (graph-level shared codec pool, 0 anims in MCC corpus) |

Engine-aware blob layout: H3 uses a hardcoded section ordering; Reach uses cumulative-sum from positional indices in a renamed-but-misleading `data sizes` struct (the Reach schema kept H3 field names but several slots were repurposed — position 0 is the static codec stream regardless of what the field is called). The engine-aware path is internal to `decode()`; the public API doesn't expose the distinction.

JMA-family export applies the per-format conventions at write time only — translation `× 100` (cm convention), quaternion **conjugate** serialization, and Foundry-style local→world `dx/dy` rotation by accumulated yaw (per Foundry commit `850d680d`, which fixes TagTool's actor-slides-backwards bug on yawed-during-walk animations). The codec-decoded values stay in raw engine units so callers can render or re-encode without unwinding.

Two corpus-wide validators live in `examples/`:

```sh
# Decode every animation, tally per-codec status:
cargo run --release -p blam-tags --example jmad_decode_sweep -- \
    out_dir /path/to/halo3_mcc/tags /path/to/haloreach_mcc/tags

# Run the JMA writer end-to-end against a sink:
cargo run --release -p blam-tags --example jmad_export_sweep -- \
    /path/to/halo3_mcc/tags /path/to/haloreach_mcc/tags
```

### JMS / ASS (render / collision / physics → JMS, with ASS for instance-bearing render_models)

`JmsFile` reconstructs a Bungie Joint Model Skeleton (`.JMS`, version 8213) from a parsed `render_model`, `collision_model`, or `physics_model` tag. The three constructors emit per-purpose JMS files — render geometry only, collision BSP only, or physics primitives + constraints — so callers can split them across the H3EK source-tree layout (`render/`, `collision/`, `physics/`).

For render_models with **instance geometry** (`instance mesh index >= 0` + `instance placements[]` populated — the brute, decorators, level objects), [`AssFile::from_render_model`](src/ass.rs) is the structurally-faithful path: it emits an ASS file whose `INSTANCES` section carries one entry per placement, which Tool re-extracts back into `instance placements[]` on recompile. The shell's `extract-geometry` command auto-dispatches between the two based on tag content; library callers pick directly.

```rust
use blam_tags::{JmsFile, TagFile};
use std::fs::File;
use std::io::BufWriter;

let render_tag = TagFile::read("masterchief.render_model")?;
let render_jms = JmsFile::from_render_model(&render_tag)?;

// Collision and physics need the render_model's skeleton for
// world-space placement (their own nodes carry only names + tree links).
let coll_tag = TagFile::read("masterchief.collision_model")?;
let coll_jms = JmsFile::from_collision_model_with_skeleton(&coll_tag, &render_jms.nodes)?;

let phmo_tag = TagFile::read("masterchief.physics_model")?;
let phmo_jms = JmsFile::from_physics_model_with_skeleton(&phmo_tag, &render_jms.nodes)?;

for (kind, jms) in [("render", &render_jms), ("collision", &coll_jms), ("physics", &phmo_jms)] {
    let mut out = BufWriter::new(File::create(format!("masterchief.{kind}.jms"))?);
    jms.write(&mut out)?;
}
```

Render path walks `regions × permutations × meshes × parts`, decompresses bounds-quantized positions/UVs against `render geometry/compression info[0]`, and converts triangle strips to lists with restart-aware parity + degenerate-triangle filtering. After that pass, **`from_render_model` also walks `instance placements[]`** — modular pieces (gauntlets, helmet variants, knee guards, jumppack braces, etc.) whose geometry sits in `meshes[instance mesh index]` and is reused at multiple bones with per-placement transforms. Each placement is paired with `meshes[N].subparts[i]` (1:1 by index), transformed by its column-major `(forward, left, up, position) × scale` matrix mirroring Foundry's `render_model.py` behaviour, and weighted to the placement's `node_index`. Without this, characters whose modular armor lives in the instance mesh — like the brute, with 38 named placements — extract with all attachments missing. TagTool's `ModelExtractor` only walks `InstancePlacements` for `VertexType.Decorator` (foliage); blam-tags is the only render_model→JMS pipeline that recovers these for skinned meshes. Collision path walks each BSP's `surfaces[]` via the edge-ring algorithm (each edge belongs to two surfaces; the matching side decides start-vs-end vertex emission), fan-triangulates each ring, and emits world-space vertices through the supplied skeleton. Physics path emits Havok shape primitives (sphere, box, pill, polyhedron) plus ragdoll/hinge constraints, using the skeleton for world-space anchor placement.

Validated across the H3 MCC corpus: 4,354 / 4,354 reconstructions clean across `render_model`, `collision_model`, and `physics_model`. 89.7% of render-model JMSes have ≥99% bounding-box match against the embedded source JMS; 86.8% have ≥99% position coverage at 10 cm precision.

```sh
cargo run --release -p blam-tags --example jms_corpus_sweep -- \
    /path/to/halo3_mcc/tags
```

### ASS (scenario_structure_bsp → ASS)

`AssFile` reconstructs a Bungie Amalgam scene (`.ASS`, version 7) from a parsed `scenario_structure_bsp` tag. ASS is the level-geometry counterpart to JMS — same family but for static scene structure rather than rigged objects. The reconstruction walks every category needed for re-import as artist source:

```rust
use blam_tags::{AssFile, TagFile};
use std::fs::File;
use std::io::BufWriter;

let bsp_tag = TagFile::read("construct.scenario_structure_bsp")?;
let mut ass = AssFile::from_scenario_structure_bsp(&bsp_tag)?;

// Layer in lighting from the paired scenario_structure_lighting_info tag
// (real BM_LIGHTING_BASIC/ATTEN/FRUS metadata + GENERIC_LIGHT objects).
let stli_tag = TagFile::read("construct.scenario_structure_lighting_info")?;
ass.add_lights_from_stli(&stli_tag)?;

let mut out = BufWriter::new(File::create("construct.ASS")?);
ass.write(&mut out)?;
```

Categories emitted: cluster MESHes (one per cluster), per-IGD-def MESHes (one per `instanced geometries definitions[]` entry, content-deduped) plus per-placement INSTANCEs, cluster portals (each as `+portal_N` MESH), weather polyhedra (convex hull from plane set, as `+weather_N`), structure collision BSP (one merged `@CollideOnly` MESH using the same edge-ring walker the JMS path uses), sbsp markers (SPHERE primitives), `environment_objects[]` xref-only OBJECTs, and SPOT/DIRECT/OMNI/AMBIENT generic lights from the `.stli`. Special-marker materials (`+portal`, `+weather`, `@collision_only`) are auto-appended so Tool.exe re-extracts each category back into its proper tag block on recompile.

Validated across the H3 MCC corpus: 147 / 147 BSPs across 49 scenarios clean — 20,747 MESH + 6,605 GENERIC_LIGHT + ~150 SPHERE markers, 82k INSTANCEs, 19.9M verts, 14.9M tris. Source ASS files have a different mesh granularity (artist-named meshes vs our cluster aggregates) — that's compile-time information the tag doesn't carry.

```sh
cargo run --release -p blam-tags --example ass_corpus_sweep -- \
    /path/to/halo3_mcc/tags
```

### Optional streams (want / info / assd)

Three optional streams can hang off the tag file — `want` (dependency list), `info` (import info), `assd` (asset-depot icon storage). They're off by default on freshly created tags; attach as needed:

```rust
tag.add_dependency_list("definitions/halo3_mcc/tag_dependency_list.json")?;
tag.add_import_info("definitions/halo3_mcc/tag_import_information.json")?;
tag.add_asset_depot_storage("definitions/halo3_mcc/asset_depot_storage.json")?;

// Populate the dependency list from the tag's tag_reference fields
// (walks the tag tree, collects every non-null non-`impo` reference,
// matches the authoring toolset 98.8% exact on real tags):
tag.rebuild_dependency_list("definitions/halo3_mcc/tag_dependency_list.json")?;

// Drop a stream:
tag.remove_import_info();
tag.remove_asset_depot_storage();

// Read the root element of each stream via the facade:
if let Some(info) = tag.import_info() {
    let build = info.field_path("build").unwrap().value().unwrap();
    println!("build: {build:?}");
}
```

**Header checksum** is left at `0` on new tags, matching BCS's behaviour. The algorithm is a reflected CRC32 (poly `0xEDB88320`) but the exact byte-span fed through it isn't pinned down — see the `TagFile::recompute_checksum` doc comment in [`src/file.rs`](src/file.rs) for the research trail. The method exists as a no-op for when we come back to it; callers that need a real checksum will get `0` for now.

### Roundtrip (read → write → compare)

```rust
use blam_tags::TagFile;

let tag = TagFile::read("path/to/source.biped")?;
tag.write("path/to/temp.biped")?;

let source = std::fs::read("path/to/source.biped")?;
let round  = std::fs::read("path/to/temp.biped")?;
assert_eq!(md5::compute(&source), md5::compute(&round));
```

For in-memory pipelines (fuzzing, archive embedding, tests), use the
byte-buffer entry points instead of touching the filesystem:

```rust
let bytes = std::fs::read("path/to/source.biped")?;
let tag = TagFile::read_from_bytes(&bytes)?;
let round_bytes = tag.write_to_bytes()?;
assert_eq!(bytes, round_bytes);
```

The corpus-wide sweep lives in [`examples/roundtrip.rs`](examples/roundtrip.rs).
Run against one or more tag roots:

```sh
cargo run --release -p blam-tags --example roundtrip -- \
    /path/to/halo3_mcc/tags /path/to/haloreach_mcc/tags
```

### Error handling on the read path

Every wire-format failure surfaces as a typed [`TagReadError`](src/error.rs) — never a panic. The variants carry enough context to diagnose a malformed tag without re-running with prints:

```rust
use blam_tags::{TagFile, TagReadError};

match TagFile::read("path/to/tag.biped") {
    Ok(tag) => { /* … */ }
    Err(TagReadError::BadChunkSignature { offset, expected, got }) => {
        eprintln!("bad signature at 0x{offset:X}: expected {expected:?}, got {got:?}");
    }
    Err(TagReadError::ChunkSizeMismatch { chunk, started_at, ended_at, expected_end }) => {
        eprintln!("{chunk} ran from 0x{started_at:X} to 0x{ended_at:X}, expected 0x{expected_end:X}");
    }
    Err(TagReadError::Io(e)) => eprintln!("I/O error: {e}"),
    Err(other) => eprintln!("read failed: {other}"),
}
```

`TagReadError` is `#[non_exhaustive]` — match with a catch-all arm so adding new variants in future versions doesn't break callers. Schema-import errors (JSON shape, parent-chain resolution) live separately in [`TagSchemaError`](src/schema.rs).

The corruption test suite at [`tests/corruption.rs`](tests/corruption.rs) covers the major variants by feeding deliberately malformed bytes through `TagFile::read_from_bytes`.

## Architecture

Tag files are schema-driven — every tag carries its own layout description (`blay` chunk), so the parser is **generic**: nothing is hard-coded per tag type.
The library's job is to (a) read the embedded schema, (b) read the payload bytes into a tree that mirrors the schema, and (c) write that tree back byte-exact.

Two facades sit on top of the raw storage types:

- **[`api`]** — data-side facade. `TagStruct`, `TagField`, `TagBlock`,
  `TagArray`, `TagFlag`, `TagResource` and their mutable counterparts.
  Reachable from `TagFile`.
- **[`definition`]** — schema-side facade. `TagStructDefinition`,
  `TagFieldDefinition`, `TagBlockDefinition`, `TagArrayDefinition`,
  `TagResourceDefinition`. Reachable from `TagFile::definitions()`.

Plus four group-specific helper layers (all built on the `api` facade):

- **[`bitmap`]** — `Bitmap`, `BitmapImage`, `BitmapFormat`,
  `BitmapCurve`, plus `write_tiff` (Tool-importable RGBA8, default)
  and `write_dds` (legacy debug). Submodules: `format` (enum +
  predicates + bpp), `decode` (per-format → RGBA8), `dds`, `tiff`,
  `layout` (cube cross + array strip). Covers all 20 formats
  observed in the halo3_mcc + haloreach_mcc bitmap corpora; both
  output paths validated at 25,908 / 25,908 images.
- **[`animation`]** — `Animation`, `AnimationGroup`, `AnimationClip`,
  `AnimationTracks`, `MovementData`, `NodeFlags`, `Skeleton`, `Pose`,
  `JmaKind`, `Codec`, `BitArray`. Decodes `model_animation_graph`
  blobs across all 10 implemented codec slots and emits JMA-family
  text files via `Pose::write_jma`. Submodules: `codec` (Codec
  enum + headers + per-slot decoders), `pose` (Skeleton + Pose
  composition), `jma` (JMA-family text writer). Engine-aware
  (H3 hardcoded layout vs Reach cumulative-sum) under the hood;
  public API is uniform.
- **[`jms`]** — `JmsFile` plus per-section types (`JmsNode`,
  `JmsMaterial`, `JmsMarker`, `JmsVertex`, `JmsTriangle`, plus
  collision/physics primitives `JmsSphere`/`JmsBox`/`JmsCapsule`/
  `JmsConvex`/`JmsRagdoll`/`JmsHinge`). Reconstructs a JMS scene
  (version 8213) from `render_model` / `collision_model` /
  `physics_model` tags. Per-purpose constructors (render only,
  collision-with-skeleton, physics-with-skeleton) match what
  Tool.exe expects in each H3EK source-tree subdirectory.
- **[`ass`]** — `AssFile` plus per-section types (`AssMaterial`,
  `AssObject` / `AssObjectPayload`, `AssLight` / `AssLightKind`,
  `AssVertex`, `AssTriangle`, `AssInstance`). Reconstructs a
  Bungie Amalgam scene (version 7) from `scenario_structure_bsp` +
  `scenario_structure_lighting_info` pairs.

A small private [`geometry`] module carries the format-specific
helpers shared between `jms` and `ass`: `CompressionBounds`
(dequantize bounds-compressed positions / texcoords), restart-aware
triangle-strip → list conversion, the Halo BSP edge-ring walker,
and the world-units → centimeter `SCALE` constant. Not part of the
public API. Generic vector / quaternion / point math lives on the
[`math`] types directly (see below); typed field readers live as
inherent methods on [`api::TagStruct`].

The [`math`] module carries the canonical Halo `real_*` types
(`RealPoint3d`, `RealVector3d`, `RealQuaternion`, `RealPlane3d`,
`Bounds<T>`, color types, …) plus their inherent methods + standard
`Ops` impls. Point-vs-vector semantics are enforced at the type
level: `RealPoint3d + RealVector3d → RealPoint3d` (translate),
`RealPoint3d - RealPoint3d → RealVector3d` (displacement),
`RealPoint3d + RealPoint3d` is intentionally **not** implemented
(use `as_vector()` to opt in explicitly). `RealQuaternion::IDENTITY`
is the explicit identity rotation — the derived `Default` is the
zero quat. See `src/math.rs` for the full surface.

Field readers (`TagStruct::read_quat` / `read_point3d` / `read_vec3`
/ `read_point2d` / `read_plane3d` / `read_rgb` / `read_real_bounds`)
return the typed math values directly. They default gracefully when
a field is genuinely missing from the struct (`IDENTITY` for quats,
`ZERO` for points/vectors, `Default` for the rest) but **panic** on
type mismatch — calling `read_point3d` on a `real_vector_3d` schema
field is a code-vs-schema bug, and silent zero-defaults would let
that hide.

Everything the user-facing code should need is one of these.
Lower-level modules (`data`, `path`, `stream`, `io`, `layout::TagLayout`) are available but no user code in this workspace (CLI, examples) reaches into them.

Error types:

- **[`error::TagReadError`]** — every failure on the binary read path. Carries chunk names, byte offsets, expected vs. actual values.
- **[`schema::TagSchemaError`]** — JSON-schema import failures (parse errors, missing parent chain, struct-size mismatches).

## Field paths

Paths match the shape the CLI uses:

```
"jump velocity"                      — root-level field
"unit/flags"                         — inline struct → field
"unit/seats[0]/flags"                — struct → block element → field
"regions[2]/permutations[0]/name"    — nested block elements
"Block:regions[0]/name"              — with optional Type: filter
```

Block and array element indices default to `0` on descent if omitted.
Field names are case-sensitive; `Type:` filters are case-insensitive.

## Version coverage

| Format                                         | Read | Write | Notes |
|------------------------------------------------|------|-------|-------|
| V1 layouts (flat `agro` records)               | ✓    | ✓     | Reconstructs `stv2` + `blv2` from paired aggregate records on write. |
| V2 layouts (`tgly` with `stv2`)                | ✓    | ✓     | Main Halo 3 / Reach format. |
| V3 layouts (adds `]==[` interop)               | ✓    | ✓     | Main Halo 3 / Reach format. |
| V4 layouts (`stv4` with per-struct version)    | ✓    | ✓     | Exercised on H4 / H2A MP tags in the community corpus sweep. |

Pageable-resource shapes handled: `tg\0c` (null), `tgrc` (exploded with inner `tgdt` + nested struct), `tgxc` (xsync, opaque payload). Exploded resources are *navigable*: `TagResource::as_struct()` returns a `TagStruct` view onto the header struct (raw bytes from the leading `struct_size` bytes of the `tgdt` payload, sub-chunks parsed from the trailing `tgst`). The path resolver and REPL `cd` step through `pageable_resource` segments transparently. The 8 inline handle bytes (engine memory state, runtime-junk in MCC tags) are exposed via `TagResource::inline_bytes()` for diagnostic tooling. Bitmaps in halo3_mcc / haloreach_mcc keep their pixel data in the top-level `processed pixel data` field rather than a pageable resource — see [`bitmap`] for the extraction path.
ApiInterop (`ti][`) fields are parsed into `TagFieldData::ApiInterop` with `descriptor` / `address` / `definition_address` accessors and a `reset()` builder for BCS's canonical `{0, UINT_MAX, 0}` pattern.
VertexBuffer fields are preserved as raw bytes through the roundtrip but not yet parsed into typed values.
