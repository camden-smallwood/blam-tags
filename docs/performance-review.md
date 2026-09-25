# blam-tags performance and duplication review

A tracking list for the 2026-09-25 review of the `blam-tags` library: every
performance problem and duplication family found, with where it is, what it
costs, and how to fix it. Tick an item when its fix lands, and record the
commit and the re-measured number next to it.

**Verified** means the claim was checked against the code, and the cost against
a benchmark or profile where one is given. **Reported** means it came from the
review pass and has not been independently confirmed yet — confirm before
acting on it.

Every fix must keep the byte-exact roundtrip gates green (MCC read → write,
classic CE / H2 decode → encode).

---

## Baseline

Measured with `scratchpad/vbench` (standalone crate, path dependency, release +
line tables), per file: `fs::read`, then parse, then `write_to_bytes`, then
drop. Warm page cache.

| Corpus | Tags | Read | Write | Drop |
|---|---|---|---|---|
| H3 MCC (`tags/objects/` from H3EK) | 15,554 / 4.2 GB | 0.78 s (`read_from_bytes`) / **4.24 s** (`TagFile::read(path)`) | 1.24 s | 0.29 s |
| H2 MCC classic | 28,177 / 12 GB | 11.7 s, of which **8.1 s** `TagLayout::from_json` | **26.6 s** | 0.88 s |
| CE MCC classic | 11,539 / 1.8 GB | 6.9 s, of which **6.3 s** `TagLayout::from_json` | 3.7 s | 0.05 s |

Top of the read profile (H3): `TagStructData::new_default`, then syscalls,
then malloc/free. Top of the write profile: `_platform_memmove`. Top of the H2
classic write profile: `write_classic_tag` self-time (the inlined CRC).

Re-measure after each fix and add a row to the log at the bottom.

---

## Tier 1 — small, local, measured

- [x] **P1. `TagFile::read(path)` is 5× slower than parsing the same bytes** — Verified
  - Where: `file.rs:261` — `BufReader::with_capacity(64 * 1024, File::open(path)?)`.
  - Why: on a file-backed `BufReader`, every `stream_position()` is an `lseek`
    syscall (~3 per `tgst`/`tgbl`), and the placeholder rewind at
    `data.rs:864` discards the buffer. `__lseek` is 88% of samples.
  - Fix: `Self::read_from_bytes(&std::fs::read(path)?)`. Also drop the
    `BufReader` around the `Cursor` in `read_from_bytes` (a redundant copy),
    and use `seek_relative` for the rewind.
  - Also speeds up: the MCC verify step in `write_atomic`, which goes through
    `Self::read(&temp_path)`.
  - Measured: 4.24 s → expected ≈ 1.3 s (0.78 s parse + 0.5 s `fs::read`).
  - **Done** (uncommitted): `read` is now `read_from_bytes(&fs::read(path)?)`.
    H3 `read(path)` 4.24 s → **1.05 s**, file read included. Still open: the
    parser signatures take `BufReader<R>`, so `read_from_bytes` still copies
    through a `BufReader<Cursor>`; dropping that is a wider signature change.
    `read_dependency_references` keeps its file reader on purpose — it seeks
    past the tag stream without reading it.

- [x] **P2. `EngineOrderBuilder::add_string` is O(n²)** — Verified
  - Where: `schema.rs:1717` — `self.strings.windows(needle.len()).position(..)`.
  - Cost: ≈93% of `TagLayout::from_json` (CE `scenario.json`: 7.3 ms per call).
  - Fix: keep a `HashMap<Box<[u8]>, u32>` of every suffix of every stored
    string (a match must end at a terminator and can't span one, so matches
    are exactly suffixes of stored strings). Insert each string's suffixes in
    blob order with `entry().or_insert` so the lowest offset wins — identical
    to today's first-match scan, so offsets stay byte-exact.
  - **Done** (uncommitted): suffix index, built per terminated piece; a string
    with an embedded NUL falls back to the scan. Every one of the 1,492
    definitions across all 8 games builds identical layout tables before and
    after (hashed Debug dump of every table); building them all went
    2.27 s → **0.66 s**. H2 layout time fell only 8.1 → 6.5 s — the rest of
    H2's JSON cost is P4's re-parsing.

