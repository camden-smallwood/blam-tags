# Making the IoStore object layer bidirectional

A plan to turn `iostore::unversioned` from a one-way reader into a codec that
both reads and writes cooked Unreal exports, and to split the `iostore` module
into layers that match what it actually does.

Every number below is measured against the shipped Campaign Evolved corpus
(1,153,964 runtime native-class exports across 103,867 packages), not estimated.

---

## 1. Where we are

### 1.1 The module is a skipper, not a codec

`unversioned.rs` is 5,241 lines. Roughly seventy helpers, and every one of them
has the shape "advance a cursor, return nothing usable": `Result<()>`,
`Result<bool>`, `Result<usize>`. There are 228 `r.take(n)` calls that discard
their bytes outright. There is no `Writer` type anywhere in the module.

So "add writing" is not a matter of adding functions. The *shape* of every
function forbids it. The byte-level facts encoded in those functions are correct
and hard-won — the problem is that they are expressed in a form only a reader can
use.

### 1.2 Three concerns are fused in one file

| Concern | Approx. lines |
|---|---|
| Byte cursor (`Reader`) | 90 |
| `FUnversionedHeader` codec | 55 |
| Property value codec (`read_value`) | 135 |
| Natively-serialized structs (~30 of them) | 400 |
| Per-class `Serialize` tails (64 arms, one `match`) | 1,320 |
| `UStruct`/`UDataTable`/`UFunction` reflection recovery | 350 |
| CE mesh-sync extraction (typed, game-specific) | 300 |
| Assorted helpers | ~2,500 |

None of these should live in the same file, and the 1,320-line `match` has no
per-class tests because there is no seam to test at.

### 1.3 Measured write-readiness

| | |
|---|---|
| Property block end known exactly | **1,153,963 / 1,153,964** |
| Export is *only* a property block + `UObject` trailer | 332,118 (**28.78%**) |
| Export has a native tail after the block | 821,845 (**71.22%**), **4.88 GiB** |
| `Native(raw bytes)` values — already lossless | 1,958,928 |
| `Opaque` values, total | 2,096,317 |
| — zero-masked non-scalars (**benign**: serialize no bytes) | 2,078,706 |
| — **consumed-and-discarded (blocks writing)** | **17,611**, in 14,740 exports (**1.28%**), in **12 classes** |
| Exports of a class whose schema declares a static array | 42,172 (3.65%) |
| Name-map entries ending `_<digits>` (re-split wrongly) | 137 of 663,971 |

Two of these change the plan.

**The 100% coverage work already bought most of the writer.** Knowing the
property block's exact end for 1,153,963 of 1,153,964 exports means a writer can
emit `[rewritten block][tail replayed verbatim]` and never model a byte of that
4.88 GiB. That is a bounded project rather than an open-ended one.

**The `Opaque` problem is 1.28%, not 24%.** 99.2% of discarded values are
zero-masked non-scalars that write no bytes anyway; they need a flag, not a
value. The genuine blockers are 17,611 values in twelve classes
(`NiagaraComponent` 12,804, `SphereComponent` 1,930, `TimelineComponent` 1,834,
`PCGGraphCompilationData` 420, `BoxComponent` 274, `WidgetAnimation` 235, the
`MovieSceneEvent*` sections, `PCGGraph*`, `TriggerBox`, `TriggerVolume`) — which
reduce to four value kinds: delegates, multicast delegates, field paths, and
`FInstancedPropertyBag`.

### 1.4 Fidelity defects (read-side bugs today, not only write blockers)

1. **Static arrays collapse.** `flattened_properties` correctly emits a property
   `array_dim` times, but `read_struct_with_schema` does
   `out.insert(prop.name.clone(), value)` into a `BTreeMap`, so N values become
   the last one. `MaterialInstance::PhysicalMaterialMap[8]` reads as a single
   element today. 76 classes, 42,172 exports.
2. **`FName` is flattened to a `String`** as `"{base}_{number-1}"`.
   `break_down_name_string` reverses it only when the base is not itself
   `Foo_<digits>`; 137 entries (`Shield_0`, `InstancedFoliageActor_25600_1_0`, …)
   re-split into a different pair.
3. **`zero_value()` returns `Opaque` for `Name` and `Enum`.** A zero-masked
   `FName` is `NAME_None` and a zero-masked enum is `0` — both are real values
   being thrown away. Measured: **1,280,279 enums and 221,430 names**, i.e. 72%
   of all zero-masked values, are discarded this way. This is the single largest
   data-loss bug in the reader, and it is invisible because the value that goes
   missing is the *default* one.
4. `PropertyType::Optional` unset and "modeled away" both become `Opaque`.
5. Container removal counts are read and dropped; `-1` (replace-whole) is
   indistinguishable from `0`.
6. `fstring` truncates at the first NUL, discards the rest of the declared
   buffer, and does not record the ASCII/UTF-16 choice.
