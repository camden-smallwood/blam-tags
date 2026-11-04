//! `c_material::postprocess` and the per-parameter
//! `s_material_postprocess_definition::UpdateRelatedPostprocessProperties`,
//! as MCC's tag_test runs them at load (0x140881FD0 and 0x140881B90).

use super::submit::color_degamma;
use super::{
    Material, MaterialFunctionParameter, MaterialFunctionType, MaterialParameter, MaterialParameterType, MaterialShader,
    shader_flags,
};
use crate::render_method::DEFAULT_BITMAP;

/// What the bake needs to know of the kit's bitmaps.
pub trait BitmapLookup {
    /// `Some(is_cube)` when a `bitmap` tag exists at `path` (no extension);
    /// a cube is a bitmap of type 2 (`sub_140881150`).
    fn find(&self, path: &str) -> Option<bool>;
}

impl<F: Fn(&str) -> Option<bool>> BitmapLookup for F {
    fn find(&self, path: &str) -> Option<bool> {
        self(path)
    }
}

/// One `textures` element of the postprocess.
#[derive(Debug, Clone, PartialEq)]
pub struct BakedTexture {
    pub parameter: String,
    pub bitmap: String,
    /// The register: a pixel sampler 0–11, or a vertex sampler.
    pub sampler: u8,
    pub vertex: bool,
    /// Byte 19's top three bits: `bitmap extern mode`, kept only under
    /// bitmap flag 0x10.
    pub extern_mode: u8,
    /// Raw address modes (0 wrap, 1 clamp, 2 mirror, 3 black border, 4
    /// mirror once, 5 mirror once border).
    pub address_u: u8,
    pub address_v: u8,
    /// Raw filter mode (`render_method_bitmap_filter_mode_enum`).
    pub filter: u8,
    /// LOD bias mode: the bias is `2 - 0.25 * mode` (X360 0x83AA2BA0).
    pub mip_bias_mode: u8,
    /// Index into [`BakedMaterial::functions`] of a frame-index function.
    pub frame_index_function: Option<usize>,
    /// -1 when the material sets none.
    pub min_mip: i8,
    pub max_mip: i8,
    pub phase_mask: u8,
}

/// A dynamic function, re-run per draw: `functions` plus its
/// `function parameters` record.
#[derive(Debug, Clone)]
pub struct BakedFunction {
    pub parameter: String,
    pub parameter_kind: MaterialParameterType,
    pub function: MaterialFunctionParameter,
    pub phase_mask: u8,
    /// The shader parameter's register, as is: a constant register for
    /// reals and colours, a sampler for bitmaps.
    pub register_index: u8,
    pub register_offset: u8,
    pub vertex: bool,
}

/// One group of boolean constants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BakedBool {
    pub values: u32,
    pub used: u32,
    pub phase_mask: u32,
}

/// `layerBlendModeEnum`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LayerBlendMode {
    #[default]
    None,
    Blended,
    Layered,
}

/// What `c_material::postprocess` builds.
#[derive(Debug, Clone, Default)]
pub struct BakedMaterial {
    /// `user_parameter_160..183`.
    pub real_vectors: Vec<[f32; 4]>,
    /// `user_vertex_parameter_240..247`.
    pub real_vertex_vectors: Vec<[f32; 4]>,
    /// `ps_material_shader_transforms[sampler]`: scale x, scale y, offset
    /// x, offset y.
    pub texture_constants: Vec<[f32; 4]>,
    pub textures: Vec<BakedTexture>,
    /// Pixel bools by register 0–31.
    pub bools: BakedBool,
    /// Vertex bools by register − 40 (40–47).
    pub vertex_bools: BakedBool,
    pub functions: Vec<BakedFunction>,
    /// `extern parameters`: index, register, offset, phase mask.
    pub externs: Vec<[u8; 4]>,
    pub alpha_blend_mode: u8,
    pub layer_blend_mode: LayerBlendMode,
    /// `materialPostprocessFlags`.
    pub flags: u16,
    pub sort_layer: u8,
}

