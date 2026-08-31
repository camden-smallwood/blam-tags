//! Reflection → rasterizer constant table, reproducing `sub_140066CAC`
//! ("create_constant_table") + `sub_140C55890` ("fill/pack"), spec §8b.
//!
//! The constant table is derived purely by reflecting the final bytecode, so a
//! compile matching the engine's inputs reproduces it exactly. The engine walks
//! `BoundResources` in order: a **texture** becomes a sampler-set entry (with a
//! `UserParameterTexture_`/`LocalTexture_`/`GlobalTexture_` prefix stripped and
//! its own D3D bind register); a **cbuffer**'s members are filtered
//! (`_pad`-suffix dropped, leading `___` stripped, only struct/bool/int/float
//! kept — UINT dropped) and packed `(cbuffer_index << 8) | (offset >> shift)`
//! with the index from [`super::cbuffer_index`]. Samplers (type 3) are skipped.
//!
//! Verified byte-exact against stock `add.pixel_shader`.

use super::cbuffer_index::cbuffer_index;
use super::d3d::*;
use super::entry::Stage;

/// One `rasterizer_constant_block` element.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstantEntry {
    pub name: String,
    pub register_start: i16,
    pub register_count: i8,
    /// 0=bool, 1=int, 2=float, 3=sampler/texture.
    pub register_set: i8,
}

/// One `global_rasterizer_constant_table_struct`.
#[derive(Clone, Debug, Default)]
pub struct ConstantTable {
    pub constants: Vec<ConstantEntry>,
    pub parameter_buffer_size: i32,
    pub extern_parameter_buffer_size: i32,
    /// 0=vertex, 1=pixel, 2=compute.
    pub table_type: i8,
}

// D3D_SHADER_VARIABLE_CLASS
const D3D_SVC_STRUCT: i32 = 5;

/// Strip the texture-declaration prefix the engine strips (case-insensitive,
/// in this order). Used for both `UserParameterTexture_*` material textures and
/// `LocalTexture_*`/`GlobalTexture_*` engine textures.
fn strip_texture_prefix(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    for p in ["userparametertexture_", "localtexture_", "globaltexture_"] {
        if lower.starts_with(p) {
            return name[p.len()..].to_string();
        }
    }
    name.to_string()
}

fn parameter_buffer_names(stage: Stage) -> (&'static str, &'static str) {
    match stage {
        Stage::Vertex => ("ParametersVS", "ParametersVSExterns"),
        Stage::Pixel => ("ParametersPS", "ParametersPSExterns"),
        Stage::Compute => ("ParametersCS", "ParametersCSExterns"),
    }
}

/// Reflect `bytecode` and build the constant table for `stage`.
pub fn reflect_constant_table(
    compiler: &Compiler,
    bytecode: &[u8],
    stage: Stage,
) -> Result<ConstantTable, String> {
    unsafe {
        let refl = compiler
            .reflect(bytecode)
            .map_err(|hr| format!("D3DReflect failed (0x{hr:08x})"))?;
        let result = reflect_inner(refl, stage);
        let vt = &*(*refl).vtbl;
        (vt.Release)(refl);
        result
    }
}

