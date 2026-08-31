//! The compile core: flag translation (`sub_1400666DC`) and the two-pass
//! PARAM_ALLOC reflect/recompile (`sub_140064A6C`), spec §6b/§6c.
//!
//! Halo material shaders declare their parameters with `PARAM(...)` /
//! `PARAM_SAMPLER_*(...)`. The register assignment for those is *generated*:
//! preprocess to discover the tokens, compile once to reflect `$Globals`, then
//! synthesise a cbuffer / texture / sampler wrapper and compile for real. A
//! shader with no `PARAM` declarations (e.g. a postprocess shader) discovers
//! nothing and compiles its body directly.

use super::d3d::*;
use super::entry::Stage;
use super::include::Includer;
use super::macros::MacroList;
use std::ffi::CStr;

/// Translate the engine's flag word (`a8`) into `D3DCOMPILE_*` `Flags1`,
/// exactly as `sub_1400666DC` does (spec §6b). Bit 4 (the PARAM_ALLOC route) is
/// handled here in Rust, not passed to `D3DCompile`.
pub fn translate_flags(engine: u32) -> u32 {
    let mut v18 = 2 * (engine & 1); // bit0 -> SKIP_VALIDATION (0x2)
    if engine & 2 != 0 {
        v18 |= 5; // bit1 -> DEBUG | SKIP_OPTIMIZATION
    }
    let mut v19 = v18;
    if engine & 4 != 0 {
        v19 |= 0x200; // bit2 -> AVOID_FLOW_CONTROL
    }
    let mut f = v19;
    if engine & 8 != 0 {
        f |= 4; // bit3 -> SKIP_OPTIMIZATION
    }
    f
}

/// Engine flag word for the normal final compile (`SKIP_VALIDATION`).
const ENGINE_FINAL: u32 = 1;
/// Engine flag word for the debug final compile (`DEBUG|SKIP_VALIDATION|SKIP_OPTIMIZATION`).
const ENGINE_FINAL_DEBUG: u32 = 3;
/// Engine flag word for the reflection first pass (`SKIP_OPTIMIZATION`).
const ENGINE_FIRST_PASS: u32 = 8;

/// Everything one compile needs.
pub struct CompileInput<'a, 'b> {
    pub compiler: &'a Compiler,
    pub source: &'a [u8],
    pub entry: &'a CStr,
    pub target: &'a CStr,
    /// Base + category/option macros, without any PARAM_ALLOC sentinel.
    pub defines: &'a MacroList,
    pub includer: &'a Includer<'b>,
    pub stage: Stage,
    pub debug: bool,
}

/// A discovered `PARAM(...)` (from the PREPROCESS scan / `$Globals` reflection).
#[derive(Debug, Clone)]
struct UserParam {
    name: String,
    /// Byte size of the value in `$Globals` (from reflection).
    size: u32,
    /// Byte offset in `$Globals`.
    offset: u32,
    /// `D3D_SHADER_VARIABLE_TYPE`.
    svt: i32,
}

/// A discovered `PARAM_SAMPLER_*` (name + texture dimension code).
#[derive(Debug, Clone)]
struct UserSampler {
    name: String,
    /// `D3D_SRV_DIMENSION`.
    dimension: i32,
}

const MEMORY: &CStr = c"memory";
const MEMORY_ANGLED: &CStr = c"<memory>";

fn blob_err(compiler_name: &str, errs: *mut ID3DBlob) -> String {
    let msg = unsafe { ID3DBlob::to_string_lossy(errs) };
    unsafe { ID3DBlob::release(errs) };
    if msg.is_empty() {
        format!("{compiler_name} failed with no error message")
    } else {
        format!("{compiler_name}: {}", msg.trim())
    }
}

