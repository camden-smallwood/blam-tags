//! Raw FFI to `d3dcompiler_47.dll` — the same compiler `tool.exe` loads.
//!
//! We resolve the entry points by hand with `LoadLibraryW` + `GetProcAddress`
//! (exactly as the engine does in `sub_140067194`) rather than link an import
//! library, so the caller can point at a specific `d3dcompiler_47.dll` and so
//! the crate carries no build-time dependency on the DirectX SDK. The COM
//! interfaces the compiler hands back (`ID3DBlob`, `ID3D11ShaderReflection` and
//! its sub-interfaces) are called through their documented v-table layouts.
//!
//! Contract reference: h3lm `docs/SHADER_COMPILE.md` §6 (backend) and §8b
//! (reflection → constant table).

#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]

use std::ffi::{c_void, CStr};
use std::os::raw::c_char;

// ---------------------------------------------------------------------------
// Win32 loader (kernel32 is always linked on the windows target)
// ---------------------------------------------------------------------------

type HMODULE = *mut c_void;
type FARPROC = *const c_void;

unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> HMODULE;
    fn GetProcAddress(module: HMODULE, name: *const c_char) -> FARPROC;
    fn FreeLibrary(module: HMODULE) -> i32;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------
// HRESULT + D3D plain-C types
// ---------------------------------------------------------------------------

pub type HRESULT = i32;
pub const S_OK: HRESULT = 0;
pub fn succeeded(hr: HRESULT) -> bool {
    hr >= 0
}

/// `D3D_SHADER_MACRO { LPCSTR Name; LPCSTR Definition; }`. The array handed to
/// `D3DCompile`/`D3DPreprocess` is terminated by an all-null entry.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D_SHADER_MACRO {
    pub Name: *const c_char,
    pub Definition: *const c_char,
}

// D3DCOMPILE_* flags (winapi values). The engine's flag word is decoded in
// `compile.rs`; these are the D3D constants it decodes to.
pub const D3DCOMPILE_DEBUG: u32 = 1 << 0;
pub const D3DCOMPILE_SKIP_VALIDATION: u32 = 1 << 1;
pub const D3DCOMPILE_SKIP_OPTIMIZATION: u32 = 1 << 2;
pub const D3DCOMPILE_AVOID_FLOW_CONTROL: u32 = 1 << 9;

// D3D_INCLUDE_TYPE
pub const D3D_INCLUDE_LOCAL: i32 = 0;
pub const D3D_INCLUDE_SYSTEM: i32 = 1;

// D3D_SHADER_INPUT_TYPE
pub const D3D_SIT_CBUFFER: i32 = 0;
pub const D3D_SIT_TEXTURE: i32 = 2;
pub const D3D_SIT_SAMPLER: i32 = 3;

// D3D_SHADER_VARIABLE_TYPE (subset we key on)
pub const D3D_SVT_BOOL: i32 = 1;
pub const D3D_SVT_INT: i32 = 2;
pub const D3D_SVT_FLOAT: i32 = 3;
pub const D3D_SVT_UINT: i32 = 19;

// D3D_SRV_DIMENSION (subset — matches spec §6c texture mapping)
pub const D3D_SRV_DIMENSION_TEXTURE2D: i32 = 4;
pub const D3D_SRV_DIMENSION_TEXTURE2DARRAY: i32 = 5;
pub const D3D_SRV_DIMENSION_TEXTURE3D: i32 = 8;
pub const D3D_SRV_DIMENSION_TEXTURECUBE: i32 = 9;

// ---------------------------------------------------------------------------
// D3D reflection descriptor structs. These are written into by the reflection
// interface, so each MUST be at least as large as the real header struct — we
// define them in full.
// ---------------------------------------------------------------------------

#[repr(C)]

pub struct D3D11_SHADER_DESC {
    pub Version: u32,
    pub Creator: *const c_char,
    pub Flags: u32,
    pub ConstantBuffers: u32,
    pub BoundResources: u32,
    pub InputParameters: u32,
    pub OutputParameters: u32,
    pub InstructionCount: u32,
    pub TempRegisterCount: u32,
    pub TempArrayCount: u32,
    pub DefCount: u32,
    pub DclCount: u32,
    pub TextureNormalInstructions: u32,
    pub TextureLoadInstructions: u32,
    pub TextureCompInstructions: u32,
    pub TextureBiasInstructions: u32,
    pub TextureGradientInstructions: u32,
    pub FloatInstructionCount: u32,
    pub IntInstructionCount: u32,
    pub UintInstructionCount: u32,
    pub StaticFlowControlCount: u32,
    pub DynamicFlowControlCount: u32,
    pub MacroInstructionCount: u32,
    pub ArrayInstructionCount: u32,
    pub CutInstructionCount: u32,
    pub EmitInstructionCount: u32,
    pub GSOutputTopology: i32,
    pub GSMaxOutputVertexCount: u32,
    pub InputPrimitive: i32,
    pub PatchConstantParameters: u32,
    pub cGSInstanceCount: u32,
    pub cControlPoints: u32,
    pub HSOutputPrimitive: i32,
    pub HSPartitioning: i32,
    pub TessellatorDomain: i32,
    pub cBarrierInstructions: u32,
    pub cInterlockedInstructions: u32,
    pub cTextureStoreInstructions: u32,
}