7. `BTreeMap` is alphabetical; the header is schema-index-ordered. Presence order
   is lost.
8. **46 silent `r.o = at; return Ok(false)` degradations.** The reason goes to
   stderr behind `BLAM_TAIL_WHY` and is otherwise lost; callers learn "there is a
   tail" with no diagnosis.
9. 17 runtime `std::env::var` calls, several on per-element paths, while
   `trace_enabled()` in the same file demonstrates the `OnceLock` pattern.
10. Inconsistent limits: `0..=1_000_000` in three places, `10_000_000` in
    `native_count`, `4096` in `read_bulk_array`, `depth > 32` in three places.
    `Reader::take` uses unchecked `self.o + n`, and several call sites pass
    `n * K` computed before the bounds check.
11. `native_struct_size` encodes UE-5.5.4-specific facts as if universal — no
    engine-version gate, so a patched build mis-sizes silently.
12. Three unit tests for 5,241 lines, all synthetic. No fixtures from real data.
    No fuzz target, though this parser now reads third-party mod containers.

---

## 2. What the engine actually does

Read out of `5.5.4-release`
(`Engine/Source/Runtime/CoreUObject/Private/Serialization/UnversionedPropertySerialization.cpp`
and the `UObject/Property*.cpp` family), because guessing at these is what
produces silent desyncs.

### 2.1 The header can be regenerated — proven

`FUnversionedHeaderBuilder` (line 795) is about thirty lines and fully
deterministic:

```
IncludeProperty(bIsZero):
    if last.ValueNum == 127: TrimZeroMask(last); push fragment
    ++last.ValueNum;  last.bHasAnyZeroes |= bIsZero;  ZeroMask.Add(bIsZero)
ExcludeProperty():
    if last.ValueNum != 0 || last.SkipNum == 127: TrimZeroMask(last); push fragment
    ++last.SkipNum
Finalize():
    TrimZeroMask(last)
    while Fragments.Num() > 1 && last.ValueNum == 0: pop
    last.bIsLast = true
TrimZeroMask(f):  if !f.bHasAnyZeroes: drop the last f.ValueNum bits of ZeroMask
```

Ported literally and run against every export in the corpus:

```
headers examined     1153838
regenerated exactly  1153836  (99.9998%)
differ                     2
```

The two are `RigVM` and `RigHierarchy`, the two classes that provably have no
property block at all (their `Serialize` never calls `Super`), so the probe was
misapplying itself. **Header regeneration is 100% on every export that has a
header.** No original bytes need to be retained.

Two pieces of state beyond the `(schema index, is-zero)` list are required:

* **The flattened schema length.** UE walks the whole schema calling
  Include/Exclude per property, and `Finalize` pops trailing value-less fragments
  only *down to one* — so a block with nothing present still encodes
  `min(schema_len, 127)` skips. Stopping at the last present index is right for
  every other case and wrong for this one (62,606 exports).
* **`leading_empty_fragments: u8`.** Measured: always exactly **2**, only ever in
  `/Game/Tags` (12,161 packages), never interleaved anywhere in the corpus.

That second one is a finding worth stating plainly: **CE's tag wrappers were not
written by Epic's builder.** `FUnversionedHeaderBuilder` cannot emit an empty
`(skip 0, value 0)` fragment, and every CE tag `.uasset` header begins with two
of them. i343's tag-cooking tool writes its own headers. UE's *loader* tolerates
them (`FIterator::Skip` walks past `ValueNum == 0`), so a regenerated header
without them would still load — but these are exactly the packages Baboon writes,
so reproducing them keeps our output diffable against the shipped originals.

### 2.2 Value-level rules now confirmed rather than guessed

* **Integer width is the property's `GetMinAlignment()`, not its declared size**
  (`SerializeAsInteger`, line 234). On x64 these coincide for every numeric type,
  so the current widths are right — but the writer must derive width the same way
  or it will be wrong the first time they diverge.
* **`CanSerializeAsInteger`** claims `FNumericProperty` and `FEnumProperty`, plus
  `FBoolProperty` only when `IsNativeBool()`. A bitfield bool goes through
  `SerializeItem` and still costs one byte (the redundant-byte comment at line
  113).
* **`CanSerializeAsZero`** needs `CPF_ZeroConstructor | CPF_NoDestructor` for
  scalars and `STRUCT_Atomic` for structs, neither of which the `.usmap` records.
  See §2.3 — the corpus answers this without them.
* **`bDense` / `IsDefault`** decides which properties are present at all by
  comparing against the CDO, which we do not have. Same answer: replay what was
  observed; *including* a previously-excluded property is always safe, excluding
  one is not.
* **`FOptionalProperty::SerializeItem`** encodes "is set" via
  `Record.TryEnterField`, i.e. an `FArchive` bool — **four bytes** — then the
  inner value through `SerializeItem` (the container path). The existing
  "measured 4 bytes" reading is correct, and the inner value's `in_container=true`
  is correct.
