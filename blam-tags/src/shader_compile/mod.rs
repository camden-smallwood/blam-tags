//! Rebuild the rasterizer's compiled-shader tags from HLSL, matching what the
//! H3EK / MCC `tool.exe` produces.
//!
//! This is a Rust port of the shader-compile pipeline reverse-engineered in the
//! h3lm project (`docs/SHADER_COMPILE.md`). `tool.exe` compiles in-process with
//! `D3DCompile` from `d3dcompiler_47.dll` at Shader Model 5, reflects the result
//! to build a constant table, and stores both into `pixel_shader` /
//! `vertex_shader` / `compute_shader` (and the `global_*_shader`) tags. We do the
//! same, loading the same system `d3dcompiler_47.dll` so the PC bytecode matches.
//!
//! Windows only (it needs `d3dcompiler_47.dll`); gated behind the
//! `shader-compile` feature.
//!
//! ```no_run
//! # #[cfg(all(feature = "shader-compile", windows))] {
//! use blam_tags::shader_compile::{ShaderCompiler, include::DiskSource};
//! use blam_tags::shader_compile::entry::Stage;
//! use blam_tags::shader_compile::macros::Platform;
//!
//! let src = DiskSource::new(r"D:\...\H3EK\source\rasterizer\hlsl");
//! let sc = ShaderCompiler::load(&src, None).unwrap();
//! // compile add's `default_ps` for the `screen` vertex type on PC
//! let out = sc.compile_variant("add", Stage::Pixel, 0, 7, 0, Platform::Pc, &[]).unwrap();
//! println!("{} bytes, {} constants", out.bytecode.len(), out.table.constants.len());
//! # }
//! ```

pub mod cbuffer_index;
pub mod d3d;
pub mod emit;
pub mod entry;
pub mod include;
pub mod macros;
pub mod param_alloc;
pub mod raw;
pub mod reflect;

use d3d::Compiler;
use emit::PlatformOutput;
use entry::Stage;
use include::{Includer, SourceProvider};
use macros::{append_base_macros, BaseMacroFlags, MacroList, Platform};
use param_alloc::{compile_shader, is_entry_not_found, CompileInput};
use reflect::reflect_constant_table;
use std::ffi::CString;

/// A loaded compiler bound to a source provider.
pub struct ShaderCompiler<'a> {
    compiler: Compiler,
    provider: &'a dyn SourceProvider,
    /// Emit debug info (PDB path + strip). Off by default.
    pub debug: bool,
}

/// Outcome of a single-variant compile.
pub enum CompileOutcome {
    /// Bytecode + reflected constant table.
    Compiled(PlatformOutput),
    /// The entry point does not exist for this shader — a valid, non-fatal
    /// result (the engine skips it).
    EntryNotFound,
}

impl<'a> ShaderCompiler<'a> {
    /// Load `d3dcompiler_47.dll` (system copy, or `dll_path` if given) and bind
    /// it to `provider`.
    pub fn load(provider: &'a dyn SourceProvider, dll_path: Option<&str>) -> Result<Self, String> {
        Ok(ShaderCompiler {
            compiler: Compiler::load(dll_path)?,
            provider,
            debug: false,
        })
    }

    /// Compile one `(stage, entry, vertex_type, pass)` variant for one platform,
    /// with optional extra `#define`s (the category/option macros for a
    /// render_method, prepended before the base macros).
    #[allow(clippy::too_many_arguments)]
    pub fn compile_variant(
        &self,
        base_name: &str,
        stage: Stage,
        entry: usize,
        vertex_type: usize,
        pass: usize,
        platform: Platform,
        extra_defines: &[(&str, &str)],
    ) -> Result<CompileOutcome, String> {
        let source = self
            .provider
            .main_source(base_name)
            .ok_or_else(|| format!("no source for shader '{base_name}'"))?;

        let entry_name = stage
            .entry_name(entry)
            .ok_or_else(|| format!("entry ordinal {entry} out of range"))?;
        // Vertex pass suffix: `_pass%d` for VS pass > 0 (spec §5).
        let entry_string = if stage == Stage::Vertex && pass > 0 {
            format!("{entry_name}_pass{pass}")
        } else {
            entry_name.to_string()
        };
        let entry_c = CString::new(entry_string).map_err(|_| "bad entry name".to_string())?;
        let target_c = CString::new(stage.profile()).unwrap();

        // Build the macro list: category/option macros first, then base macros.
        let mut defines = MacroList::new();
        for (n, v) in extra_defines {
            defines.define(n, v);
        }
        append_base_macros(&mut defines, entry, vertex_type, stage, platform, BaseMacroFlags::default());

        let includer = Includer::new(self.provider);
        let result = compile_shader(CompileInput {
            compiler: &self.compiler,
            source: &source,
            entry: &entry_c,
            target: &target_c,
            defines: &defines,
            includer: &includer,
            stage,
            debug: self.debug,
        });

        let bytecode = match result {
            Ok(b) => b,
            Err(e) if is_entry_not_found(&e) => return Ok(CompileOutcome::EntryNotFound),
            Err(e) => {
                let missing = includer.take_missing();
                if !missing.is_empty() {
                    return Err(format!("{e}  (unresolved includes: {})", missing.join(", ")));
                }
                return Err(e);
            }
        };

        let table = reflect_constant_table(&self.compiler, &bytecode, stage)?;
        Ok(CompileOutcome::Compiled(PlatformOutput { bytecode, table }))
    }
}