/// Compile a shader, running the PARAM_ALLOC two-pass when the source declares
/// parameters. Returns the final bytecode.
pub fn compile_shader(input: CompileInput) -> Result<Vec<u8>, String> {
    // --- pass 0: preprocess with PARAM_ALLOC_PREPROCESS, scan for tokens ---
    let mut pre_defines = input.defines.clone();
    pre_defines.define("PARAM_ALLOC_PREPROCESS", "1");
    let pre_raw = pre_defines.as_raw();

    let (params, samplers) = unsafe {
        let (hr, text, errs) = input.compiler.preprocess(
            input.source,
            MEMORY_ANGLED,
            pre_raw.as_ptr(),
            input.includer.as_ptr(),
        );
        if !succeeded(hr) {
            return Err(blob_err("D3DPreprocess", errs));
        }
        ID3DBlob::release(errs);
        let preprocessed = ID3DBlob::to_vec(text);
        ID3DBlob::release(text);
        scan_tokens(&preprocessed)
    };
    drop(pre_raw);

    // No parameters declared → the wrapper would add nothing but the global
    // include the body already carries; compile the body directly.
    if params.is_empty() && samplers.is_empty() {
        return compile_body(&input, input.source, input.defines);
    }

    // --- pass 1: compile with PARAM_ALLOC_FIRST_PASS, reflect $Globals ---
    let mut first_defines = input.defines.clone();
    first_defines.define("PARAM_ALLOC_FIRST_PASS", "1");
    let first_bytecode = compile_body_flags(&input, input.source, &first_defines, ENGINE_FIRST_PASS)?;

    let resolved = reflect_user_params(input.compiler, &first_bytecode, &params, &samplers)?;

    // --- synthesise the wrapper and compile it for real ---
    let wrapper = synthesize_wrapper(&resolved.params, &resolved.samplers, input.source);
    compile_body(&input, wrapper.as_bytes(), input.defines)
}

/// Compile a source body with the final flags.
fn compile_body(input: &CompileInput, source: &[u8], defines: &MacroList) -> Result<Vec<u8>, String> {
    let engine = if input.debug { ENGINE_FINAL_DEBUG } else { ENGINE_FINAL };
    compile_body_flags(input, source, defines, engine)
}

fn compile_body_flags(
    input: &CompileInput,
    source: &[u8],
    defines: &MacroList,
    engine_flags: u32,
) -> Result<Vec<u8>, String> {
    let raw = defines.as_raw();
    let flags1 = translate_flags(engine_flags);
    unsafe {
        let (hr, code, errs) = input.compiler.compile(
            source,
            MEMORY,
            raw.as_ptr(),
            input.includer.as_ptr(),
            input.entry,
            input.target,
            flags1,
        );
        if !succeeded(hr) {
            // The engine treats "entrypoint not found" as a benign skip.
            let msg = ID3DBlob::to_string_lossy(errs);
            ID3DBlob::release(errs);
            ID3DBlob::release(code);
            if msg.contains("entrypoint not found") {
                return Err(EntryNotFound.to_string());
            }
            return Err(if msg.is_empty() {
                format!("D3DCompile failed (0x{hr:08x})")
            } else {
                format!("D3DCompile: {}", msg.trim())
            });
        }
        ID3DBlob::release(errs);
        let bytecode = ID3DBlob::to_vec(code);
        ID3DBlob::release(code);
        Ok(bytecode)
    }
}

/// Sentinel error string for a missing entry point (a valid outcome: not every
/// entry point exists for every shader).
struct EntryNotFound;
impl std::fmt::Display for EntryNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "entrypoint not found")
    }
}
/// True if `err` is the benign "entry point does not exist" outcome.
pub fn is_entry_not_found(err: &str) -> bool {
    err.contains("entrypoint not found")
}

// ---------------------------------------------------------------------------
// PREPROCESS token scan (spec §6c step 4)
// ---------------------------------------------------------------------------

fn scan_tokens(text: &[u8]) -> (Vec<UserParam>, Vec<UserSampler>) {
    let text = String::from_utf8_lossy(text);
    let mut params = Vec::new();
    let mut samplers = Vec::new();
    for token in split_markers(&text, "___PARAM___") {
        // ___PARAM___(type name [semantic]) — we only need the name; size/type
        // come from reflection. Keep the raw inner text for the name (last ident
        // before an optional `[`/`:`).
        if let Some(name) = last_identifier(&token) {
            params.push(UserParam {
                name,
                size: 0,
                offset: 0,
                svt: D3D_SVT_FLOAT,
            });
        }
    }
    for token in split_markers(&text, "___SAMPLER___") {
        // ___SAMPLER___(DIM name)
        let mut it = token.split_whitespace();
        let dim = it.next().unwrap_or("2D");
        let name = it.next().unwrap_or("").trim().to_string();
        if !name.is_empty() {
            samplers.push(UserSampler {
                name,
                dimension: sampler_dimension(dim),
            });
        }
    }
    (params, samplers)
}