* **`FDelegateProperty::SerializeItem`** is `FScriptDelegate::Serialize` =
  `FPackageIndex` + `FName`. Matches the current reader.
* **`FSoftObjectProperty::SerializeItem`** is `FSoftObjectPtr::Serialize` → two
  `FName`s then an `FString`. Note this is byte-identical to the "different" form
  the PCG tail reads (`take(16)` + `fstring`); the comment there claiming they
  differ is wrong and should go.

### 2.3 The zero-maskable set is closed — measured, so no binary needed

`CanSerializeAsZero` is the one rule that needs data the `.usmap` lacks. Rather
than recover `EPropertyFlags`/`EStructFlags` from the shipped executable, measure
what the cooker actually zero-masks. Over all 2,078,706 zero-masked non-scalar
values in the corpus:

| Type | Count |
|---|---|
| `Enum` | 1,280,279 |
| `Struct:Guid` | 544,929 |
| `Name` | 221,430 |
| `Struct:Vector` | 22,351 |
| `Struct:IntPoint` / `Struct:IntVector` | 2,921 each |
| `Struct:Rotator` | 2,754 |
| `Struct:Int64Vector` | 600 |
| `Struct:Vector2D` | 206 |
| `Struct:LinearColor` | 194 |
| `Struct:Vector2f` | 113 |
| `Struct:Box`, `Struct:DeprecateSlateVector2D` | 4 each |

**Zero strings, arrays, maps, sets, texts, delegates, soft objects, optionals or
field paths** — exactly as `CanSerializeAsZero` predicts, since all of those have
destructors. The struct set is twelve entries, every one already in
`native_struct_size`.

So the rule is derivable from the `.usmap` alone: scalars and enums and names are
zero-maskable; a struct is zero-maskable iff it is in the fixed-size native table
and small. Anything outside that set becomes a hard error rather than a silent
guess, and the round-trip gate catches a wrong classification the moment the
header bytes differ.

This is what makes edited values regenerable too, not just replayed ones — which
closes the last gap in §5.

Cross-checked against the UHT dump, which records `USTRUCT` specifiers: **ten of
the eleven are `USTRUCT(Atomic, BlueprintType, Immutable, NoExport)`**. The
exception, `FDeprecateSlateVector2D`, derives `FVector2f` — and
`STRUCT_Inherit = STRUCT_HasInstancedReference | STRUCT_Atomic` (Class.h:880)
makes Atomic an *inherited* flag, so it is atomic too. The whole dump contains
only **61 Atomic structs**. The rule is therefore exact and enumerable:

> A struct is zero-maskable iff `STRUCT_Atomic` (declared or inherited) and its
> size is under sixteen integers of its alignment.

### 2.4 Which structs have a custom serializer — and the audit list

`UScriptStruct::SerializeItem` (Class.cpp:3189) dispatches on
`UseNativeSerialization()` → `STRUCT_SerializeNative` → `ICppStructOps::Serialize`.
Crucially, a `Serialize` that returns **false** means "I wrote a prefix but did
not consume the struct", and the normal property block still follows — which is
the `FMaterialOverrideNanite` case already documented in the reader. So
`WithSerializer = true` does **not** imply "not a property block"; it means the
dispatch decision needs auditing.

`STRUCT_SerializeNative` is runtime-computed and therefore in neither the UHT
dump nor the static registration tables (§2.5). But the *declaration* —
`TStructOpsTypeTraits<T>::WithSerializer` — is in UE source, and GitHub code
search reaches the private repo. 185 candidate files, 130 of which exist at
5.5.4, yield **165 structs declaring a custom serializer**. Cross-referenced
against the 3,368 structs CE ships (UHT dump) and against the 77 we model
natively (50 fixed-size + 27 variable):

> **51 structs that CE ships, that have a custom serializer, and that we do not
> model.**

**A declaration is not a defect.** Many are editor-only paths a cooked package
never contains (`ExpressionInput`, 531 uses, is reached only through
`MaterialEditorOnlyData`), and a `Serialize` that returns **false** leaves the
ordinary property block in place. So this is a list of *unaudited dispatch
decisions* — currently resolved by "assume property block" and validated only by
the fact that CE's data decodes.

A caution learned the hard way while building this list. The first pass matched
`WithSerializer = true` without stripping comments, and produced exactly two
false positives — `FFrameRate` and `FFrameTime` — which were then written up here
as confirmed latent mis-decodes. They are not. Epic disables both deliberately:

```cpp
// The native function has a custom serializer but assets have already been created
// with the generic UPROPERTY serializer, so we can't switch them to use a custom
// serializer without breaking assets (creates mismatched sizes in data).
// WithSerializer = true,
```