impl D3D11_SHADER_DESC {
    /// `HIWORD(Version)` — the program type: 0=PS, 1=VS, 5=CS (spec §6c).
    pub fn program_type(&self) -> u32 {
        (self.Version >> 16) & 0xffff
    }
}

#[repr(C)]

pub struct D3D11_SHADER_BUFFER_DESC {
    pub Name: *const c_char,
    pub Type: i32,
    pub Variables: u32,
    pub Size: u32,
    pub uFlags: u32,
}

#[repr(C)]

pub struct D3D11_SHADER_VARIABLE_DESC {
    pub Name: *const c_char,
    pub StartOffset: u32,
    pub Size: u32,
    pub uFlags: u32,
    pub DefaultValue: *mut c_void,
    pub StartTexture: u32,
    pub TextureSize: u32,
    pub StartSampler: u32,
    pub SamplerSize: u32,
}

#[repr(C)]

pub struct D3D11_SHADER_TYPE_DESC {
    pub Class: i32,
    pub Type: i32,
    pub Rows: u32,
    pub Columns: u32,
    pub Elements: u32,
    pub Members: u32,
    pub Offset: u32,
    pub Name: *const c_char,
}

#[repr(C)]

pub struct D3D11_SHADER_INPUT_BIND_DESC {
    pub Name: *const c_char,
    pub Type: i32,
    pub BindPoint: u32,
    pub BindCount: u32,
    pub uFlags: u32,
    pub ReturnType: i32,
    pub Dimension: i32,
    pub NumSamples: u32,
}

// These descriptor structs are plain-old-data (integers + pointers); a caller
// builds one with `unsafe { std::mem::zeroed() }` before a `GetDesc` call, which
// is a valid all-zero state (null pointers, zero counts).

// ---------------------------------------------------------------------------
// COM v-tables. Only the slots we call are typed; the rest are opaque pointers
// so the layout offset of the ones we use is correct.
// ---------------------------------------------------------------------------

type PFN = *const c_void;

#[repr(C)]
pub struct ID3DBlobVtbl {
    pub QueryInterface: PFN,
    pub AddRef: PFN,
    pub Release: unsafe extern "system" fn(this: *mut ID3DBlob) -> u32,
    pub GetBufferPointer: unsafe extern "system" fn(this: *mut ID3DBlob) -> *mut c_void,
    pub GetBufferSize: unsafe extern "system" fn(this: *mut ID3DBlob) -> usize,
}
#[repr(C)]
pub struct ID3DBlob {
    pub vtbl: *const ID3DBlobVtbl,
}

impl ID3DBlob {
    /// Copy the blob's bytes out into an owned `Vec`.
    pub unsafe fn to_vec(this: *mut ID3DBlob) -> Vec<u8> { unsafe {
        if this.is_null() {
            return Vec::new();
        }
        let vt = &*(*this).vtbl;
        let ptr = (vt.GetBufferPointer)(this) as *const u8;
        let len = (vt.GetBufferSize)(this);
        if ptr.is_null() || len == 0 {
            return Vec::new();
        }
        std::slice::from_raw_parts(ptr, len).to_vec()
    }}