/// A [`crate::convert::ShaderRecompiler`] backed by the raw-`.hlsl` path. Wire
/// this into a byte-order conversion so a `pixel_shader` / `vertex_shader` /
/// `compute_shader` is regenerated from the kit's HLSL instead of being refused.
///
/// `base_name` is the shader source name (the leaf of the tag name — the caller,
/// which knows the tag's path, supplies it). Groups the raw path does not cover
/// (`global_*_shader`, `render_method_template`) and names with no matching HLSL
/// source are declined, so the conversion falls back to its refusal for those.
pub struct RawShaderRecompiler<'a> {
    pub provider: &'a dyn SourceProvider,
    pub dll_path: Option<String>,
    pub base_name: String,
}

impl crate::convert::ShaderRecompiler for RawShaderRecompiler<'_> {
    fn recompile(&self, target: &mut crate::TagFile, group_name: &str) -> Result<bool, String> {
        let stage = match group_name {
            "pixel_shader" => Stage::Pixel,
            "vertex_shader" => Stage::Vertex,
            "compute_shader" => Stage::Compute,
            _ => return Ok(false),
        };
        if self.provider.main_source(&self.base_name).is_none() {
            return Ok(false);
        }
        recompile_raw_into(target, self.provider, self.dll_path.as_deref(), &self.base_name, stage)?;
        Ok(true)
    }
}

/// Recompile a raw `.hlsl` shader (`tool.exe`'s `shaders` verb) into `target`,
/// which should be a clone of a stock tag of the right group (so its embedded
/// `blay` layout and `version`/`entry_points` fields are the kit's). Used by the
/// conversion path to regenerate a compiled-shader tag from the kit's own HLSL
/// instead of carrying incompatible bytecode across.
///
/// `base_name` is the shader source name (the leaf of the tag name, e.g. `add`
/// for `rasterizer\shaders\add`). `stage` is the group being written.
pub fn recompile_raw_into(
    target: &mut crate::TagFile,
    provider: &dyn SourceProvider,
    dll_path: Option<&str>,
    base_name: &str,
    stage: Stage,
) -> Result<(), String> {
    use crate::TagFieldData;

    let source = provider
        .main_source(base_name)
        .ok_or_else(|| format!("no HLSL source for shader '{base_name}'"))?;
    let directives = raw::parse_directives(&source);

    let sc = ShaderCompiler::load(provider, dll_path)?;
    let compiled = raw::compile_raw(&sc, base_name, &directives, Platform::Pc)?;
    if compiled.is_empty() {
        return Err(format!("shader '{base_name}' produced no {stage:?} variants"));
    }

    // Preserve the kit template's version and entry_points flag word.
    let (version, flags) = {
        let root = target.root();
        let version = root
            .field_path("version")
            .and_then(|f| f.value())
            .and_then(|v| match v {
                TagFieldData::LongInteger(x) => Some(x),
                _ => None,
            })
            .unwrap_or(0);
        let flags = root
            .field_path("entry_points")
            .and_then(|f| f.value())
            .and_then(|v| match v {
                TagFieldData::LongFlags { value, .. } => Some(value),
                _ => None,
            })
            .unwrap_or(0);
        (version, flags)
    };

    match stage {
        Stage::Pixel => emit::emit_flat(target, &compiled.pixel, version, flags)?,
        Stage::Compute => emit::emit_flat(target, &compiled.compute, version, flags)?,
        Stage::Vertex => emit::emit_vertex(target, &compiled.vertex, version, flags)?,
    }
    Ok(())
}