Both are property blocks, exactly as the reader already treats them, and the
100% byte-accounting was right where the cross-reference was wrong. The lesson
generalises: **when source and corpus disagree, the corpus is evidence and the
source reading is a hypothesis.** Every entry below is a hypothesis until its
`Serialize` body is read.

**Task:** classify each of the 51 into `writes-its-own-format` (model it),
`writes-a-prefix-then-returns-false` (prefix + property block), or
`returns-false-immediately` / `declaration disabled` (property block). Each
classification is one `Serialize` body from UE source. Record the verdict and its
citation next to the entry in the struct registry, so the table stops being a
list of measurements and becomes a list of decisions.

### 2.5 What each source of truth can and cannot answer

| Question | Source | Have it? |
|---|---|---|
| Property order, types, `array_dim` | `.usmap` | yes |
| Serialization algorithms (header, values, containers) | UE 5.5.4 source | yes, via `gh api` |
| `STRUCT_Atomic`, `Immutable`, `NoExport`; CE class/struct declarations | UHT dump | yes, on disk |
| Which structs have a custom serializer | UE source `TStructOpsTypeTraits` | yes, via code search (§2.4) |
| `STRUCT_SerializeNative` as a *flag* | runtime only | no — and not needed |
| CE-specific classes' `Serialize` overrides | — | **they have none** |
| Nanite container, page encoding, bit layout | UE source (§2.6) | yes |

Three entries deserve comment.

**No CE class writes a native tail.** All 64 arms of the tail dispatcher are
stock UE or Wwise plugin classes — not one is a `Blam*` or `Halo*`. Together with
100% byte-completeness over 1,153,964 exports, that proves CE's own classes are
pure property-block classes (plus whatever their stock bases append). The
executable is therefore *not* a dependency for modelling tails, which is what it
looked like before the dispatcher was enumerated.

**`STRUCT_SerializeNative` as a runtime flag is not needed.** It is one of
`STRUCT_ComputedFlags` (Class.h:883), set by `PrepareCppStructOps` from
`ICppStructOps::HasSerializer()`, so it exists only in a live `UScriptStruct` —
recovering it would mean a UE4SS-style runtime dump or classifying
`ICppStructOps` vtables in the binary. Neither is necessary: the *declaration*
that produces the flag is in UE source and code search reaches it (§2.4).

**Nanite is in the repo too** — see §2.6. Nothing in this project needs the
executable.

### 2.6 Nanite comes from source, including the encoder

Present and complete at 5.5.4:

| File | Size | What it is |
|---|---|---|
| `Runtime/Engine/Private/Rendering/NaniteResources.cpp` | 109 KB | `FResources::SerializeInternal` — the cooked container: `StreamablePages` bulk data, `RootData`, the cooked-only branches |
| `Developer/NaniteBuilder/Private/NaniteEncode.cpp` | 176 KB | **the encoder** — `WritePages`, `PackCluster`, `EncodeGeometryData`, `FPageSections` |
| `Shaders/Private/Nanite/NaniteDataDecode.ush` | 37 KB | the GPU-side unpack — the authoritative bit layout |
| `Shaders/Private/Nanite/NaniteAttributeDecode.ush` | 33 KB | attribute/UV/tangent unpack |
| `Runtime/Engine/Private/Rendering/NaniteStreamingManager.cpp` | 140 KB | page streaming and fixups |

Having the **encoder** matters more than having a decoder. `nanite.rs` is
currently a partial port of CUE4Parse's *reader*, which can only ever produce a
reader; `NaniteEncode.cpp` states how the pages are built, which is what a
bidirectional codec actually needs. The `.ush` files pin the bit packing exactly,
so the two ends can be checked against each other rather than against a
reimplementation.

This makes Nanite an ordinary — if large — Phase 4 tail, not a reverse-engineering
project.

---

## 3. Module reorganisation

The `iostore` module currently mixes four layers in one flat directory. They form
a strict downward dependency chain, so they should be four directories.