    /// Interpret the blob as a NUL-terminated ASCII string (compiler errors).
    pub unsafe fn to_string_lossy(this: *mut ID3DBlob) -> String { unsafe {
        if this.is_null() {
            return String::new();
        }
        let vt = &*(*this).vtbl;
        let ptr = (vt.GetBufferPointer)(this) as *const c_char;
        if ptr.is_null() {
            return String::new();
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }}

    pub unsafe fn release(this: *mut ID3DBlob) { unsafe {
        if !this.is_null() {
            let vt = &*(*this).vtbl;
            (vt.Release)(this);
        }
    }}
}

#[repr(C)]
pub struct ID3D11ShaderReflectionVtbl {
    pub QueryInterface: PFN,
    pub AddRef: PFN,
    pub Release: unsafe extern "system" fn(this: *mut ID3D11ShaderReflection) -> u32,
    pub GetDesc:
        unsafe extern "system" fn(this: *mut ID3D11ShaderReflection, p: *mut D3D11_SHADER_DESC) -> HRESULT,
    pub GetConstantBufferByIndex: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflection,
        index: u32,
    ) -> *mut ID3D11ShaderReflectionConstantBuffer,
    pub GetConstantBufferByName: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflection,
        name: *const c_char,
    ) -> *mut ID3D11ShaderReflectionConstantBuffer,
    pub GetResourceBindingDesc: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflection,
        index: u32,
        p: *mut D3D11_SHADER_INPUT_BIND_DESC,
    ) -> HRESULT,
    // remaining slots unused
    pub GetInputParameterDesc: PFN,
    pub GetOutputParameterDesc: PFN,
    pub GetPatchConstantParameterDesc: PFN,
    pub GetVariableByName: PFN,
    pub GetResourceBindingDescByName: PFN,
}
#[repr(C)]
pub struct ID3D11ShaderReflection {
    pub vtbl: *const ID3D11ShaderReflectionVtbl,
}

#[repr(C)]
pub struct ID3D11ShaderReflectionConstantBufferVtbl {
    pub GetDesc: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflectionConstantBuffer,
        p: *mut D3D11_SHADER_BUFFER_DESC,
    ) -> HRESULT,
    pub GetVariableByIndex: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflectionConstantBuffer,
        index: u32,
    ) -> *mut ID3D11ShaderReflectionVariable,
    pub GetVariableByName: PFN,
}
#[repr(C)]
pub struct ID3D11ShaderReflectionConstantBuffer {
    pub vtbl: *const ID3D11ShaderReflectionConstantBufferVtbl,
}

#[repr(C)]
pub struct ID3D11ShaderReflectionVariableVtbl {
    pub GetDesc: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflectionVariable,
        p: *mut D3D11_SHADER_VARIABLE_DESC,
    ) -> HRESULT,
    pub GetType:
        unsafe extern "system" fn(this: *mut ID3D11ShaderReflectionVariable) -> *mut ID3D11ShaderReflectionType,
    pub GetBuffer: PFN,
    pub GetInterfaceSlot: PFN,
}
#[repr(C)]
pub struct ID3D11ShaderReflectionVariable {
    pub vtbl: *const ID3D11ShaderReflectionVariableVtbl,
}

#[repr(C)]
pub struct ID3D11ShaderReflectionTypeVtbl {
    pub GetDesc: unsafe extern "system" fn(
        this: *mut ID3D11ShaderReflectionType,
        p: *mut D3D11_SHADER_TYPE_DESC,
    ) -> HRESULT,
    // rest unused
}
#[repr(C)]
pub struct ID3D11ShaderReflectionType {
    pub vtbl: *const ID3D11ShaderReflectionTypeVtbl,
}

/// `IID_ID3D11ShaderReflection` = 8d536ca1-0cca-4956-a837-786963755584.
#[repr(C)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}
pub static IID_ID3D11_SHADER_REFLECTION: GUID = GUID {
    data1: 0x8d53_6ca1,
    data2: 0x0cca,
    data3: 0x4956,
    data4: [0xa8, 0x37, 0x78, 0x69, 0x63, 0x75, 0x55, 0x84],
};

// ---------------------------------------------------------------------------
// Entry-point function-pointer types
// ---------------------------------------------------------------------------

type PFN_D3DCompile = unsafe extern "system" fn(
    pSrcData: *const c_void,
    SrcDataSize: usize,
    pSourceName: *const c_char,
    pDefines: *const D3D_SHADER_MACRO,
    pInclude: *mut c_void,
    pEntrypoint: *const c_char,
    pTarget: *const c_char,
    Flags1: u32,
    Flags2: u32,
    ppCode: *mut *mut ID3DBlob,
    ppErrorMsgs: *mut *mut ID3DBlob,
) -> HRESULT;

type PFN_D3DPreprocess = unsafe extern "system" fn(
    pSrcData: *const c_void,
    SrcDataSize: usize,
    pSourceName: *const c_char,
    pDefines: *const D3D_SHADER_MACRO,
    pInclude: *mut c_void,
    ppCodeText: *mut *mut ID3DBlob,
    ppErrorMsgs: *mut *mut ID3DBlob,
) -> HRESULT;

type PFN_D3DReflect = unsafe extern "system" fn(
    pSrcData: *const c_void,
    SrcDataSize: usize,
    pInterface: *const GUID,
    ppReflector: *mut *mut c_void,
) -> HRESULT;

