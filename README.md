# blam-tags workspace

A Rust implementation of the Halo tag file format: a byte-exact roundtrip-capable library, a CLI for inspecting and editing tags, and Python bindings.

No ManagedBlam, no .NET, no engine required. For MCC tags the parser reads each tag's embedded layout chunk and interprets the bytes directly; classic Halo CE / Halo 2 loose tags — which carry no embedded layout — are decoded against a field definition synthesized from the same JSON schemas.

## Crates

| Crate | Role |
|---|---|
| [<code>blam&#8209;tags</code>](./blam-tags/) | The library. Reads, writes, navigates, and edits tag files — MCC self-describing tags and classic CE / H2 loose tags through one API. Plus group-specific extractors (`bitmap` → TIFF / DDS, `model_animation_graph` → JMA-family, `render_model`/`collision_model`/`physics_model` → JMS, `scenario_structure_bsp` → ASS, `particle_model` → JMI) and a cross-game tag-conversion subsystem. |
| [<code>blam&#8209;tag&#8209;shell</code>](./blam-tag-shell/) | Command-line front-end + interactive REPL. Subcommands for header metadata, directory listing / search / dependency walking, field tree inspection, get / set / flag / block edits, options enumeration, schema and value diffing, integrity checks, replay-script export, raw `tag_data` field dump, bare-tag creation (`new`), stream management (`want` / `info` / `assd`), the group-specific extractors (bitmap → TIFF/DDS, `.model` → JMS or ASS via `extract-geometry`'s content-based dispatch, `.scenario` → ASS per BSP, `particle_model` → JMI, animation → JMA-family), `extract-import-info` for unpacking the zlib-compressed source files Bungie's importer baked into the `info` stream, and `list-cache` plus the global `--cache <DIR>` flag for browsing / reading Halo 4 monolithic tag-cache builds. |
| [<code>blam&#8209;tags&#8209;py</code>](./blam-tags-py/) | Python bindings (PyO3). The math / game / file / fields types and the borrowing `api` facade, exposed as an `import blam_tags` extension module. Mostly machine-generated; see the crate's [README](./blam-tags-py/README.md) and usage [GUIDE](./blam-tags-py/GUIDE.md). |
| [<code>blam&#8209;tags&#8209;bindgen</code>](./blam-tags-bindgen/) | The generator behind `blam-tags-py`: reads the rustdoc JSON of `blam-tags`, applies the policy in `blam-tags-py/bindings.toml`, and emits the PyO3 wrappers, the `.pyi` stubs, and a coverage report. Deterministic, so "regenerate and diff" works as a CI check. |

Each crate has its own README with API shape / command reference.

## Status

- **Byte-exact roundtrip validated across every tag in the Halo 3, Halo 3: ODST, Halo Reach, Halo 4, and Halo 2: Anniversary MP MCC corpora.** Read → write → md5 compare yields zero differences. Locally verified on the 119,432-tag H3 + Reach subset; full-corpus, validation (including H4 and H2A MP) contributed by the community.

- **Layout versions 1 – 4** all read/write and exercised in the above sweep.

- **Classic Halo CE and Halo 2 loose tags read, edit, and write byte-exactly through the same API.** Gen-1/2 tags are not self-describing — no embedded `blay`/`tgly` layout, no `tgbl`/`tgst` chunking, just flat depth-first data — so [`blam_tags::classic`](./blam-tags/src/classic.rs) decodes and re-encodes them against a field definition synthesized from the JSON schemas (ported from HABT's XML layouts). CE reads big-endian, H2 little-endian with per-`FieldSet` versioned struct layouts resolved from each block header's version word. The decoded form is the same `TagBlockData` / `TagStructData` model the MCC reader produces, so `TagFile::read` auto-detects the format and every downstream navigator, editor, and extractor works unchanged. Kits: `haloce_mcc`, `halo2_mcc`.

- **Cross-game tag conversion** (library, [`blam_tags::convert`](./blam-tags/src/convert/)). Analyzes and converts a tag from one engine to another — group routing, struct pairing, field matching, value translation, companion-tag synthesis, editing-kit MCC-header application — and reports what a move would lose before committing to it (`LossPolicy`, `TagConversionReport`). The subsystem is engine-agnostic core; presentation, kit mounting, and the bulk worker are the caller's (e.g. an editing-kit front-end), not the shell's.

- **Read path is panic-free on malformed input.** Every wire-format failure surfaces as a typed [`TagReadError`](blam-tags/src/error.rs) — `BadChunkSignature`, `BadChunkVersion`, `ChunkSizeMismatch`, `CountMismatch`, `InvalidUtf8`, etc. Corruption-suite tests live at [`blam-tags/tests/corruption.rs`](blam-tags/tests/corruption.rs).

- **Pageable resources walk like any other container.** Exploded resources expose a `TagResource::as_struct()` view onto the header struct (raw bytes pulled from the `tgdt` payload, sub-chunks parsed from `tgst`); the path resolver, REPL `cd`, and `inspect` all step through them transparently.

- **Bitmap → TIFF / DDS extraction with 100% format coverage** across the halo3_mcc + haloreach_mcc bitmap corpora (25,908 / 25,908 images). Pure-tag-file path: pixels come from `processed pixel data`. **TIFF is the default** — Tool.exe-importable RGBA8 with the SnowyMouse libtiff field profile, full per-format decode (uncompressed + BC1/2/3/4/5 + Halo's `dxn_mono_alpha`), DX cube cross + vertical-strip array layouts. **DDS** (`--format dds`) preserves original bytes for inspection: legacy fourcc/pixelformat for the common formats, DXT10 for arrays and `signedr16g16b16a16`, CPU decode to A8R8G8B8 for `dxn_mono_alpha`. See [`blam-tag-shell extract-bitmap`](./blam-tag-shell/README.md#extract-bitmap--bitmap-to-tiff--dds) and the [`blam_tags::bitmap`](./blam-tags/src/bitmap/) module.

- **`model_animation_graph` → JMA-family text export with 100% codec coverage** across the H3 + Reach MCC corpora (36,270 / 36,270 animations). Decodes all 10 implemented codec slots — UncompressedStatic, UncompressedAnimated, EightByteQuantizedRotationOnly, ByteKeyframe, WordKeyframe, ReverseByte/WordKeyframe, BlendScreen, Curve, RevisedCurve — composes static + animated tracks against the skeleton (rest pose from render_model nodes + jmad's `additional node data` cache), and emits `.JMM/.JMA/.JMT/.JMZ/.JMO/.JMR/.JMW` text files re-importable by Halo content tooling. Movement deltas are folded into the root bone — H3 JMA has no separate movement section — with Foundry-style local→world `dx/dy` rotation by accumulated yaw. Per-type frame layout matches Tool's importer convention: base/JMW append a trailing held frame, JMR prepends rest pose, JMO prepends + composes `(rest × codec_delta)` per frame. JMW is selected from `internal_flags / world relative` (not the `animation_type` enum). See [`blam-tag-shell extract-animation`](./blam-tag-shell/README.md#extract-animation--decode-and-export-an-animation) / [`list-animations`](./blam-tag-shell/README.md#list-animations--enumerate-jmad-animations) and the [`blam_tags::animation`](./blam-tags/src/animation/) module.

- **`.model` → JMS / ASS export with full coverage** of the H3 MCC corpus. Polymorphic over `render_model`/`collision_model`/`physics_model` — emits per-purpose source files in the H3EK source-tree layout (`render/`, `collision/`, `physics/`). **Render-side auto-dispatch**: ASS when the render_model carries `instance mesh index >= 0` + populated `instance placements[]` (the brute, decorators, level objects — 141/1869 H3 render_models); JMS otherwise. ASS round-trips back through Tool to recreate the per-placement structure on recompile; JMS bakes placements inline as triangles (lossy but universally supported). `--force jms`/`ass` overrides the dispatch. Collision path walks BSP edge rings, physics path emits Havok primitives + ragdoll/hinge constraints — both always JMS (ASS doesn't represent them). The skeleton from `render_model` provides world-space placement for `coll`/`phmo`. See [`blam-tag-shell extract-geometry`](./blam-tag-shell/README.md#extract-geometry--model-to-source-tree-jms--ass-files), the [`blam_tags::jms`](./blam-tags/src/jms.rs) module, and [`AssFile::from_render_model`](./blam-tags/src/ass.rs).

- **`.scenario` → ASS export with full corpus coverage** (147 / 147 BSPs across 49 H3 scenarios). Emits one ASS file per `scenario.structure_bsps[]` entry, pairing each `scenario_structure_bsp` with its `scenario_structure_lighting_info`. Categories emitted: cluster MESHes, per-IGD-def MESHes + per-placement INSTANCEs, real `BM_LIGHTING_*` material metadata, cluster portals, weather polyhedra, structure collision BSP, sbsp markers (as SPHERE primitives), `environment_objects[]` xref placements, and SPOT/DIRECT/OMNI/AMBIENT generic lights. Output mirrors H3EK's `data/levels/<map>/structure/<bsp>.ASS` layout for re-import as artist source. See [`blam-tag-shell extract-geometry`](./blam-tag-shell/README.md#extract-geometry--tag-to-source-tree-jms--ass-files) (scnr/sbsp inputs) and the [`blam_tags::ass`](./blam-tags/src/ass.rs) module.

- **`particle_model` → JMI export.** A `particle_model` (H3/Reach/H4 `pmdf`, H2 `PRTM`) is the merged mesh Tool builds from a set of per-object JMS files indexed by a `.jmi` manifest. `extract-geometry` reverses that: it emits the `.jmi` plus one JMS per object in the layout Tool's `import particle model` reads back. See [`blam-tag-shell extract-geometry`](./blam-tag-shell/README.md#extract-geometry--tag-to-source-tree-jms--ass-files), [`blam_tags::jmi`](./blam-tags/src/jmi.rs), and [`blam_tags::particle_model`](./blam-tags/src/particle_model.rs).

- **Halo 4 monolithic tag-cache reads.** The H4 dev-build `tag_cache/` directory (one `blob_index.dat` + a handful of `tags_N` / `cache_N` partition blobs) opens as a read-only filesystem of tags addressable by cache-relative `group:name` paths. Every read-side verb (`inspect`, `get`, `extract-bitmap`, `extract-geometry`, `extract-animation`, `list-cache`, …) works against cache entries identically to filesystem tags. See [`blam-tag-shell --cache`](./blam-tag-shell/README.md#--cache---c-optional--for-halo-4-monolithic-builds) and the [`blam_tags::monolithic`](./blam-tags/src/monolithic/) module.

- **UE5 IoStore virtual filesystem + Halo: Campaign Evolved mod-writing** (behind the `iostore` feature). Campaign Evolved — the UE5 remake of Halo 1 on a modified Reach engine — cooks its Reach-format tags into UE5 IoStore paks (`.utoc`/`.ucas`, TOC version 8, Oodle-compressed) as `.ubulk` payloads, each a byte-complete self-describing MCC tag. The [`blam_tags::iostore`](./blam-tags/src/iostore/) module mounts those containers as a read-only tag filesystem (pure-Rust Oodle decode via `oozextract`, mmap'd `.ucas`), and — for modding — either **overwrites a tag inside its own pak in place** (append the edited chunk to the `.ucas` + repoint the `.utoc`, preserving the perfect-hash seeds) or **writes higher-priority override containers** that the game loads on top of the base without touching it (each emitted as the `.utoc`/`.ucas`/`.pak` triplet UE's loader requires — it discovers containers by scanning `Paks/` for `.pak` stubs and derives the `.utoc`/`.ucas` from them): same-name overrides that reuse the original chunk id, and new/renamed tags built with a byte-exact port of UE5's **Zen `.uasset` (de)serializer** (deserialize→serialize round-trip validated on ~380 real packages) plus a container-header / package-store / redirect writer. Tag identity (`FPackageId`, `public_export_hash`) is derived with the game's own `CityHash64`, validated against real chunk ids. The Zen serializer was ported near-verbatim from [trumank/retoc](https://github.com/trumank/retoc) (MIT).

- **X360 render-geometry hydration.** Render_models in H4 X360 monolithic caches carry GPU-format vertex / index buffers as xsync-resident resources rather than the author-format `raw_vertex_block` / `raw indices` PC MCC tags ship with. The [`blam_tags::render_geometry`](./blam-tags/src/render_geometry/) module decodes those buffers (UDec4N / DHEN3N / DEC3N / UShort4N / UShort2N / UByte4 / UByte4N / half2 / Float3 / Float4 — all binary-verified against the rasterizer vertex declaration table at runtime) into the same fields, so `extract-geometry` produces JMS / ASS from X360 source identically to PC MCC source. Skinned meshes get a `per_mesh_node_map` local→global skeleton-index remap on the way through; storm_elite and bigmuthafucka both extract cleanly into Blender via the patched HABT importer.

- **Python bindings** ([`blam-tags-py`](./blam-tags-py/)). `import blam_tags` exposes the math / game / file / fields types and the `api` facade — open, navigate, read, and edit tags from Python with a byte-exact write path. The lifetime-bound facade (`TagStruct` / `TagField` / `TagBlock`) is re-expressed as path handles that re-resolve on every access and refuse to resolve after a structural edit moved their element out from under them. Built with maturin (`abi3-py38`, one wheel per platform for CPython 3.8+). Usage guide: [`blam-tags-py/GUIDE.md`](./blam-tags-py/GUIDE.md).

## Build

```sh
cargo build --release --workspace
```

Builds the library, the CLI binary (`blam-tag-shell`), and the Python extension (`blam-tags-py`). Two optional library features are off by default: `audio` (Vorbis/Opus decode) and `iostore` (the UE5 IoStore / Campaign Evolved support and its examples). On macOS the checked-in [`.cargo/config.toml`](./.cargo/config.toml) supplies the `-undefined dynamic_lookup` linker args PyO3's `extension-module` needs so the Python cdylib links under a plain `cargo build`; to build an installable wheel instead, use [maturin](https://www.maturin.rs/) (`maturin build --release -m blam-tags-py/Cargo.toml`).

## Use the CLI

The shell needs a `--game <GAME>` flag (alias `-g`) on every invocation — it scopes schema lookups and group-name resolution to `definitions/<GAME>/`. `<GAME>` is a directory name under `definitions/`: `haloce_mcc`, `halo2_mcc`, `halo2amp_mcc`, `halo3_mcc`, `halo3odst_mcc`, `haloreach_mcc`, or `halo4_mcc` (plus `haloce_evolved` for Campaign Evolved).

```sh
cargo run --release -p blam-tag-shell -- --game halo3_mcc header path/to/masterchief.biped
cargo run --release -p blam-tag-shell -- --game halo3_mcc get    path/to/masterchief.biped "jump velocity"
cargo run --release -p blam-tag-shell -- --game halo3_mcc set    path/to/masterchief.biped "jump velocity" 3.14
```

Full command reference in [`blam-tag-shell/README.md`](./blam-tag-shell/README.md).

## Use the library

```rust
use blam_tags::TagFile;

let mut tag = TagFile::read("path/to/masterchief.biped")?;

// Read a field by slash-separated path. `value()` returns the
// per-variant `TagFieldData` (or `None` for container/padding fields).
let jump = tag.root().field_path("jump velocity").unwrap();
println!("{} ({}): {:?}", jump.name(), jump.type_name(), jump.value().unwrap());

// Toggle a flag and write the edit back to a new file.
tag.root_mut()
    .field_path_mut("unit/flags").unwrap()
    .flag_mut("has_hull").unwrap()
    .toggle();

tag.write("path/to/edited.biped")?;
```

Full API tour with more examples in [`blam-tags/README.md`](./blam-tags/README.md).

## Use from Python

```python
import blam_tags as bt

tag  = bt.TagFile.read("path/to/masterchief.biped")
root = tag.root()

root.field("jump velocity").set(9.75)
root.field("flags").set_flag("turns without animating", True)

seats = root.field("unit").as_struct().field("seats").as_block()
seats.add()
seats[0].field("label").set("driver")

tag.write("edited.biped")
```

Build a wheel with maturin (`maturin build --release -m blam-tags-py/Cargo.toml`) and `pip install` it, or `maturin develop` into a virtualenv. Full walkthrough — the object model, the value type-mapping table, the staleness rule, and the exception hierarchy — in [`blam-tags-py/GUIDE.md`](./blam-tags-py/GUIDE.md).

## Layout

```
blam-tags/            — workspace root
├── Cargo.toml        — virtual manifest (4 members + excluded fuzz crate)
├── .cargo/config.toml — macOS linker args for the PyO3 cdylib
├── definitions/      — per-kit JSON schemas (submodule)
├── blam-tags/        — library crate
│   ├── src/            — generic tag tree: io, math, error, fields,
│   │                     layout, schema, data, path, field_path, stream,
│   │                     file, api, definition, game
│   │                   — classic CE/H2 loose-tag decode/encode: classic
│   │                   — cross-game conversion: convert
│   │                   — group-specific extractors: bitmap, animation,
│   │                     jms, jmi, ass, particle_model, extract
│   │                     (sharing the geometry helper module)
│   │                   — Halo 4 platform support: monolithic (cache
│   │                     reader + xsync), render_geometry (X360 vertex /
│   │                     index buffer hydration into author-format)
│   │                   — typed walkers for narrow groups: render_method,
│   │                     render_model, tag_function
│   └── tests/        — integration tests (corruption suite, etc)
├── blam-tag-shell/   — CLI crate
│   └── src/          — Clap entry point + per-command implementations
├── blam-tags-py/     — Python bindings (PyO3): generated + hand-written
│   │                   facade, .pyi stubs, GUIDE.md, pytest suites
│   └── src/manual/   — the hand-written borrowing facade
└── blam-tags-bindgen/ — generator that emits blam-tags-py from rustdoc JSON
```