```
iostore/
  mod.rs                     re-exports + IoStoreError only

  container/                 layer 1 — bytes in a .utoc/.ucas/.pak
    archive.rs               IoStoreArchive               (from mod.rs)
    toc.rs                   TOC parse/emit, chunk ids, offset/length codec
    directory.rs             directory index parse/emit
    header.rs                ContainerHeader              (was container_header.rs)
    pak.rs  oodle.rs
    writer.rs                OverrideContainerWriter
    cityhash.rs              (split out of writer.rs — 150 lines of hashing)

  package/                   layer 2 — one Zen package
    ser.rs                   Readable/Writeable primitives
    name_map.rs
    summary.rs               FZenPackageSummary + versioning
    header.rs                FZenPackageHeader read/write (was zen.rs)
    index.rs                 FPackageObjectIndex, FPackageId (was ue_types.rs)
    script_objects.rs
    builder.rs               NEW — assemble a package from exports

  object/                    layer 3 — one export's payload
    archive.rs               NEW — `Ar` trait + Reader + Writer
    schema.rs                Usmap                        (was usmap.rs)
    value.rs                 FName, FStr, PropValue, PropertyBlock
    block.rs                 FUnversionedHeader codec + HeaderBuilder
    property.rs              one property value, read + write
    structs/                 natively-serialized structs (WithSerializer)
      mod.rs                 registry: name -> Layout
      fixed.rs               the fixed-size table
      core.rs curves.rs material.rs niagara.rs moviescene.rs perplatform.rs
    tails/                   per-class Serialize tails
      mod.rs                 dispatch table + TailSpan
      engine.rs rendering.rs mesh.rs texture.rs niagara.rs chaos.rs
      rigvm.rs moviescene.rs pcg.rs audio.rs landscape.rs level.rs
    export.rs                a whole export: block + trailer + tails
    reflect.rs               UserDefinedStruct layout, UDataTable, UFunction script

  asset/                     layer 4 — typed decoders over layer 3
    static_mesh.rs skeletal_mesh.rs nanite.rs wwise_event.rs
    mesh_sync.rs             CE MeshSyncRegions (was in unversioned.rs)
```

`iostore::unversioned` and the other current paths stay as deprecated re-export
shims for one release, so Baboon (7 call sites) and the 40 example probes keep
building while they migrate.

---

## 4. The plan

### Phase 0 — Reorganise, no behaviour change — **DONE**

Both gates pass: `ce_coverage_matrix` output is **byte-identical** to the
baseline (831 lines, 100.0000% / 100.00%), 263 lib tests pass, and the only
`--all-targets` failures are the three that already failed at `HEAD`
(`ce_hunter_jms`, a stale probe using a since-changed `from_ue_meshes`
signature).

`unversioned.rs` went from **5,241 lines to 32** — a facade that re-exports the
public API so `iostore::unversioned::…` keeps resolving for Baboon's 7 call sites
and the 40 example probes. The old flat paths (`iostore::zen`, `iostore::usmap`,
…) survive as aliases on `iostore` itself.

Four deviations from §3 as written, each deliberate:

1. **`package/zen.rs` kept its name** rather than becoming `package/header.rs` —
   two files called `header.rs` in sibling layers is worse for navigation than
   one named after the format it parses.
2. **`package/ue_types.rs` kept its name** rather than becoming `index.rs`. It is
   a grab-bag (`FPackageObjectIndex`, `EIoStoreTocVersion`, CityHash), and
   `EIoStoreTocVersion` is really container-level. Splitting a grab-bag is its
   own task with its own risk; renaming it without splitting would only mislead.
3. **`object/` uses flat sibling files**, not `structs/` and `tails/`
   directories. The helpers moved with their subsystem, but
   `read_class_native_tail` is still one 1,320-line `match`. Subdividing *that*
   means turning it into a dispatch table, which is a behaviour-adjacent change
   and does not belong in a no-behaviour-change phase.
4. **Two modules appeared that §3 did not predict**: `object/common.rs` (counts,
   bulk arrays, container-removal prefixes — shared by property values, native
   structs and tails alike) and `object/text.rs` (`FText`).

Still outstanding from §3, deferred rather than done: `IoStoreArchive` has not
been split out of `iostore/mod.rs` into `container/{archive,toc,directory}.rs`,
and CityHash has not been split out of `container/writer.rs`.

Resulting layout:

| | | | |
|---|---|---|---|
| `container/` | `header` 645 | `pak` 503 | `oodle` 72, `writer` 1310 |
| `package/` | `zen` 1399 | `ser` 370 | `ue_types` 279, `name_map` 237, `script_objects` 149 |
| `object/` | `tails` 2913 | `structs` 620 | `usmap` 442, `block` 361, `reflect` 319, `archive` 170, `property` 163, `value` 143, `text` 137, `export` 137, `common` 79, `unversioned` 32 |
| `asset/` | `nanite` 1438 | `skeletal_mesh` 445 | `static_mesh` 298, `mesh_sync` 298, `wwise_event` 184 |

### Phase 1 — The `Ar` archive and a lossless value model

**Done so far: the header codec is bidirectional and gated.**
`object/block.rs` now carries `HeaderBuilder`, a literal port of
`FUnversionedHeaderBuilder`, behind a byte-slice API — `parse_header` /
`emit_header`. The corpus gate `ce_header_roundtrip` reports:

```
headers examined     1153836
regenerated exactly  1153836 (100.0000%)
differ               0
skipped (no block)   2  ["RigVM", "RigHierarchy"]
```

Eight unit tests pin the edge cases that make this hard to get right — the
empty-block schema length, the CE two-empty-fragment prefix, trailing-skip
elision, the 127-skip and 127-value fragment boundaries, and each zero-mask width
(u8 / u16 / u32 words) including the "no zeroes, no mask" case. The two
`Serialize`-without-`Super` classes are named and skipped rather than silently
dropped, so the denominator stays honest.