/// `materialPostprocessFlags` bits.
pub mod postprocess_flags {
    pub const WIREFRAME_OUTLINE: u16 = 0x1;
    pub const FORCE_SINGLE_PASS: u16 = 0x2;
    pub const PIXEL_CONSTANT_FUNCTIONS: u16 = 0x4;
    pub const VERTEX_CONSTANT_FUNCTIONS: u16 = 0x8;
    pub const TEXTURE_TRANSFORM_FUNCTIONS: u16 = 0x10;
    pub const TEXTURE_FRAME_FUNCTIONS: u16 = 0x20;
    pub const RESOLVE_SCREEN: u16 = 0x40;
    pub const DISABLE_ATMOSPHERE_FOG: u16 = 0x80;
    pub const USES_DEPTH_CAMERA: u16 = 0x100;
    pub const MATERIAL_IS_VARIABLE: u16 = 0x200;
}

/// Entry points and vertex types the force-single-pass test reads.
const ENTRY_ALBEDO: usize = 1;
const ENTRY_STATIC_PER_VERTEX: usize = 7;
const ENTRY_SINGLE_PASS_PER_VERTEX: usize = 22;
const VERTEX_WORLD: usize = 0;

/// Externs the bake adds (`e_material_extern`).
const EXTERN_EMBLEM: u8 = 18;
const EXTERN_DYNAMIC_CUBEMAP: u8 = 26;

fn near_zero(value: f32) -> bool {
    value.abs() < 0.0001
}

/// `sub_14090A890`: a bitmap named by a parameter's `bitmap path`. The path
/// as written, then with its extensions cut, then the file name under
/// `shaders\default_bitmaps\bitmaps`.
fn bitmap_from_path(path: &str, bitmaps: &impl BitmapLookup) -> Option<(String, bool)> {
    if path.is_empty() {
        return None;
    }
    let cut = |text: &str| text.split('.').next().unwrap_or(text).to_owned();
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    [path.to_owned(), cut(path), format!("shaders\\default_bitmaps\\bitmaps\\{}", cut(file))]
        .into_iter()
        .find_map(|candidate| bitmaps.find(&candidate).map(|cube| (candidate, cube)))
}

/// The bitmap a texture binds: the source parameter's reference, its path,
/// then the shader parameter's path, then `color_white`.
fn resolve_bitmap(source: &MaterialParameter, shader: &MaterialParameter, bitmaps: &impl BitmapLookup) -> (String, bool) {
    source
        .bitmap
        .as_deref()
        .and_then(|path| bitmaps.find(path).map(|cube| (path.to_owned(), cube)))
        .or_else(|| bitmap_from_path(&source.bitmap_path, bitmaps))
        .or_else(|| bitmap_from_path(&shader.bitmap_path, bitmaps))
        .unwrap_or_else(|| (DEFAULT_BITMAP.to_owned(), bitmaps.find(DEFAULT_BITMAP).unwrap_or(false)))
}

/// Where a real or colour parameter's lanes live (`GetRealConstantStorage`
/// 0x1408815B0): pixel registers 160–183, vertex 240–247.
fn real_storage<'a>(baked: &'a mut BakedMaterial, shader: &MaterialParameter) -> Option<(&'a mut [f32; 4], usize, bool)> {
    let (vectors, base, range, pixel) = if shader.is_vertex() {
        (&mut baked.real_vertex_vectors, 240usize, 8usize, false)
    } else {
        (&mut baked.real_vectors, 160, 24, true)
    };
    let slot = usize::from(shader.register_index).checked_sub(base).filter(|&slot| slot < range)?;
    if vectors.len() <= slot {
        vectors.resize(slot + 1, [0.0; 4]);
    }
    Some((&mut vectors[slot], usize::from(shader.register_offset), pixel))
}

/// Where a function's static value lands.
enum Storage {
    None,
    Real,
    Transform { slot: usize, texture: usize },
}