unsafe fn reflect_inner(
    refl: *mut ID3D11ShaderReflection,
    stage: Stage,
) -> Result<ConstantTable, String> {
    unsafe {
    let rvt = &*(*refl).vtbl;
    let debug = std::env::var_os("BLAM_SHADER_DEBUG").is_some();

    let mut desc: D3D11_SHADER_DESC = std::mem::zeroed();
    let hr = (rvt.GetDesc)(refl, &mut desc);
    if !succeeded(hr) {
        return Err(format!("ID3D11ShaderReflection::GetDesc failed (0x{hr:08x})"));
    }

    let (param_name, extern_name) = parameter_buffer_names(stage);
    let mut parameter_buffer_size: i32 = 0;
    let mut extern_parameter_buffer_size: i32 = 0;
    let mut constants: Vec<ConstantEntry> = Vec::new();

    // Walk bound resources in declaration order (this is the engine's order).
    for i in 0..desc.BoundResources {
        let mut bind: D3D11_SHADER_INPUT_BIND_DESC = std::mem::zeroed();
        if !succeeded((rvt.GetResourceBindingDesc)(refl, i, &mut bind)) {
            continue;
        }
        let res_name = cstr_to_string(bind.Name);
        match bind.Type {
            D3D_SIT_TEXTURE => {
                constants.push(ConstantEntry {
                    // Constant names are stored as string_ids, which the engine
                    // canonicalises to lower case.
                    name: strip_texture_prefix(&res_name).to_ascii_lowercase(),
                    register_start: bind.BindPoint as i16,
                    register_count: bind.BindCount as i8,
                    register_set: 3,
                });
            }
            D3D_SIT_CBUFFER => {
                let idx = match cbuffer_index(&res_name) {
                    Some(x) => x,
                    None => {
                        return Err(format!("constant buffer '{res_name}' not found in engine table"));
                    }
                };
                let cbuf = (rvt.GetConstantBufferByName)(refl, bind.Name);
                if cbuf.is_null() {
                    continue;
                }
                let cvt = &*(*cbuf).vtbl;
                let mut bdesc: D3D11_SHADER_BUFFER_DESC = std::mem::zeroed();
                if !succeeded((cvt.GetDesc)(cbuf, &mut bdesc)) {
                    continue;
                }
                if res_name == param_name {
                    parameter_buffer_size = bdesc.Size as u16 as i32;
                } else if res_name == extern_name {
                    extern_parameter_buffer_size = bdesc.Size as u16 as i32;
                }
                for vi in 0..bdesc.Variables {
                    let var = (cvt.GetVariableByIndex)(cbuf, vi);
                    if var.is_null() {
                        continue;
                    }
                    let vvt = &*(*var).vtbl;
                    let mut vdesc: D3D11_SHADER_VARIABLE_DESC = std::mem::zeroed();
                    if !succeeded((vvt.GetDesc)(var, &mut vdesc)) {
                        continue;
                    }
                    let raw_name = cstr_to_string(vdesc.Name);
                    // filter: drop names ending in "_pad"
                    if raw_name.len() > 4 && raw_name.ends_with("_pad") {
                        continue;
                    }
                    // strip a leading "___"; store lower-cased (string_id canonical form)
                    let name = raw_name
                        .strip_prefix("___")
                        .unwrap_or(&raw_name)
                        .to_ascii_lowercase();

                    // classify by type/class
                    let (class, svt) = type_of(vvt, var);
                    let set: i8 = if class == D3D_SVC_STRUCT {
                        2 // struct -> float bank
                    } else {
                        match svt {
                            D3D_SVT_BOOL => 0,
                            D3D_SVT_INT => 1,
                            D3D_SVT_FLOAT => 2,
                            _ => {
                                if debug {
                                    eprintln!("  drop {raw_name} (svt={svt})");
                                }
                                continue; // UINT, double, void, ... dropped
                            }
                        }
                    };
                    let shift: u32 = if set == 0 { 2 } else { 4 };
                    let register_start = ((idx << 8) | (vdesc.StartOffset >> shift)) as i16;
                    let register_count = ((vdesc.Size + (1 << shift) - 1) >> shift) as i8;
                    constants.push(ConstantEntry {
                        name,
                        register_start,
                        register_count,
                        register_set: set,
                    });
                }
            }
            // D3D_SIT_SAMPLER and others: skipped
            _ => {}
        }
    }

    Ok(ConstantTable {
        constants,
        parameter_buffer_size,
        extern_parameter_buffer_size,
        table_type: stage as i8,
    })
    }
}

/// Read a variable's `(Class, Type)` from reflection.
unsafe fn type_of(vvt: &ID3D11ShaderReflectionVariableVtbl, var: *mut ID3D11ShaderReflectionVariable) -> (i32, i32) {
    unsafe {
        let ty = (vvt.GetType)(var);
        if ty.is_null() {
            return (0, D3D_SVT_FLOAT);
        }
        let tvt = &*(*ty).vtbl;
        let mut tdesc: D3D11_SHADER_TYPE_DESC = std::mem::zeroed();
        if succeeded((tvt.GetDesc)(ty, &mut tdesc)) {
            (tdesc.Class, tdesc.Type)
        } else {
            (0, D3D_SVT_FLOAT)
        }
    }
}