/// Return the parenthesised body after each occurrence of `marker(`.
fn split_markers(text: &str, marker: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(pos) = text[search..].find(marker) {
        let start = search + pos + marker.len();
        // skip to the '('
        let bytes = text.as_bytes();
        let mut i = start;
        while i < bytes.len() && bytes[i] != b'(' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // match to the closing ')'
        let mut depth = 0i32;
        let mut j = i;
        while j < bytes.len() {
            match bytes[j] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if j < bytes.len() {
            out.push(text[i + 1..j].to_string());
        }
        search = j.max(start);
    }
    out
}

fn last_identifier(s: &str) -> Option<String> {
    // strip any [..] array suffix and : semantic, then take the final ident
    let head = s.split([':', '[']).next().unwrap_or(s);
    head.split_whitespace()
        .last()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string())
        .filter(|w| !w.is_empty())
}

fn sampler_dimension(dim: &str) -> i32 {
    match dim.trim() {
        "2D_ARRAY" => D3D_SRV_DIMENSION_TEXTURE2DARRAY,
        "3D" => D3D_SRV_DIMENSION_TEXTURE3D,
        "CUBE" => D3D_SRV_DIMENSION_TEXTURECUBE,
        // COMPARISON_2D / 2D_HALF / 2D all reflect as Texture2D
        _ => D3D_SRV_DIMENSION_TEXTURE2D,
    }
}

// ---------------------------------------------------------------------------
// First-pass reflection: read UserParameter_* / UserParameterTexture_* (spec §6c step 7)
// ---------------------------------------------------------------------------

struct ResolvedParams {
    params: Vec<UserParam>,
    samplers: Vec<UserSampler>,
}

fn reflect_user_params(
    compiler: &Compiler,
    bytecode: &[u8],
    scanned_params: &[UserParam],
    scanned_samplers: &[UserSampler],
) -> Result<ResolvedParams, String> {
    unsafe {
        let refl = compiler
            .reflect(bytecode)
            .map_err(|hr| format!("D3DReflect (first pass) failed (0x{hr:08x})"))?;
        let rvt = &*(*refl).vtbl;

        // $Globals members named UserParameter_* -> offset/size/type.
        let mut params: Vec<UserParam> = Vec::new();
        let globals = (rvt.GetConstantBufferByName)(refl, c"$Globals".as_ptr());
        if !globals.is_null() {
            let cvt = &*(*globals).vtbl;
            let mut bdesc: D3D11_SHADER_BUFFER_DESC = std::mem::zeroed();
            if succeeded((cvt.GetDesc)(globals, &mut bdesc)) {
                for vi in 0..bdesc.Variables {
                    let var = (cvt.GetVariableByIndex)(globals, vi);
                    if var.is_null() {
                        continue;
                    }
                    let vvt = &*(*var).vtbl;
                    let mut vdesc: D3D11_SHADER_VARIABLE_DESC = std::mem::zeroed();
                    if !succeeded((vvt.GetDesc)(var, &mut vdesc)) {
                        continue;
                    }
                    let full = cstr_to_string(vdesc.Name);
                    if let Some(name) = full.strip_prefix("UserParameter_") {
                        let ty = (vvt.GetType)(var);
                        let svt = if ty.is_null() {
                            D3D_SVT_FLOAT
                        } else {
                            let tvt = &*(*ty).vtbl;
                            let mut tdesc: D3D11_SHADER_TYPE_DESC = std::mem::zeroed();
                            if succeeded((tvt.GetDesc)(ty, &mut tdesc)) {
                                tdesc.Type
                            } else {
                                D3D_SVT_FLOAT
                            }
                        };
                        params.push(UserParam {
                            name: name.to_string(),
                            size: vdesc.Size,
                            offset: vdesc.StartOffset,
                            svt,
                        });
                    }
                }
            }
        }

        // Bound resources named UserParameterTexture_* -> dimension.
        let mut samplers: Vec<UserSampler> = Vec::new();
        let mut desc: D3D11_SHADER_DESC = std::mem::zeroed();
        if succeeded((rvt.GetDesc)(refl, &mut desc)) {
            for i in 0..desc.BoundResources {
                let mut bind: D3D11_SHADER_INPUT_BIND_DESC = std::mem::zeroed();
                if !succeeded((rvt.GetResourceBindingDesc)(refl, i, &mut bind)) {
                    continue;
                }
                if bind.Type != D3D_SIT_TEXTURE {
                    continue;
                }
                let full = cstr_to_string(bind.Name);
                if let Some(name) = full.strip_prefix("UserParameterTexture_") {
                    samplers.push(UserSampler {
                        name: name.to_string(),
                        dimension: bind.Dimension,
                    });
                }
            }
        }

        (rvt.Release)(refl);

        // Fall back to the PREPROCESS scan for anything reflection dropped
        // (e.g. a param the first-pass compile optimised away entirely).
        if params.is_empty() && !scanned_params.is_empty() {
            params = scanned_params.to_vec();
        }
        if samplers.is_empty() && !scanned_samplers.is_empty() {
            samplers = scanned_samplers.to_vec();
        }

        Ok(ResolvedParams { params, samplers })
    }
}