/// Run the bake. `material` overrides `shader`'s parameters by name;
/// registers always come from the shader.
pub fn bake(material: &Material, shader: &MaterialShader, bitmaps: &impl BitmapLookup) -> BakedMaterial {
    let mut baked = BakedMaterial::default();
    for kind in MaterialParameterType::ALL {
        for shader_parameter in shader.parameters.iter().filter(|parameter| parameter.kind == kind) {
            let source = material.parameter(&shader_parameter.name).unwrap_or(shader_parameter);
            let mut storage = Storage::None;
            let mut pixel = true;
            match kind {
                MaterialParameterType::Real => {
                    if let Some((lanes, offset, is_pixel)) = real_storage(&mut baked, shader_parameter) {
                        let values = [source.real, source.vector[0], source.vector[1], source.vector[2]];
                        let count = usize::from(shader_parameter.register_count).min(4 - offset.min(4));
                        lanes[offset..offset + count].copy_from_slice(&values[..count]);
                        pixel = is_pixel;
                        storage = Storage::Real;
                    }
                }
                MaterialParameterType::Color => {
                    if let Some((lanes, offset, is_pixel)) = real_storage(&mut baked, shader_parameter) {
                        let [alpha, red, green, blue] = source.color;
                        let rgb = [color_degamma(red), color_degamma(green), color_degamma(blue)];
                        for (lane, value) in rgb.into_iter().enumerate() {
                            if let Some(slot) = lanes.get_mut(offset + lane) {
                                *slot = value;
                            }
                        }
                        if shader_parameter.register_count >= 4 {
                            if let Some(slot) = lanes.get_mut(offset + 3) {
                                *slot = alpha;
                            }
                        }
                        pixel = is_pixel;
                        storage = Storage::Real;
                    }
                }
                MaterialParameterType::Bool => {
                    let register = shader_parameter.register_index;
                    let (group, bit) = match register {
                        0..=31 => (&mut baked.bools, register),
                        40..=47 => {
                            pixel = false;
                            (&mut baked.vertex_bools, register - 40)
                        }
                        _ => continue,
                    };
                    group.phase_mask |= if shader_parameter.render_phases_used == 0 {
                        u32::MAX
                    } else {
                        u32::from(shader_parameter.render_phases_used)
                    };
                    group.used |= 1 << bit;
                    if source.int_bool != 0 {
                        group.values |= 1 << bit;
                    } else {
                        group.values &= !(1 << bit);
                    }
                }
                // "Integer type not yet implemented properly".
                MaterialParameterType::Int => {}
                MaterialParameterType::Bitmap => {
                    let (bitmap, cube) = resolve_bitmap(source, shader_parameter, bitmaps);
                    let mut texture = BakedTexture {
                        parameter: shader_parameter.name.clone(),
                        bitmap,
                        sampler: shader_parameter.register_index & 0x1F,
                        vertex: shader_parameter.is_vertex(),
                        extern_mode: if source.bitmap_flags & 0x10 != 0 { source.bitmap_extern_mode & 0x7 } else { 0 },
                        address_u: u8::from(cube),
                        address_v: u8::from(cube),
                        filter: 0,
                        mip_bias_mode: if cube { 10 } else { 8 },
                        frame_index_function: None,
                        min_mip: -1,
                        max_mip: -1,
                        phase_mask: shader_parameter.phase_mask(),
                    };
                    if texture.vertex {
                        // Vertex samplers count from register 9.
                        texture.sampler = shader_parameter.register_index.wrapping_sub(9) & 0x1F;
                        pixel = false;
                    } else if texture.sampler < 12 {
                        let slot = usize::from(texture.sampler);
                        if baked.texture_constants.len() <= slot {
                            baked.texture_constants.resize(slot + 1, [0.0; 4]);
                        }
                        // The material's own scale and offset hold only with no
                        // functions and a non-zero scale; else the shader's.
                        let own = source.functions.is_empty() && !near_zero(source.real) && !near_zero(source.vector[0]);
                        let from = if own { source } else { shader_parameter };
                        baked.texture_constants[slot] = [from.real, from.vector[0], from.vector[1], from.vector[2]];
                        storage = Storage::Transform { slot, texture: baked.textures.len() };
                    }
                    let flags = source.bitmap_flags;
                    if flags != 0 {
                        if flags & 0x4 != 0 {
                            texture.address_u = source.bitmap_address_mode_x as u8;
                        } else if flags & 0x2 != 0 {
                            texture.address_u = source.bitmap_address_mode as u8;
                        }
                        if flags & 0x8 != 0 {
                            texture.address_v = source.bitmap_address_mode_y as u8;
                        } else if flags & 0x2 != 0 {
                            texture.address_v = source.bitmap_address_mode as u8;
                        }
                        if flags & 0x1 != 0 {
                            texture.filter = (source.bitmap_filter_mode & 0xF) as u8;
                        }
                        if flags & 0x20 != 0 {
                            texture.mip_bias_mode = (source.bitmap_sharpen_mode & 0xF) as u8;
                        }
                        if flags & 0x80 != 0 {
                            texture.min_mip = source.bitmap_min_mipmap as i8;
                        }
                        if flags & 0x100 != 0 {
                            texture.max_mip = source.bitmap_max_mipmap as i8;
                        }
                    }
                    baked.textures.push(texture);
                }
            }

            // The source parameter's functions: dynamic ones are queued, and
            // every one leaves its value at rest in the static constant.
            for function in &source.functions {
                let mut queued = None;
                if function.is_dynamic() {
                    let mut function = function.clone();
                    if near_zero(function.time_period) {
                        function.time_period = 1.0;
                    }
                    if !function.input_name.is_empty() || !function.range_name.is_empty() {
                        baked.flags |= postprocess_flags::MATERIAL_IS_VARIABLE;
                    }
                    baked.flags |= match function.kind {
                        kind if kind.is_transform() => postprocess_flags::TEXTURE_TRANSFORM_FUNCTIONS,
                        MaterialFunctionType::FrameIndex => postprocess_flags::TEXTURE_FRAME_FUNCTIONS,
                        _ if pixel => postprocess_flags::PIXEL_CONSTANT_FUNCTIONS,
                        _ => postprocess_flags::VERTEX_CONSTANT_FUNCTIONS,
                    };
                    queued = Some(baked.functions.len());
                    baked.functions.push(BakedFunction {
                        parameter: shader_parameter.name.clone(),
                        parameter_kind: kind,
                        function,
                        phase_mask: shader_parameter.phase_mask(),
                        register_index: shader_parameter.register_index,
                        register_offset: shader_parameter.register_offset,
                        vertex: shader_parameter.is_vertex(),
                    });
                }
                let Some(curve) = function.function.as_ref() else { continue };
                match storage {
                    Storage::None => {}
                    Storage::Real => {
                        let Some((lanes, offset, _)) = real_storage(&mut baked, shader_parameter) else { continue };
                        let mut write = |lane: usize, value: f32| {
                            if let Some(slot) = lanes.get_mut(offset + lane) {
                                *slot = value;
                            }
                        };
                        if function.kind == MaterialFunctionType::Color {
                            let color = curve.evaluate_color(0.0, 0.0);
                            write(0, color_degamma(color.red));
                            write(1, color_degamma(color.green));
                            write(2, color_degamma(color.blue));
                        } else {
                            write_static(function.kind, curve.evaluate(0.0, 0.0), &mut write);
                        }
                        // MCC only: these two stay inside their display range.
                        if source.register_count == 1 && matches!(shader_parameter.name.as_str(), "fresnelpower" | "fresnel_inv") {
                            if let Some(slot) = lanes.get_mut(offset) {
                                *slot = slot.max(source.display_minimum).min(source.display_maximum);
                            }
                        }
                    }
                    Storage::Transform { slot, texture } => {
                        if function.kind == MaterialFunctionType::FrameIndex {
                            baked.textures[texture].frame_index_function = queued;
                            continue;
                        }
                        let lanes = &mut baked.texture_constants[slot];
                        if function.kind == MaterialFunctionType::Color {
                            let color = curve.evaluate_color(0.0, 0.0);
                            lanes[0] = color_degamma(color.red);
                            lanes[1] = color_degamma(color.green);
                            lanes[2] = color_degamma(color.blue);
                        } else {
                            write_static(function.kind, curve.evaluate(0.0, 0.0), &mut |lane, value| lanes[lane] = value);
                        }
                    }
                }
            }
            update_related(&mut baked, material, shader, source, shader_parameter);
        }
    }

    if shader.flags & shader_flags::IS_EMBLEM != 0 {
        baked.externs.push([EXTERN_EMBLEM, 0xFF, 0, 0xFF]);
    }
    baked.alpha_blend_mode = material.alpha_blend_mode;
    baked.sort_layer = material.sort_layer;
    let screen = material.shader.to_ascii_lowercase().contains("material_shaders\\screen\\");
    if screen {
        if baked.sort_layer > 3 {
            baked.sort_layer = 0;
        }
        if material.render_flags & 1 != 0 {
            baked.flags |= postprocess_flags::RESOLVE_SCREEN;
        } else {
            baked.flags &= !postprocess_flags::RESOLVE_SCREEN;
        }
    } else {
        if !(1..=3).contains(&baked.sort_layer) {
            baked.sort_layer = 2;
        }
        if material.transparent_shadow_policy == 0 && material.render_flags & 1 != 0 {
            baked.flags |= postprocess_flags::DISABLE_ATMOSPHERE_FOG;
        } else {
            baked.flags &= !postprocess_flags::DISABLE_ATMOSPHERE_FOG;
        }
    }
    if shader.flags & shader_flags::IS_VOLUME_FOG != 0 {
        baked.alpha_blend_mode = 1;
    }
    baked
}