- [x] **P3. Byte- and bit-at-a-time CRC32** — Verified
  - Where: `classic.rs:590` `classic_checksum` (table, byte at a time — the
    dominant self-time of classic writes); `file.rs:~837`
    `reflected_crc32_no_final_xor` (bit at a time — 72 ms on 12.7 MB).
  - Fix: both are `!crc32fast::hash(body)`. `crc32fast` 1.5.0 is already in
    `Cargo.lock` through flate2; make it a direct dependency. Checked
    `zlib.crc32(body) ^ 0xFFFFFFFF == header@40` on 300 real H2 tags: 300/300.
  - Duplication: this also collapses the two CRC implementations into one.
  - **Done** (uncommitted): `classic_checksum` is `!crc32fast::hash(body)`;
    the MCC live-reload checksum calls it, and the bitwise copy and the table
    are gone. `crc32fast` is a direct dependency. Unit tests (`123456789`,
    empty) pass. H2 write 26.6 s → **4.1 s**, CE write 3.7 s → **0.59 s**,
    mismatch counts unchanged.

- [x] **P4. Classic reads rebuild the layout from JSON for every tag** — Verified
  - Where: `blam-tag-shell/src/context.rs:146`, `convert/mod.rs:103`,
    `:1783`, the classic template scan (`convert/mod.rs:3786`), every test that
    reads a classic tag.
  - Cost: 69% of H2 read time, 92% of CE read time.
  - Fix: `#[derive(Clone)]` on `TagLayout`; a layout cache keyed by
    (definition path, engine), since `read_classic_tag_file` mutates the
    layout through `adjust_layout_for_engine`. Inside `from_json`, parse
    `_meta.json` once instead of in `merge_parent_schemas` (444),
    `group_root_struct_size` (636), `template_ancestor_schemas` (651),
    `tmpl_expansion_size` (896, per `tmpl` field) and
    `persist_layout_version` (1328), plus `file.rs:822`.
  - Also: `schema.rs:406` uses `serde_json::from_reader(BufReader)`; use
    `fs::read` + `from_slice` (serde_json documents `from_reader` as slower).
  - **Done** (uncommitted): `from_json_with_meta` keeps a process-wide cache
    of built layouts keyed by canonical path, validated against the size and
    mtime of the definition and its `_meta.json`; a hit is a clone re-stamped
    with a fresh layout guid, since every build gets its own. `TagLayout` and
    `TagLayoutHeader` derive `Clone`. JSON is read with `fs::read` +
    `from_slice`. Every caller benefits unchanged (shell, converter, Python,
    tests). Classic read: H2 9.9 s → **3.8 s** (layouts 6.5 → 0.43 s),
    CE 3.4 s → **0.77 s**. Test: `an_edited_definition_is_not_served_from_the_layout_cache`.
    Not done: an edit to an *ancestor* or template sibling JSON isn't seen by
    a warm cache — only the definition itself and `_meta.json` are checked.