**Done: `PropValue` no longer discards anything.** `Opaque` is deleted. In its
place:

| Was | Now |
|---|---|
| `Opaque` (delegate) | `Delegate { object, function }` |
| `Opaque` (multicast) | `MulticastDelegate(Vec<(i32, String)>)` |
| `Opaque` (field path) | `FieldPath { path, owner }` |
| `Opaque` (unset optional) | `Unset` — distinct from set-but-empty |
| `Opaque` (property bag) | `Raw(Vec<u8>)` — the exact bytes consumed |

The rule is that there is no "saw it and dropped it" case: anything the reader
declines to interpret keeps its bytes, so a writer can put them back.

**Done: `zero_value` stopped throwing away defaults.** A zero-masked property is
a *value* — `LoadZero` memzeroes the storage — and returning a placeholder for
the non-scalar cases discarded **1,501,709** values across the corpus (1,280,279
enums, 221,430 names). Now a zero `FName` is `NAME_None`, a zero enum is its
underlying integer's zero, and a zero atomic struct is that many zero bytes, so
`MeshTransform::from_prop` and friends work on defaulted values instead of
hitting a hole. Types `CanSerializeAsZero` rejects yield an empty `Raw` span
rather than an invented value, since reaching that arm means a misread schema.

Three more tests cover these; the corpus gates are unchanged (byte accounting
cannot move, because zero-masked values serialize no bytes) while the decoded
data is strictly richer.

**Done: static arrays no longer collapse.** `Usmap::flattened_slots` pairs each
schema slot with its index inside a static array, so a `Thing[N]` property comes
back as an N-element `PropValue::Array` instead of whichever slot happened to be
written last. Slots the block never mentions stay `Unset` — absent is not the
same as zero.

The §1.3 figure of 42,172 was "exports of a class whose schema declares a static
array", which overstates the damage. Measured on real data, **1,935 exports
actually had more than one slot populated** and were therefore losing values:

| Exports | Property | Slots set |
|---|---|---|
| 420 | `MovieScene3DTransformSection::Rotation[3]` | 3 of 3 |
| 420 | `MovieScene3DTransformSection::Scale[3]` | 3 of 3 |
| 420 | `MovieScene3DTransformSection::Translation[3]` | 3 of 3 |
| 396 | `CurveLinearColor::FloatCurves[4]` | 4 of 4 |
| 94 / 86 / 78 | `MovieScene2DTransformSection::{Scale,Translation,Shear}[2]` | 2 of 2 |
| 12 | `BlamScenario::PlayerAppearanceCustomizations[4]` | 4 of 4 |
| 6 | `RecastNavMesh::NavMeshResolutionParams[3]` | 2.5 avg |
| 3 | `CurveVector::FloatCurves[3]` | 3 of 3 |

So every cinematic transform track was reading as one axis, and every FX colour
curve as one channel. Small in count, not small in meaning — and note one of them
is a CE class.

**Done: defects 9 and 10 — environment probes and bounds policy.**

All 17 runtime `std::env::var` calls are gone from the walk. `BLAM_TAIL_WHY` was
being re-read from the environment on per-element paths; both flags now sit
behind `OnceLock` in `object/archive.rs`, so only two `env::var` calls remain in
the whole layer and each runs once.

`object/limits.rs` replaces seven scattered literals across five files with one
documented policy, and states *why* each bound is what it is — notably that a
native tail's count is deliberately looser than a property block's, because a
`FRawStaticIndexBuffer` stores single-byte elements and a 1024×1024 plane's index
buffer really is 25,165,824 of them. It also adds `PREALLOC_CAP`: a validated
count can still be a million, so reserving for it before use would let a bogus
one allocate gigabytes; reserving a bounded amount and letting the vector grow
costs nothing on real data.

`Reader::take` computed `self.o + n`, which wraps on a hostile count and can turn
a read-past-end into an in-range slice. Now `checked_add`, with a test — this is
reachable from any third-party mod container, which this module now reads.

**Measured: container removals are all but nonexistent — and deliberately
deferred.** Making the removal count a hard error and re-running the corpus
found **5 exports out of 1,153,834** with a non-zero one, and **zero** with
`INDEX_NONE`. They fall in two classes: `NiagaraComponent` (2) and
`BlamMeshSynchronizationComponent` (3) — the latter being the class Baboon reads
for model previews, so this is not a theoretical corner.

The reader already consumes them correctly, which is why coverage is 100%; what
is lost is their *content*. Modelling that means restructuring
`PropValue::Map`/`Set`, which breaks roughly eight example probes, for 0.0004% of
exports. Deferred to Phase 2 for a reason that only becomes clear with a writer
in hand: **the removal prefix itself is reconstructible from the schema**, since
`PropertyType::Set`/`Map` says whether one is written at all. Only the removed
elements' bytes are missing, so the round-trip gate will flag exactly these 5
exports and we can decide with the cost visible. Noted here so the gap is a
decision rather than a surprise.