/// A scalar function's lane, by what it drives.
fn write_static(kind: MaterialFunctionType, value: f32, write: &mut impl FnMut(usize, f32)) {
    match kind {
        MaterialFunctionType::ScaleUniform => {
            write(1, value);
            write(0, value);
        }
        MaterialFunctionType::ScaleV => write(1, value),
        MaterialFunctionType::OffsetU => write(2, value),
        MaterialFunctionType::OffsetV => write(3, value),
        MaterialFunctionType::FrameIndex | MaterialFunctionType::Color => {}
        MaterialFunctionType::Value | MaterialFunctionType::ScaleU | MaterialFunctionType::Alpha => write(0, value),
    }
}

/// `UpdateRelatedPostprocessProperties` (0x140881B90), once per parameter.
fn update_related(
    baked: &mut BakedMaterial,
    material: &Material,
    shader: &MaterialShader,
    source: &MaterialParameter,
    shader_parameter: &MaterialParameter,
) {
    let name = source.name.as_str();
    if name == "wireframe_outline" && source.int_bool != 0 {
        baked.flags |= postprocess_flags::WIREFRAME_OUTLINE;
    }
    if source.bitmap_flags & 0x40 != 0 {
        baked.externs.push([
            EXTERN_DYNAMIC_CUBEMAP,
            shader_parameter.register_index,
            shader_parameter.register_offset,
            shader_parameter.phase_mask(),
        ]);
        baked.flags |= postprocess_flags::MATERIAL_IS_VARIABLE;
    }
    // Albedo compiled for world vertices with neither per-vertex lit entry:
    // the material draws in one pass.
    if !material.shader.is_empty()
        && shader.has_vertex_shader(ENTRY_ALBEDO, VERTEX_WORLD)
        && !shader.has_vertex_shader(ENTRY_STATIC_PER_VERTEX, VERTEX_WORLD)
        && !shader.has_vertex_shader(ENTRY_SINGLE_PASS_PER_VERTEX, VERTEX_WORLD)
    {
        baked.flags |= postprocess_flags::FORCE_SINGLE_PASS;
    }
    match baked.layer_blend_mode {
        LayerBlendMode::None if name == "blend_map" => baked.layer_blend_mode = LayerBlendMode::Layered,
        // `sub_1400738C0` compares without case [inferred from its use].
        LayerBlendMode::Layered if name.eq_ignore_ascii_case("r_color") => baked.layer_blend_mode = LayerBlendMode::Blended,
        _ => {}
    }
    if shader.flags & shader_flags::IS_HOLOGRAM != 0 && name.starts_with("lgt_") {
        let index = match name {
            "lgt_key_forward_value" => Some(35),
            "lgt_fill_forward_value" => Some(37),
            "lgt_rim_forward_value" => Some(39),
            "lgt_key_intensity" => Some(36),
            "lgt_fill_intensity" => Some(38),
            "lgt_rim_intensity" => Some(40),
            _ => None,
        };
        if let Some(index) = index {
            baked.externs.push([index, shader_parameter.register_index, shader_parameter.register_offset, 0xFF]);
            baked.flags |= postprocess_flags::MATERIAL_IS_VARIABLE;
        }
    }
    if source.kind == MaterialParameterType::Bitmap && source.bitmap_extern_mode == 4 {
        baked.flags |= postprocess_flags::USES_DEPTH_CAMERA | postprocess_flags::MATERIAL_IS_VARIABLE;
    }
}