- [x] **P5. Simple-block elements rebuild their default scaffolding one at a time** — Verified
  - Where: `data.rs:1255-1261` calls `TagStructData::new_default` for every
    element of a simple block, walking every field each time.
  - Cost: top CPU symbol of the H3 read profile (~35% of in-memory read time
    on a large BSP, per the review's measurement).
  - Fix: build the default once per block and clone it; precompute a
    per-struct "has container fields" flag and push an empty element directly
    when it is false. Related: `get_struct_expected_children`
    (`layout.rs:146`) re-walks the child struct for every nested struct read —
    cache it per struct on the layout.
  - **Done, partly** (uncommitted): simple blocks build the scaffold once and
    `resize` with clones. A/B, 3 reps of H3 read-only: 2.27 s → **2.21 s**
    (~3%) — the review's "35%" was one BSP, not the corpus. Not done: complex
    elements with an empty `tgst` (`data.rs:~499`) still walk per element
    (~4% of read samples); a per-struct flag on `TagLayout` would fix it, but
    the layout is mutable and the flag could go stale — not worth 3%.
    `get_struct_expected_children` didn't show in the profile; left alone.

- [x] **P6. `write_atomic` serializes the tag stream twice** — Verified
  - Where: `file.rs:494` `write_atomic_bytes` — `write_to_bytes()` and then
    `tag_stream.write(..)` into `main_stream` just for the checksum.
  - Fix: compute the CRC over `bytes[64..64 + 12 + stream_size]` of the
    already-serialized output (after P3); verify with `read_from_bytes(&bytes)`
    instead of re-reading the temp file from disk.
  - **Done** (uncommitted): `write_atomic_bytes` writes header, `tag!` and
    the optional streams once (shared `write_optional_streams` with
    `write_to`) and checksums the `tag!` range in place. Output byte-identical
    to before on 2,223 H3 tags. Wall-clock gain is small — `write_atomic` is
    dominated by `sync_all` and the file I/O. The verify step still re-reads
    the temp file from disk on purpose (it checks what landed), and is fast now
    after P1.

---

## Tier 2 — structural

- [x] **P7. Each chunk level is written to its own buffer, then copied into its parent** — Verified
  - Where: `stream.rs:134-144`, `data.rs:517-521` (tgst), `1284-1297` (tgbl),
    `1148-1151` (tgrc), `layout.rs:584-591`.
  - Cost: `_platform_memmove` tops the write profile; write is 1.6× read.
    Copying grows with nesting depth × tag size.
  - Fix: one output `Vec<u8>`; push a 12-byte placeholder header, write the
    body, patch the size in place. Or a size pre-pass.
  - **Done** (uncommitted): `io::begin_tag_chunk` / `end_tag_chunk` open a
    chunk with a zero size and patch it once its payload is written; tgst
    (size also in the version slot), tgbl, tgrc, bdat and the stream chunk all
    write into the one buffer. `TagStream::write` takes `&mut Vec<u8>`, and
    `TagFile::write_mcc_into` is the single MCC write path behind
    `write_to_bytes`, `write` and `write_atomic`. H3 write, A/B over 3 runs:
    1.31 s → **0.49–0.73 s**, 0 mismatches on 15,554 tags; `write_atomic`
    output byte-identical on 2,223. What `memmove` remains is the one
    unavoidable copy of leaf data blobs (578 samples) — regrowth is 94.
    Not done: `TagLayout::write` still buffers its body (small).

- [ ] **P8. Field lookup by name re-cleans both names on every comparison** — Verified
  - Where: `data.rs:700` `find_field_by_name` → `field_name_matches`
    (`data.rs:285`) → `clean_field_name` on the stored name *and* the query,
    per candidate; `layout.rs:291` `get_string` byte-loops to the terminator
    and re-validates UTF-8; `field_name.rs:178` searches markers with a
    `&[char]` pattern.
  - Cost: a `field(name)` walk is 5.5× a `value()` walk (review's
    measurement); ~1,387 name-lookup call sites, many inside per-element and
    per-vertex loops.
  - Fix, in increasing effort: (a) clean the query once before the loop;
    (b) ASCII byte scan for markers; (c) precompute each field's clean-name
    range when the layout is built, making the compare a slice `==`;
    (d) a resolved-key API (`TagBlock::resolve(name) -> FieldKey`,
    `TagStruct::field_at(key)`) for hot loops.

- [ ] **P9. Sub-chunk lookups are linear scans** — Reported
  - Where: `api.rs:979` `sub_chunk`, `data.rs:540`, `564`, `756`, `793`, the
    10 `descend_*` helpers in `path.rs:459-560` — each
    `sub_chunks.iter().find(|e| e.field_index == Some(i))`. Walking a struct
    is O(k²).
  - Fix: route all through one `TagStructData::sub_chunk(i)`/`_mut`, then
    precompute each field's position among its struct's sub-chunk fields and
    search forward from there.

- [ ] **P10. `deserialize_field` allocates even when the caller only needs a type check** — Reported
  - Where: `fields.rs:628-636` (enum name `String`), `724-747` (flag names
    `Vec<(u32, String)>`), `1025` (`Data` clones the whole payload), `981`.
  - Worst case: `collect_from_field` (`file.rs:~733`, used by
    `rebuild_dependency_list` / `read_dependency_references`) calls `value()`
    on every leaf, cloning every data blob to check for a tag reference.
  - Fix: check `field_type()` before `value()`; add a raw-bits accessor for
    flag tests; resolve names lazily.

- [ ] **P11. `jms.rs` decodes the same raw vertex once per triangle corner** — Verified
  - Where: `jms.rs:3139-3150` (`build_geometry`), `jms.rs:~3298`
    (`append_instance_geometry`).
  - Fix: decode `raw_v` into a `Vec<JmsVertex>` once per mesh, clone per corner.
  - See also **B1** — the `continue` on this path is an index-corruption bug.

- [ ] **P12. Converter hot spots** — Reported (measured by the review: 11 ms/effect … 278 ms/scenario)
  - `build_target_from_definitions` (`convert/mod.rs:2733`) runs
    `TagLayout::from_json` and then `TagFile::new` (a second full build), and
    the result is discarded when a kit template exists (`2030`). Build once,
    lazily.
  - `ConversionMappingCatalog::load()` parses and validates the embedded JSON
    every call → `static LazyLock`.
  - `SchemaFieldAliases::load` ×2 per conversion; `ancestor_schemas` re-reads
    the group file it was just handed.
  - `validate_reference_fidelity` (`4124`) is O(missing × issues), re-parsing
    both paths each time (31.6% of scenario conversion). Pre-parse issue
    paths once; hoist `field_path`.
  - `convert_struct` (`4517-4606`) re-matches fields for every element:
    memoize a plan per (source struct, target struct).
  - `clean_field_key` (`35`) costs 5–7 allocations per call and is called on
    layout-constant names in loops; cache per (layout, field index), add a
    fast path for names without `/[#:`.
  - Per-field path `String`s built even when nothing is reported (`4607`,
    `5678`, `5713`): use a push/truncate buffer.
  - `template_option_counts` (`2069`) re-reads and re-parses the chosen
    template every conversion.

- [ ] **P13. iostore hot spots** — Verified (code) / unmeasured
  - Oodle: `container/oodle.rs:61` builds a new `oozextract::Extractor` per
    64 KiB block — 3 × 256 KiB zero-filled buffers each time. Reuse one per
    thread; first confirm `read_sync` fully resets state between calls.
  - `read_chunk` / `read_prefix` (`mod.rs:667-716`): zeroed temp buffer per
    block, `extend_from_slice`, then a final `.to_vec()` of the whole chunk.
    Decompress straight into `out`; merge the two ~30-line copies into one
    `read_range`.
  - `flattened_schema` (`object/block.rs:420` → `usmap.rs:197`) re-walks and
    re-sorts the super chain for every struct value. Memoize per struct.
  - `object/block.rs:629` `Arc::from(prop.name.as_str())` allocates per
    property; make `UsmapProperty.name` an `Arc<str>` and clone the `Arc`.
  - `find_chunk` (`mod.rs:616`) scans the TOC linearly; lazy
    `HashMap<[u8; 12], u32>` (keep the perfect-hash validation).
  - Reported, lower: user-struct layout cache dropped per package
    (`world.rs:719`); `register_generated_classes` fully decompresses every
    package (`world.rs:616`); 27 `ar.raw(&mut X.clone(), n)` write sites
    (add `raw_from(&[u8])`); `Usmap::meteorite()` re-parsed per fuzz input
    and in ~20 tests (add a shared `OnceLock`); name map cloned and SipHashed
    per package (`name_map.rs:146`).

- [ ] **P14. Bitmap / audio / classic copies** — Verified (code) / unmeasured
  - `bitmap/mod.rs:553` `resolve_image_pixels`: `shared_pixels[offset..].to_vec()`
    copies from each image's offset to the **end** of the blob — quadratic in
    image count, done eagerly in `Bitmap::new`. Borrow instead (`Cow`).
  - `audio/wwise/ww_vorbis.rs:97-112` + `715-721`: every payload byte goes
    through 8 `put_bit` calls. Add a byte-level `write_bytes` (keep any
    page-flush behaviour in `flush_bits` intact).
  - `classic.rs:724`, `1014`: `raw_data[..].to_vec()` per block element;
    pass the slice. `encode_block` (`~1392`) clones `raw_data`.
    `sync_fixed_counts` / `encode_struct_trailing` do a linear `find` per
    field (both visible in the H2 write profile).
  - Reported, lower: decoded bitmap output allocated per mip then appended
    (`bitmap/mod.rs:981`); X360 `level_data` goes through 4 buffers
    (`xbox360.rs:498`); Xbox ADPCM allocates 2 `Vec`s per 36-byte block
    (`audio/classic.rs:98`); keyframe bracket search restarts at 0 each frame
    (`animation/codec.rs:855`); per-frame cbuffer rebuild clones names and
    string-matches (`render_method/cbuffer.rs:128`); object-space corrections
    redo FK with fresh allocations per frame (`animation/pose.rs:190`);
    `CodebookLibrary::load()` per `.wem` (`ww_vorbis.rs:453`).

- [ ] **P15. Geometry allocation churn** — Reported
  - Two heap `Vec`s per vertex in `JmsVertex` / `AssVertex` / `AuthorVertex`
    (`node_sets`, `uvs`); ≤4 influences fits a `SmallVec`/`ArrayVec`
    (public API change).
  - `render_model.rs:2340` `read_raw_vertex` collects two `Vec`s per vertex.
  - `render_import.rs:280` builds 3 lowercase `String`s per triangle for a
    section key, then linear-searches.
  - `jms.rs:1376` / `ass.rs:1951` re-resolve the material per collision
    surface (allocation + O(materials) scan).
  - `weld.rs:326` clones every vertex to normalize normals; `:349`
    `Vec<Vec<u32>>`; `:453` allocates per candidate comparison.
  - `prt.rs:209` allocates a ray stack per ray; the per-vertex loop is
    embarrassingly parallel.

---

## Tier 3 — duplication

- [ ] **D1. iostore skip readers duplicate the typed tail models** — Reported
  - ~17 wire formats written twice: `tails.rs` "skip" readers vs
    `tail_models.rs` typed `read`s (static mesh buffers, LODs, skeletal
    sections, shader maps, reference skeleton, Nanite, …). Implement each skip
    as `Model::read(r).map(drop)`. ≈1,000–1,500 lines.
  - `roundtrip_tail_exact` (`tail_models.rs:6235-7212`): 48 boilerplate arms →
    a `TailModel` trait + one generic helper. ≈400 lines.
  - Nanite `FResources` parsed three ways, one a byte scan
    (`asset/nanite.rs:1198`).
  - `container/writer.rs`: duplicate / delete / rename each copy the same
    preflight + commit sequence (≈180 lines); TOC parse and directory-index
    code split between `mod.rs` and `writer.rs` (three fstring readers, `be40`
    encode/decode apart, `TOC_MAGIC` defined twice).
  - `BulkArray`, `WeightedRandomSampler`, `read_i32_array` duplicated between
    `hand_written.rs` and `tail_models.rs`; `count()` vs `native_count`.

- [ ] **D2. Geometry readers, walkers and math** — Reported
  - Mesh index walker ×7 (`raw indices`/`raw indices32`, strip check,
    `i16 as u16` start wrap, strip-or-list) across `jms.rs`, `ass.rs`,
    `particle_model.rs`, `render_model.rs` → `geometry::MeshIndices`.
  - Vertex readers ×8 feeding 6 near-identical vertex structs.
  - ≈250 lines of matrix/quaternion/vector math that `math.rs` already has
    (`gltf.rs:459-563`, `render_import.rs:706-735`,
    `physics_import.rs:543-567`, f32/f64 `v3` helpers in 5+ files,
    normalize-with-floor inlined 10+ times).
  - Importer tag-writing helpers (`with_block`/`try_set`/`string_id`) ×4;
    first-child/sibling derivation ×4; `JMS_TO_WORLD` defined ×4.
  - UE bake setup shared between `jms.rs:498` and `render_model.rs:918`;
    `jms.rs:392` `from_ue_skeletal_mesh` has no callers.
  - H3 vs H2 sbsp→ASS builders share 166 identical lines (`ass.rs:312-1095`).
  - `write_floats` ×3, `EdgeRow` cache ×3, shader-basename extraction ×8
    with two different methods, material find-or-insert ×7.
  - Leave `collision_verify.rs`'s independent decode alone — it is the oracle.

- [ ] **D3. Converter walkers, value matches and normalizers** — Reported
  - 14 hand-written recursive walkers (`convert/mod.rs` + `resources.rs`),
    many O(n²) via `fields().nth(i)` + `field_at_mut(i)` → one
    `visit_structs` / `visit_structs_mut` in `api.rs`. ≈250–300 lines.
  - `initialize_block_index_defaults` (`4307`) duplicates
    `api.rs:1440 default_new_element_block_indices` and runs twice per new
    element.
  - Per-variant `TagFieldData` matches (`default_field_value`,
    `value_is_meaningful`, `integer_value`, `set_int_field`,
    `clear_flags_by_name`, `read_int_any`, …) → methods on `TagFieldData`
    generated from one variant list.
  - 7 name normalizers (`clean_field_key`, `normalize_option_name`,
    `option_name_aliases`, `squashed_parameter_name`, the inline split in
    `field_names_match`, `typed_enums::fold`, `clean_field_name`); a
    documented bug came from mixing two of them.
  - Catalog: 9 near-identical validation loops and 9 lookup methods that
    re-normalize rule strings per query.
  - 56 `#[ignore] scratch_*` diagnostics (≈4k lines) in `convert/mod.rs` →
    `examples/`; `collect_files` duplicates `walk_files`.

- [ ] **D4. Core** — Reported
  - Path descent written 3 times (`path.rs:51-277`) plus the 10
    `descend_*`/`_mut` helpers and `data.rs:746-808`.
  - Element size computed twice (`api.rs:1072`, `data.rs:1301`).
  - Chunk header read + signature + version check inlined ~6 times;
    `read_tag_chunk_header` ≡ `read_chunk_header`; five near-identical leaf
    arms in `read_sub_chunks` / `write_sub_chunks`.
  - Top-level stream loop twice (`file.rs` `read_from` and
    `read_dependency_references`).
  - LCS alignment twice (`schema_compat.rs:1210`, `schema_compare.rs:192`).
  - `_meta.json` + parent-chain walk ×5 (see P4).
  - ~20 typed readers in `api.rs:323-686` share one shape.

- [ ] **D5. Bitmap and shared numeric helpers** — Reported
  - `decode_bc1/2/3/7` identical but for the block function
    (`bitmap/decode.rs:511`); 7 hand-copied BC4-style block loops.
  - `dds::decode_dxn_mono_alpha` + its `unpack_bc4_alpha_block` have no
    callers and duplicate `decode.rs`.
  - `half_to_f32` ×3 (`bitmap/decode.rs:451`, `iostore/asset/texture2d.rs:893`,
    `render_geometry/decode.rs:446`; the first two use `powi`); unit clamp ×2.
  - Endian slice readers repeated in `animation/classic.rs:505` vs
    `fields.rs:541`; `yaw_quat` ×2; `lut()` ×2 in `tag_function`.
  - `xbox360.rs:98` `swap_byte_pairs` has no callers.

---

## Build configuration

- [ ] **C1. Dev profile** — Verified (`target/debug` is 66 GB)
  - `[profile.dev] debug = "line-tables-only"`;
    `[profile.dev.package."*"] opt-level = 3` so corpus tests run the
    decoders (oozextract, miniz/zlib-rs, bcdec_rs, lewton, blake3, tiff)
    optimized; `[profile.dev.build-override] opt-level = 0`.
  - One-time `cargo clean` (27 GB of `target/debug/examples` is stale probes).

- [ ] **C2. Release / dist profiles** — Verified: **no runtime gain**
  - Fat LTO + `codegen-units = 1` measured on vbench: read 0.78 → 0.83 s,
    write unchanged. It only shrinks the shell binary 6.9 → 4.6 MB and roughly
    doubles a full release build (40 → 75 s).
  - If wanted for artifacts: a `[profile.dist]` inheriting release with
    `lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`. **Never**
    `panic = "abort"` — `convert` uses `catch_unwind` and PyO3 needs unwinding.
  - A `[profile.profiling]` (release + `debug = true`) for samply / Instruments.

- [ ] **C3. flate2 zlib-rs backend** — Reported, unmeasured
  - `flate2 = { version = "1.1", features = ["zlib-rs"] }` in
    `blam-tags/Cargo.toml` wins over tiff's default backend. Measure on
    info-stream unpacking before keeping.

- [ ] **C4. mimalloc for the CLI** — Reported, unmeasured
  - Only if a benchmark shows it; not in the PyO3 cdylib.

---

## Bugs found during the review

- [ ] **B1. JMS render export corrupts triangle indices on a missing vertex** — Verified
  - `jms.rs:3142`: `let Some(v) = raw_v.element(vi as usize) else { continue; };`
    pushes fewer than 3 vertices, but the triangle still uses
    `[base, base + 1, base + 2]`. Same shape at `~3298`.
- [ ] **B2. `"PRT vertex type"` set twice** — Reported
  - `render_import.rs:~996` and `~1002`; the first write is dead.
- [ ] **B3. Collision material match can false-match** — Reported
  - `jms.rs:~1400` / `ass.rs:~1951`: `ends_with(cell_label)` matches
    `"x perm region"` for `"perm region"`.
- [ ] **B4. `.utoc` directory-index parser can panic or loop on malformed input** — Reported
  - `iostore/mod.rs:~785-830`: unchecked `di[o..o + 4]` indexing and no cycle
    guard on the sibling / next-file chains.
- [ ] **B5. 4 H2 roundtrip mismatches in the vbench run** — Unexplained
  - vbench maps file extension → `definitions/halo2_mcc/<ext>.json`, which may
    explain the 540 failures, but 4 tags decoded and re-encoded differently.
    Identify them against the 28,195 / 28,195 gate.

---

## Measurement log

| Date | Change | H3 read | H3 write | H2 read | H2 write | CE read | CE write |
|---|---|---|---|---|---|---|---|
| 2026-09-25 | baseline | 4.24 s (path) / 0.78 s (bytes) | 1.24 s | 11.7 s | 26.6 s | 6.9 s | 3.7 s |
| 2026-09-25 | P1 + P2 + P3 | 1.05 s (path) / 0.75 s (bytes) | 1.18 s | 9.9 s | 4.1 s | 3.4 s | 0.59 s |
| 2026-09-25 | + P4 + P5 + P6 | 1.12 s (path) / 0.79 s (bytes)¹ | 1.27 s¹ | 3.8 s | 4.2 s | 0.77 s | 0.60 s |
| 2026-09-25 | + P7 | — | 0.49–0.73 s | — | — | — | — |

¹ Single runs of H3 vary ±5%; the P5 A/B over 3 reps is the reliable number (−3% read).