// ---------------------------------------------------------------------------
// Wrapper synthesis (spec §6c step 8)
// ---------------------------------------------------------------------------

fn synthesize_wrapper(params: &[UserParam], samplers: &[UserSampler], body: &[u8]) -> String {
    let mut out = String::new();

    // Per-parameter value defines: `#define ___name value`, where value is the
    // packed cbuffer member reference.  Two banks: the primary Parameters
    // cbuffer and the extern bank at register(b13).
    let mut cbuffer = String::from("cbuffer Parameters : register(b13)\n{\n");
    for p in params {
        let hlsl_type = svt_hlsl_type(p.svt, p.size);
        let packoffset = format!("packoffset(c{}.x)", p.offset / 16);
        cbuffer.push_str(&format!(
            "\t{hlsl_type} UserParameter_{} : {packoffset};\n",
            p.name
        ));
        out.push_str(&format!("#define ___{} UserParameter_{}\n", p.name, p.name));
    }
    cbuffer.push_str("};\n");

    // Texture + sampler declarations.
    let mut resources = String::new();
    for (i, s) in samplers.iter().enumerate() {
        let (tex_ty, samp_struct) = sampler_hlsl_types(s.dimension);
        resources.push_str(&format!(
            "SamplerState UserParameterSampler_{name} : register(s{i});\n\
             {tex_ty}<float4> UserParameterTexture_{name} : register(t{i});\n\
             static const {samp_struct} ___{name} = {{ UserParameterSampler_{name}, UserParameterTexture_{name} }};\n",
            name = s.name,
        ));
    }

    out.push_str("#include <global.fx>\n");
    out.push_str(&cbuffer);
    out.push_str(&resources);
    out.push_str(&String::from_utf8_lossy(body));
    out
}

fn svt_hlsl_type(svt: i32, size: u32) -> &'static str {
    // Approximate the storage type from reflection. Vectors are packed as
    // float4-family; the exact HLSL element type only affects declaration, not
    // the reflected register layout the constant table records.
    match svt {
        D3D_SVT_BOOL => "bool",
        D3D_SVT_INT => "int4",
        D3D_SVT_UINT => "uint4",
        _ => {
            let _ = size;
            "float4"
        }
    }
}

fn sampler_hlsl_types(dim: i32) -> (&'static str, &'static str) {
    match dim {
        D3D_SRV_DIMENSION_TEXTURE2DARRAY => ("Texture2DArray", "texture_sampler_2d_array"),
        D3D_SRV_DIMENSION_TEXTURE3D => ("Texture3D", "texture_sampler_3d"),
        D3D_SRV_DIMENSION_TEXTURECUBE => ("TextureCube", "texture_sampler_cube"),
        _ => ("Texture2D", "texture_sampler_2d"),
    }
}