The same measurement incidentally shows a `TSet` currently decodes as
`PropValue::Array`, indistinguishable from a `TArray` — which is fine for the
writer, because the schema disambiguates, but worth knowing.

**The §2.4 audit is forward-compatibility work, not a bug hunt.** Two facts
reframe it. First, the reader accounts for every byte of all 1,153,834 exports;
systematic mis-dispatch of a struct that appears in cooked data would show up as
unaccounted bytes, so this is strong evidence — not proof, but strong — that none
of the 51 is mishandled *in CE's data today*. Second, sampling the candidates
with real cooked usage keeps producing the same answer:

| Struct | Verdict |
|---|---|
| `FPostProcessSettings` (12 uses) | `Serialize` returns **false** — *"Don't actually serialize, just write the custom version for PostSerialize"* (Scene.cpp:668). Property block. |
| `FFrameRate` (32 uses) | declaration commented out (§2.4). Property block. |
| `FFrameTime` (7 uses) | declaration commented out. Property block. |

So `WithSerializer = true` most often means *hook, not format*: the struct wants
a custom-version register or a legacy conversion path, and hands the actual bytes
back to the property serializer. The audit's value is guarding against a mod or a
future build that uses one of the remaining candidates in a cooked asset — real,
but not urgent, and best done as each is encountered rather than as 51 speculative
lookups.

**Done: `FName` keeps its identity.** `PropValue::Name`, `SoftObjectPath`, the
delegates and the field path now carry an `FName { index, number, text }` rather
than a display string. The two are not interconvertible: `base "Rocket" + number
5` and `base "Rocket_4" + number 0` both render `"Rocket_4"`, and 137 of the
663,971 distinct name-map entries in the corpus are of the second kind, so a
writer round-tripping through the string would re-split them and grow the name
map.

This turned out to be a *prerequisite* for the writer rather than an independent
item — emitting an `FName` needs its index, and deriving one from text is exactly
the ambiguity above. Ordering the work the other way round would have built the
writer on a value model that could not feed it.

`Reader` keeps both accessors: `fname()` where the value is stored, `name()` for
the dozen sites that only compare or print. `FName` derefs to `str` and
implements `Display`, `Ord` and `Hash`, so reading code is unaffected — `Ord` is
by text first with index and number as tie-breakers, which sorts the way a reader
expects while staying consistent with `Eq`.

**Done: the `Ar` archive, and the leaf half of the value writer.**
`object/archive.rs` defines `Ar` — the bidirectional seam — implemented by
`Reader` (loading) and a new `Writer` (saving). The writer needs **no name map**:
an `FName` carries its own index, which is the payoff of the change above.

`property.rs` gains `write_value`, covering every *leaf* shape — scalars, names,
strings, object indices, soft object paths, delegates, field paths, optionals,
containers of those, and natively sized structs whose bytes were kept verbatim —
and the public `emit_value` beside `emit_header`. Nine round-trip tests compare
*bytes*, not values, since that is the property the writer actually has to have.

It deliberately refuses three shapes rather than guessing: a nested reflected
struct, a hand-written native struct, and `FText`. All three need a property
*block* underneath, and emitting one byte-exactly needs per-slot presence and
zero-masking that the `BTreeMap` value shape has already discarded by then. That
is the `PropertyBlock` model — the first task of Phase 2, and now clearly
motivated by a concrete blocker rather than by design taste.

`Ar` carries no `is_loading`/`pos`. Both belong there the moment a single body
serves both directions and has to size a container from a count that is read on
load and derived on save; nothing does yet, so they are left out rather than
added speculatively.

**Phase 1 is complete.** Read-side data loss is closed (discarded values,
discarded defaults, collapsed static arrays, ambiguous names), the bounds have
one policy, the environment is read once, and the write path has a proven header
codec and a tested leaf-value writer.

Write each layout *once*, against a trait that either reads or writes:

```rust
trait Ar {
    fn u8(&mut self, v: &mut u8) -> Result<()>;
    fn u32(&mut self, v: &mut u32) -> Result<()>;
    fn fname(&mut self, v: &mut FName) -> Result<()>;
    fn fstring(&mut self, v: &mut FStr) -> Result<()>;
    fn raw(&mut self, v: &mut Vec<u8>, n: usize) -> Result<()>;
    fn is_loading(&self) -> bool;
    fn pos(&self) -> usize;
}
```

This mirrors UE's own `FArchive& operator<<`, which is how the ported code was
written before the port dropped that half. A layout fix then lands in read and
write simultaneously — the entire reason not to hand-write 5,000 lines of
`write_*` twins that drift apart.