/// A loaded `d3dcompiler_47.dll` with the entry points resolved.
pub struct Compiler {
    module: HMODULE,
    d3d_compile: PFN_D3DCompile,
    d3d_preprocess: PFN_D3DPreprocess,
    d3d_reflect: PFN_D3DReflect,
}

// The DLL is stateless across our calls; sharing the resolved pointers is safe.
unsafe impl Send for Compiler {}
unsafe impl Sync for Compiler {}

impl Compiler {
    /// Load `d3dcompiler_47.dll` by name (resolved via the normal DLL search
    /// path — the system copy, the same one `tool.exe` binds on PC), or from an
    /// explicit path when one is given.
    pub fn load(explicit_path: Option<&str>) -> Result<Self, String> {
        let name = explicit_path.unwrap_or("d3dcompiler_47.dll");
        unsafe {
            let module = LoadLibraryW(wide(name).as_ptr());
            if module.is_null() {
                return Err(format!("could not load {name}"));
            }
            let get = |sym: &str| -> Result<FARPROC, String> {
                let c = std::ffi::CString::new(sym).unwrap();
                let p = GetProcAddress(module, c.as_ptr());
                if p.is_null() {
                    Err(format!("{name} is missing export {sym}"))
                } else {
                    Ok(p)
                }
            };
            let d3d_compile = std::mem::transmute::<FARPROC, PFN_D3DCompile>(get("D3DCompile")?);
            let d3d_preprocess =
                std::mem::transmute::<FARPROC, PFN_D3DPreprocess>(get("D3DPreprocess")?);
            let d3d_reflect = std::mem::transmute::<FARPROC, PFN_D3DReflect>(get("D3DReflect")?);
            Ok(Compiler {
                module,
                d3d_compile,
                d3d_preprocess,
                d3d_reflect,
            })
        }
    }

    /// Raw `D3DCompile`. `include` is an optional pointer to an `ID3DInclude`
    /// (our `include::TagIncluder` exposes one). Returns (code, errors); either
    /// may be null.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn compile(
        &self,
        src: &[u8],
        source_name: &CStr,
        defines: *const D3D_SHADER_MACRO,
        include: *mut c_void,
        entry: &CStr,
        target: &CStr,
        flags1: u32,
    ) -> (HRESULT, *mut ID3DBlob, *mut ID3DBlob) { unsafe {
        let mut code: *mut ID3DBlob = std::ptr::null_mut();
        let mut errs: *mut ID3DBlob = std::ptr::null_mut();
        let hr = (self.d3d_compile)(
            src.as_ptr() as *const c_void,
            src.len(),
            source_name.as_ptr(),
            defines,
            include,
            entry.as_ptr(),
            target.as_ptr(),
            flags1,
            0,
            &mut code,
            &mut errs,
        );
        (hr, code, errs)
    }}

    /// Raw `D3DPreprocess`. Returns (text, errors).
    pub unsafe fn preprocess(
        &self,
        src: &[u8],
        source_name: &CStr,
        defines: *const D3D_SHADER_MACRO,
        include: *mut c_void,
    ) -> (HRESULT, *mut ID3DBlob, *mut ID3DBlob) { unsafe {
        let mut text: *mut ID3DBlob = std::ptr::null_mut();
        let mut errs: *mut ID3DBlob = std::ptr::null_mut();
        let hr = (self.d3d_preprocess)(
            src.as_ptr() as *const c_void,
            src.len(),
            source_name.as_ptr(),
            defines,
            include,
            &mut text,
            &mut errs,
        );
        (hr, text, errs)
    }}

    /// Raw `D3DReflect` for `ID3D11ShaderReflection`. Caller must `Release` the
    /// returned interface.
    pub unsafe fn reflect(&self, bytecode: &[u8]) -> Result<*mut ID3D11ShaderReflection, HRESULT> { unsafe {
        let mut refl: *mut c_void = std::ptr::null_mut();
        let hr = (self.d3d_reflect)(
            bytecode.as_ptr() as *const c_void,
            bytecode.len(),
            &IID_ID3D11_SHADER_REFLECTION,
            &mut refl,
        );
        if succeeded(hr) && !refl.is_null() {
            Ok(refl as *mut ID3D11ShaderReflection)
        } else {
            Err(hr)
        }
    }}
}

impl Drop for Compiler {
    fn drop(&mut self) {
        unsafe {
            if !self.module.is_null() {
                FreeLibrary(self.module);
            }
        }
    }
}

/// Read a `*const c_char` the reflection API handed back into an owned String.
pub unsafe fn cstr_to_string(p: *const c_char) -> String { unsafe {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}}
