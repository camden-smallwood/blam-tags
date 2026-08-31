# `shader_compile` — rebuild compiled-shader tags from HLSL

A Windows-only, feature-gated (`shader-compile`) port of the H3EK / MCC
`tool.exe` shader-compile pipeline. It compiles HLSL to Shader Model 5 with the
same `d3dcompiler_47.dll` the kit uses, reflects the result into a rasterizer
constant table, and writes both into `pixel_shader` / `vertex_shader` /
`compute_shader` (and, in progress, `global_*_shader`) tags — **byte-for-byte
identical to what the kit ships.**

This is the compiler the [`convert`](../convert/) path refuses these tag groups
for want of ("a `pixel_shader` holds compiled … microcode … turning one into the
other is a shader compiler"): the PC slot it produces is bit-exact, so those
conversions can regenerate the shader instead of copying incompatible bytes.

## Result

Census over the kit's single-variant `screen` postprocess shaders (compile from
`source\rasterizer\hlsl`, compare to the stock `tags\rasterizer\shaders\*` tags):

```
tested:                     99
bytecode byte-identical:    97/99
constant table byte-exact:  99/99
```

The two bytecode outliers (`screenshot_combine`, `screenshot_combine_dof`) share
their `final_composite_base.hlsl` body with shaders that match exactly and differ
only inside a bloom b-spline helper — consistent with those two stock tags being
older builds, not a compile error. Every constant table is exact.

Why bit-exact is even possible: DXBC is deterministic given (compiler binary,
source, includes, defines, entry, target, flags). We load the same system
`d3dcompiler_47.dll`, resolve `#include` from the same source tree, and emit the
same macro list, entry name, `*_5_0` profile, and `D3DCOMPILE_SKIP_VALIDATION`
flag the engine does.

## How it maps to the engine

| module | engine function | role |
|---|---|---|
| `d3d` | `sub_140067194` | load `d3dcompiler_47.dll`, resolve `D3DCompile`/`D3DPreprocess`/`D3DReflect` |
| `entry` | `off_14185F460…F940` | entry-name / vertex-type / profile / deform tables, read verbatim from the image |
| `macros` | `sub_140C587A0` | the `#define` list (base + category/option), in the engine's order |
| `include` | `c_tag_d3dx_include::Open` | `#include` resolution (source tree or hlsl tag) |
| `param_alloc` | `sub_1400666DC` / `sub_140064A6C` | flag translation + the two-pass PARAM_ALLOC reflect/recompile |
| `reflect` + `cbuffer_index` | `sub_140066CAC` / `sub_140C55890` / `sub_140CBD050` | reflection → constant table, with the 113-entry cbuffer-name→index table |
| `emit` | `sub_140C56B50` / `sub_140C56D60` | write bytecode + tables into the tag ("splut" + entry-point refs) |

The reverse-engineered contract is documented in the h3lm repo's
`docs/SHADER_COMPILE.md`.

## Usage

```rust
use blam_tags::shader_compile::{ShaderCompiler, CompileOutcome, include::DiskSource};
use blam_tags::shader_compile::entry::Stage;
use blam_tags::shader_compile::macros::Platform;

let src = DiskSource::new(r"…\H3EK\source\rasterizer\hlsl");
let sc = ShaderCompiler::load(&src, None)?;            // None = system d3dcompiler_47.dll
let CompileOutcome::Compiled(out) =
    sc.compile_variant("add", Stage::Pixel, 0, /*vtype*/7, 0, Platform::Pc, &[])? else { return Ok(()) };
// out.bytecode : Vec<u8> DXBC ; out.table : ConstantTable
```

`examples/shader_compile_add.rs` (single shader, compares to a stock tag) and
`examples/shader_compile_census.rs` (the census above) are runnable harnesses.

## Wired into `convert`

The byte-order conversion path (`convert::analyze_conversion_inner`) refuses
`pixel_shader` / `vertex_shader` / `compute_shader` on an Xbox-360→kit upgrade
because compiled GPU code cannot be byte-swapped across instruction sets. It now
accepts an optional [`convert::ShaderRecompiler`]; when one is supplied it
**regenerates the shader from the kit's HLSL** rather than refusing.

`RawShaderRecompiler` is the implementation. Drive a conversion with it via
`convert::analyze_conversion_with_shader_recompiler`:

```rust
let src = DiskSource::new(hlsl_root);
let recompiler = RawShaderRecompiler { provider: &src, dll_path: None, base_name: "add".into() };
let draft = convert::analyze_conversion_with_shader_recompiler(
    &source, "halo3_mcc", "halo3_mcc", defs, Some(&templates), &recompiler)?;
// draft.tag carries byte-exact recompiled bytecode
```

Verified end-to-end in `examples/convert_shader_e2e.rs`: a compiled shader that
would otherwise be refused converts with byte-identical bytecode. Groups the raw
path does not cover yet (`global_*_shader`, `render_method_template`) and names
with no matching HLSL are declined, so the conversion keeps its refusal for
those. Without the `shader-compile` feature the hook is simply never passed and
the refusal is unchanged.

## Scope / limitations

- **PC slot only, for now.** The Durango slot needs the Xbox XDK
  `D3DCompiler_47_xdk.dll` (not shipped in H3EK). The pipeline supports it —
  `ShaderCompiler::load(Some(xdk_path))` with `Platform::Durango` — when that DLL
  is available. Xenon is dead in the engine and not produced.
- **Raw-`.hlsl` path is proven.** The `render_method` template path (material
  shaders through category/option macros + the full PARAM_ALLOC wrapper, plus
  `global_*_shader`/routing) reuses every piece here but its multi-entry driver
  and byte-exact wrapper synthesis are the next step.