Applies to: the property block, the fixed-size struct table, and the ~30
hand-written structs in `read_native_variable_struct` (those sit *inside*
property blocks, so they must round-trip). **Not** to the class tails yet.

The model:

```rust
pub struct PropertyBlock {
    entries: Vec<PropertyEntry>,   // header order
    schema_len: u16,               // §2.1: needed to regenerate the header
    leading_empty_fragments: u8,   // §2.1: 2 for CE tag wrappers, else 0
}
pub struct PropertyEntry {
    schema_index: u16,
    array_index: u8,               // which slot of a static array
    name: Arc<str>,
    value: PropValue,
    zero_masked: bool,
}
```

`PropValue` gains `Name(FName{index,number})` (carrying the resolved string
alongside so `as_str()` still works), `Str` with its encoding, real
`Delegate`/`MulticastDelegate`/`FieldPath`, `Optional(Option<..>)`, and container
`removals`. **`Opaque` is deleted** — anything the reader declines to model
records the byte span it consumed as `Raw(Vec<u8>)` instead. That single rule
converts all 17,611 blocking values into writable ones, in about twelve arms.

Fixes defects 1–7 structurally. Fold in 8–10 here too (diagnostics sink, limits
policy, `OnceLock` flags) — they are mechanical and they make Phases 2–4 far
easier to debug.

Also here: **the §2.4 struct audit** over the 51 candidates. The struct registry
gains a third outcome beyond native/reflected — **unknown**, which is a hard
error rather than a silent fall-through to the property-block path. That is what
converts "CE's data happens to decode" into "we know which structs we have
decided about".

**Gate:** coverage matrix still 100%, plus a new round-trip gate over the
property block alone at 100%.

### Phase 2 — Export round-trip

`object/export.rs`: `read_export` → `Export { block, trailer, tails }`,
`write_export` emits. Tails start as `TailSpan`s — raw byte ranges — which gives
100% export round-trip immediately.

**Gate:** `ce_roundtrip_matrix`, sibling to `ce_coverage_matrix`, reporting per
class: exports, byte-identical, and first divergence offset with a hexdump either
side. Target 1,153,964 / 1,153,964.

Add a `cargo-fuzz` target here, over both `read_export` and read→write→read.

### Phase 3 — Package assembly

`package/builder.rs`: rebuild a package from modified exports — recompute
`cooked_serial_offset` / `cooked_serial_size`, grow the name map, update
`imported_public_export_hashes` and the dependency bundles.
`FZenPackageHeader::serialize` today recomputes its *own* internal offsets by
seek-back but writes the export map verbatim, so export offsets are the caller's
job and nothing currently does it.

**Gate:** package-level round-trip — read and rewrite all 103,867 `.uasset`s byte
-identically. Much stronger than the export gate: it catches name-map ordering,
alignment and summary-offset bugs that the export gate cannot see.

Then the real test: change one property on a real CE asset, rebuild, load in
game.

### Phase 4 — Model the tails

Every tail is a stock UE or Wwise class (§2.5), so 5.5.4 source describes all of
them — including **Nanite**, inside the largest tail of all, where we have the
encoder as well as the decoder (§2.6). **No part of this project needs the
executable.**

Class by class, ordered by value: `StaticMesh` (1,309 MiB), `BodySetup`
(1,051 MiB), `SkeletalMesh` (470 MiB), `Texture2D` (453 MiB),
`InstancedStaticMeshComponent` (434 MiB), `MaterialInstanceConstant` (249 MiB),
`AnimSequence` (171 MiB).

The ordering matters for a reason worth calling out: **a modeled tail can be
validated against the span it replaces.** Convert one class, and the round-trip
gate proves the model is lossless against the bytes it used to copy. You never
have to trust a tail model — which is the opposite of the situation the reader
was built under.

### Phase 5 — Hardening

Fixture-based per-class tests (~30 committed exports, a few hundred KB, so CI
needs no game install); engine-version gating for `native_struct_size` and the
tail table; upgrading the fixed-size struct table from "measured on asset X" to
"cited from `TStructOpsTypeTraits<T>::WithSerializer`".

---

## 5. Can we discard the original bytes?

The stated preference is to regenerate rather than retain. Staged answer:

| Layer | Regenerate? |
|---|---|
| `FUnversionedHeader` | **Yes — proven**, 99.9998% (the 2 exceptions are not headers). Needs `schema_len` + `leading_empty_fragments`. |
| Property values | **Yes**, once `PropValue` is lossless (Phase 1). The gate proves it rather than asserting it. |
| Native tails | **Not yet.** 4.88 GiB, 71% of exports. Spans in Phase 2, modeled per class in Phase 4, each verified against the span it replaces. |

So full regeneration is reachable, incrementally, and at every point we know
exactly how far along we are — because the gate reports it per class rather than
as an aggregate claim.
