//! Source resolution and the `ID3DInclude` the compiler hands to `D3DCompile`.
//!
//! `tool.exe` reads shader source from `hlsl`-group tags and resolves
//! `#include <a.b>` to the tag `shaders\a_b` (spec §3a/§3b). We abstract that
//! behind [`SourceProvider`] so a caller can instead compile straight from a
//! source tree (`source\rasterizer\hlsl\*.fx`), whose files are byte-identical
//! to those tag payloads.

use super::d3d::{HRESULT, S_OK};
use std::cell::RefCell;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::path::PathBuf;

/// Supplies HLSL source text: the main shader body and each `#include`.
pub trait SourceProvider {
    /// The main source for a shader base name. For the raw-`.hlsl` path this is
    /// the file/tag `<base>`; for the render-method path the engine looks up
    /// `<base>_hlsl` then `<base>_fx`.
    fn main_source(&self, base: &str) -> Option<Vec<u8>>;

    /// Resolve an `#include "name"` / `#include <name>` to its bytes. `name` is
    /// the literal include string (e.g. `global.fx`).
    fn include(&self, name: &str) -> Option<Vec<u8>>;
}

/// Reads from a source tree on disk (the kit's `source\rasterizer\hlsl`). The
/// include name is used as a path relative to `root`, and the main base name is
/// tried as `<base>.fx` then `<base>.hlsl`.
pub struct DiskSource {
    pub root: PathBuf,
}

impl DiskSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        // Include/source names use Windows separators inside the tag world; on
        // disk they are the same tree, so normalise `\` to the OS separator.
        let rel = rel.replace('\\', "/");
        std::fs::read(self.root.join(rel)).ok()
    }
}

impl SourceProvider for DiskSource {
    fn main_source(&self, base: &str) -> Option<Vec<u8>> {
        // A render-method base arrives as `shaders\<name>`; on disk the source
        // tree is flat, so use the leaf.
        let leaf = base.rsplit(['\\', '/']).next().unwrap_or(base);
        self.read(&format!("{leaf}.fx"))
            .or_else(|| self.read(&format!("{leaf}.hlsl")))
    }
    fn include(&self, name: &str) -> Option<Vec<u8>> {
        self.read(name)
    }
}

// ---------------------------------------------------------------------------
// ID3DInclude COM shim
// ---------------------------------------------------------------------------

#[repr(C)]
struct ID3DIncludeVtbl {
    open: unsafe extern "system" fn(
        this: *mut c_void,
        include_type: i32,
        file_name: *const c_char,
        parent_data: *const c_void,
        pp_data: *mut *const c_void,
        p_bytes: *mut u32,
    ) -> HRESULT,
    close: unsafe extern "system" fn(this: *mut c_void, data: *const c_void) -> HRESULT,
}

static INCLUDE_VTBL: ID3DIncludeVtbl = ID3DIncludeVtbl {
    open: includer_open,
    close: includer_close,
};

/// An `ID3DInclude` implementation backed by a [`SourceProvider`]. The first
/// field is the v-table pointer, so `&mut Includer as *mut _` is a valid
/// `ID3DInclude*`.
#[repr(C)]
pub struct Includer<'a> {
    vtbl: *const ID3DIncludeVtbl,
    provider: &'a dyn SourceProvider,
    /// Buffers handed back to the compiler, kept alive until the `Includer` is
    /// dropped (the engine's include text is tag-owned and equally long-lived;
    /// `Close` is a no-op).
    live: RefCell<Vec<Box<[u8]>>>,
    /// Names that failed to resolve, for a useful error after the compile.
    missing: RefCell<Vec<String>>,
}

impl<'a> Includer<'a> {
    pub fn new(provider: &'a dyn SourceProvider) -> Self {
        Includer {
            vtbl: &INCLUDE_VTBL,
            provider,
            live: RefCell::new(Vec::new()),
            missing: RefCell::new(Vec::new()),
        }
    }

    /// A pointer usable as the `pInclude` argument to `D3DCompile`. The compiler
    /// calls back into `Open`/`Close`, which mutate only through the interior
    /// `RefCell`s, so a shared borrow is sound.
    pub fn as_ptr(&self) -> *mut c_void {
        self as *const Includer as *mut c_void
    }

    pub fn take_missing(&self) -> Vec<String> {
        std::mem::take(&mut *self.missing.borrow_mut())
    }
}

unsafe extern "system" fn includer_open(
    this: *mut c_void,
    _include_type: i32,
    file_name: *const c_char,
    _parent_data: *const c_void,
    pp_data: *mut *const c_void,
    p_bytes: *mut u32,
) -> HRESULT {
    unsafe {
        let me = &*(this as *const Includer);
        let name = if file_name.is_null() {
            String::new()
        } else {
            CStr::from_ptr(file_name).to_string_lossy().into_owned()
        };
        match me.provider.include(&name) {
            Some(bytes) => {
                let boxed = bytes.into_boxed_slice();
                let ptr = boxed.as_ptr() as *const c_void;
                let len = boxed.len() as u32;
                me.live.borrow_mut().push(boxed);
                *pp_data = ptr;
                *p_bytes = len;
                S_OK
            }
            None => {
                me.missing.borrow_mut().push(name);
                *pp_data = std::ptr::null();
                *p_bytes = 0;
                -0x7fff_bffb // E_FAIL
            }
        }
    }
}

unsafe extern "system" fn includer_close(_this: *mut c_void, _data: *const c_void) -> HRESULT {
    // The buffer stays alive in `live` until the Includer drops; nothing to free.
    S_OK
}
