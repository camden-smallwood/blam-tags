use super::bake::postprocess_flags;
use super::*;

/// An unranged constant function whose value is `value`.
fn constant(value: f32) -> TagFunction {
    let mut bytes = vec![0x01, 0x20, 0x00, 0x00];
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes.extend_from_slice(&[0; 20]);
    TagFunction::parse(&bytes).unwrap()
}

fn function(kind: MaterialFunctionType, curve: TagFunction) -> MaterialFunctionParameter {
    MaterialFunctionParameter {
        kind,
        input_name: String::new(),
        range_name: String::new(),
        output_modifier: 0,
        output_modifier_input: String::new(),
        time_period: 0.0,
        function: Some(curve),
    }
}

fn parameter(name: &str, kind: MaterialParameterType) -> MaterialParameter {
    MaterialParameter {
        name: name.to_owned(),
        kind,
        bitmap: None,
        bitmap_path: String::new(),
        color: [0.0; 4],
        real: 0.0,
        vector: [0.0; 3],
        int_bool: 0,
        bitmap_flags: 0,
        bitmap_filter_mode: 0,
        bitmap_address_mode: 0,
        bitmap_address_mode_x: 0,
        bitmap_address_mode_y: 0,
        bitmap_sharpen_mode: 0,
        bitmap_extern_mode: 0,
        bitmap_min_mipmap: 0,
        bitmap_max_mipmap: 0,
        render_phases_used: 0,
        functions: Vec::new(),
        display_minimum: 0.0,
        display_maximum: 0.0,
        register_index: 0,
        register_offset: 0,
        register_count: 0,
        register_set: 0,
    }
}

fn real(name: &str, register: u8, offset: u8, count: u8, values: [f32; 4]) -> MaterialParameter {
    MaterialParameter {
        real: values[0],
        vector: [values[1], values[2], values[3]],
        register_index: register,
        register_offset: offset,
        register_count: count,
        register_set: 2,
        ..parameter(name, MaterialParameterType::Real)
    }
}

fn sampler(name: &str, register: u8, scale: [f32; 4]) -> MaterialParameter {
    MaterialParameter {
        real: scale[0],
        vector: [scale[1], scale[2], scale[3]],
        register_index: register,
        register_count: 1,
        register_set: 3,
        ..parameter(name, MaterialParameterType::Bitmap)
    }
}

fn shader(parameters: Vec<MaterialParameter>) -> MaterialShader {
    MaterialShader { flags: 0, parameters, vertex_shader_hashes: Vec::new() }
}

fn material(parameters: Vec<MaterialParameter>) -> Material {
    Material { shader: "shaders\\material_shaders\\materials\\srf_blinn".to_owned(), parameters, ..Material::default() }
}

/// Every bitmap exists, none is a cube.
fn any_bitmap(_: &str) -> Option<bool> {
    Some(false)
}

#[test]
fn values_come_from_the_material_and_registers_from_the_shader() {
    let shader = shader(vec![real("tint", 161, 1, 3, [0.5, 0.5, 0.5, 0.0]), real("gloss", 160, 3, 1, [0.25, 0.0, 0.0, 0.0])]);
    // The material's copy carries no registers: they must be the shader's.
    let material = material(vec![MaterialParameter { real: 0.9, vector: [0.8, 0.7, 0.0], ..parameter("tint", MaterialParameterType::Real) }]);
    let baked = bake(&material, &shader, &any_bitmap);
    assert_eq!(baked.real_vectors[1], [0.0, 0.9, 0.8, 0.7]);
    // Not overridden: the shader's own value.
    assert_eq!(baked.real_vectors[0], [0.0, 0.0, 0.0, 0.25]);
}

#[test]
fn a_constant_function_overrides_the_stored_real() {
    let shader = shader(vec![real("diffuse_intensity", 160, 0, 1, [1.0, 0.0, 0.0, 0.0])]);
    let override_ = MaterialParameter {
        real: 0.3,
        functions: vec![function(MaterialFunctionType::Value, constant(0.7))],
        ..parameter("diffuse_intensity", MaterialParameterType::Real)
    };
    let baked = bake(&material(vec![override_]), &shader, &any_bitmap);
    assert_eq!(baked.real_vectors[0][0], 0.7, "the function's value, not `real` (0.3)");
    assert!(baked.functions.is_empty(), "a constant function is not queued");
}

#[test]
fn colors_degamma_and_keep_alpha_only_in_a_fourth_lane() {
    let color = |name: &str, register: u8, count: u8| MaterialParameter {
        register_index: register,
        register_count: count,
        register_set: 2,
        ..parameter(name, MaterialParameterType::Color)
    };
    let shader = shader(vec![color("rgb", 160, 3), color("rgba", 161, 4)]);
    let set = |name: &str| MaterialParameter { color: [0.5, 0.5, 1.0, 0.0], ..parameter(name, MaterialParameterType::Color) };
    let baked = bake(&material(vec![set("rgb"), set("rgba")]), &shader, &any_bitmap);
    let half = 0.5f32.powf(2.2);
    assert_eq!(baked.real_vectors[0], [half, 1.0, 0.0, 0.0]);
    assert_eq!(baked.real_vectors[1], [half, 1.0, 0.0, 0.5]);
}

#[test]
fn a_texture_keeps_its_own_transform_only_without_functions_or_zero_scale() {
    let shader = shader(vec![
        sampler("color_map", 0, [1.0, 1.0, 0.0, 0.0]),
        sampler("detail_map", 1, [4.0, 4.0, 0.0, 0.0]),
        sampler("other_map", 2, [2.0, 2.0, 0.0, 0.0]),
    ]);
    let own = MaterialParameter { real: 3.0, vector: [5.0, 0.5, 0.25], ..parameter("color_map", MaterialParameterType::Bitmap) };
    let zero = MaterialParameter { real: 3.0, vector: [0.0, 0.5, 0.25], ..parameter("detail_map", MaterialParameterType::Bitmap) };
    let animated = MaterialParameter {
        real: 3.0,
        vector: [5.0, 0.0, 0.0],
        functions: vec![function(MaterialFunctionType::OffsetU, constant(0.125))],
        ..parameter("other_map", MaterialParameterType::Bitmap)
    };
    let baked = bake(&material(vec![own, zero, animated]), &shader, &any_bitmap);
    assert_eq!(baked.texture_constants[0], [3.0, 5.0, 0.5, 0.25]);
    assert_eq!(baked.texture_constants[1], [4.0, 4.0, 0.0, 0.0], "a zero scale takes the shader's");
    // A function makes the shader's transform the base, then writes its lane.
    assert_eq!(baked.texture_constants[2], [2.0, 2.0, 0.125, 0.0]);
}

#[test]
fn bitmaps_fall_back_from_reference_to_paths_to_white() {
    let exists = |path: &str| matches!(path, "objects\\a\\found" | "shaders\\default_bitmaps\\bitmaps\\gray_50" | "objects\\cube").then_some(path == "objects\\cube");
    let shader_parameter = |name: &str, register: u8, path: &str| MaterialParameter {
        bitmap_path: path.to_owned(),
        ..sampler(name, register, [1.0, 1.0, 0.0, 0.0])
    };
    let shader = shader(vec![
        shader_parameter("by_reference", 0, ""),
        shader_parameter("by_cut_path", 1, ""),
        shader_parameter("by_default_folder", 2, ""),
        shader_parameter("by_shader_path", 3, "objects\\a\\found.tif"),
        shader_parameter("nothing", 4, "nowhere"),
        shader_parameter("cube", 5, ""),
    ]);
    let set = |name: &str, bitmap: Option<&str>, path: &str| MaterialParameter {
        bitmap: bitmap.map(str::to_owned),
        bitmap_path: path.to_owned(),
        ..parameter(name, MaterialParameterType::Bitmap)
    };
    let material = material(vec![
        set("by_reference", Some("objects\\a\\found"), ""),
        set("by_cut_path", Some("objects\\missing"), "objects\\a\\found.bitmap.tif"),
        set("by_default_folder", None, "art\\textures\\gray_50.tif"),
        set("by_shader_path", None, ""),
        set("nothing", None, "missing"),
        set("cube", Some("objects\\cube"), ""),
    ]);
    let baked = bake(&material, &shader, &exists);
    let bitmaps: Vec<&str> = baked.textures.iter().map(|texture| texture.bitmap.as_str()).collect();
    assert_eq!(
        bitmaps,
        [
            "objects\\a\\found",
            "objects\\a\\found",
            "shaders\\default_bitmaps\\bitmaps\\gray_50",
            "objects\\a\\found",
            crate::render_method::DEFAULT_BITMAP,
            "objects\\cube",
        ]
    );
    // A cube clamps and biases its mips by mode 10; anything else wraps at 8.
    assert_eq!((baked.textures[5].address_u, baked.textures[5].address_v, baked.textures[5].mip_bias_mode), (1, 1, 10));
    assert_eq!((baked.textures[0].address_u, baked.textures[0].address_v, baked.textures[0].mip_bias_mode), (0, 0, 8));
}

#[test]
fn bitmap_flags_override_sampler_state() {
    let shader = shader(vec![sampler("a", 0, [1.0, 1.0, 0.0, 0.0]), sampler("b", 1, [1.0, 1.0, 0.0, 0.0])]);
    let set = |name: &str, flags: u16| MaterialParameter {
        bitmap_flags: flags,
        bitmap_filter_mode: 4,
        bitmap_address_mode: 2,
        bitmap_address_mode_x: 1,
        bitmap_address_mode_y: 3,
        bitmap_sharpen_mode: 11,
        bitmap_min_mipmap: 2,
        bitmap_max_mipmap: 5,
        ..parameter(name, MaterialParameterType::Bitmap)
    };
    // 0x21: filter and sharpen, as the assault rifle's metal.
    let baked = bake(&material(vec![set("a", 0x21), set("b", 0x2 | 0x8 | 0x80 | 0x100)]), &shader, &any_bitmap);
    let a = &baked.textures[0];
    assert_eq!((a.filter, a.mip_bias_mode, a.address_u, a.address_v, a.min_mip), (4, 11, 0, 0, -1));
    let b = &baked.textures[1];
    // 0x2 sets both axes, 0x8 then takes v from `address mode y`.
    assert_eq!((b.filter, b.mip_bias_mode, b.address_u, b.address_v, b.min_mip, b.max_mip), (0, 8, 2, 3, 2, 5));
}

#[test]
fn dynamic_functions_are_queued_and_constant_ones_are_not() {
    let shader = shader(vec![real("a", 160, 0, 1, [0.0; 4]), real("b", 160, 1, 1, [0.0; 4])]);
    let mut modified = function(MaterialFunctionType::Value, constant(1.0));
    modified.output_modifier = 2;
    modified.output_modifier_input = "shield_vitality".to_owned();
    let a = MaterialParameter { functions: vec![modified], ..parameter("a", MaterialParameterType::Real) };
    let b = MaterialParameter { functions: vec![function(MaterialFunctionType::Value, constant(1.0))], ..parameter("b", MaterialParameterType::Real) };
    let baked = bake(&material(vec![a, b]), &shader, &any_bitmap);
    assert_eq!(baked.functions.len(), 1, "only the modified constant runs per draw");
    assert_eq!(baked.functions[0].parameter, "a");
    assert_eq!(baked.functions[0].function.time_period, 1.0, "a zero period reads as one second");
    assert_ne!(baked.flags & postprocess_flags::PIXEL_CONSTANT_FUNCTIONS, 0);
}

#[test]
fn fresnel_power_stays_in_its_display_range() {
    let clamped = |name: &str| MaterialParameter { display_minimum: 0.5, display_maximum: 4.0, ..real(name, 160, 0, 1, [0.0; 4]) };
    let mut fresnel = clamped("fresnelpower");
    fresnel.register_index = 160;
    let mut other = clamped("glossiness");
    other.register_index = 161;
    let shader = shader(vec![fresnel, other]);
    let set = |name: &str| MaterialParameter {
        display_minimum: 0.5,
        display_maximum: 4.0,
        register_count: 1,
        functions: vec![function(MaterialFunctionType::Value, constant(9.0))],
        ..parameter(name, MaterialParameterType::Real)
    };
    let baked = bake(&material(vec![set("fresnelpower"), set("glossiness")]), &shader, &any_bitmap);
    assert_eq!(baked.real_vectors[0][0], 4.0);
    assert_eq!(baked.real_vectors[1][0], 9.0, "only the fresnel names clamp");
}

#[test]
fn bools_set_their_bits_and_phase_masks_by_register() {
    let flag = |name: &str, register: u8, phases: u8| MaterialParameter {
        register_index: register,
        register_count: 1,
        render_phases_used: phases,
        ..parameter(name, MaterialParameterType::Bool)
    };
    let shader = shader(vec![flag("a", 3, 4), flag("b", 5, 0), flag("v", 41, 1)]);
    let on = |name: &str| MaterialParameter { int_bool: 1, ..parameter(name, MaterialParameterType::Bool) };
    let baked = bake(&material(vec![on("a"), on("v")]), &shader, &any_bitmap);
    assert_eq!(baked.bools.values, 1 << 3);
    assert_eq!(baked.bools.used, (1 << 3) | (1 << 5));
    assert_eq!(baked.bools.phase_mask, u32::MAX, "a parameter with no phases opens every phase");
    assert_eq!((baked.vertex_bools.values, baked.vertex_bools.used, baked.vertex_bools.phase_mask), (1 << 1, 1 << 1, 1));
}

#[test]
fn sort_layer_and_single_pass_follow_the_shader() {
    // The test runs per parameter (`UpdateRelatedPostprocessProperties`), so
    // the shader needs one.
    let mut single = shader(vec![real("a", 160, 0, 1, [0.0; 4])]);
    single.vertex_shader_hashes = vec![vec![], vec![12345], vec![], vec![], vec![], vec![], vec![], vec![-1]];
    let baked = bake(&material(Vec::new()), &single, &any_bitmap);
    assert_eq!(baked.sort_layer, 2, "an invalid layer is normal outside screen materials");
    assert_ne!(baked.flags & postprocess_flags::FORCE_SINGLE_PASS, 0);

    let mut lit = single.clone();
    lit.vertex_shader_hashes[7] = vec![678];
    let mut screen = material(Vec::new());
    screen.shader = "shaders\\material_shaders\\screen\\tint_alpha".to_owned();
    let baked = bake(&screen, &lit, &any_bitmap);
    assert_eq!(baked.sort_layer, 0, "a screen material keeps layer 0");
    assert_eq!(baked.flags & postprocess_flags::FORCE_SINGLE_PASS, 0, "it has a per-vertex lit entry");
}

#[test]
fn entry_points_submit_their_phases() {
    assert_eq!(entry_point_phase_mask(1), 1);
    assert_eq!(entry_point_phase_mask(8), 2);
    assert_eq!(entry_point_phase_mask(23), 4);
    assert_eq!(entry_point_phase_mask(12), 0);
}

/// The kit's own tags read and bake: the Chief's helmet keeps its color map
/// and the shader's registers.
#[test]
fn kit_materials_read_and_bake() {
    let Some(tags) = std::env::var_os("BLAM_TEST_H4EK").map(std::path::PathBuf::from) else {
        eprintln!("skipping kit_materials_read_and_bake: set BLAM_TEST_H4EK");
        return;
    };
    let tags = if tags.join("tags").is_dir() { tags.join("tags") } else { tags };
    let read = |path: &str| crate::TagFile::read(tags.join(path)).unwrap_or_else(|error| panic!("{path}: {error}"));
    let material = Material::from_tag(&read(
        "objects/characters/storm_masterchief/materials/storm_masterchief_default/storm_masterchief_default_armor_helm.material",
    ));
    assert!(!material.shader.is_empty());
    let shader = MaterialShader::from_tag(&read(&format!("{}.material_shader", material.shader.replace('\\', "/"))));
    assert!(!shader.parameters.is_empty());
    assert!(shader.parameters.iter().any(|parameter| parameter.register_index != 0), "the shader holds the registers");
    let exists = |path: &str| tags.join(format!("{}.bitmap", path.replace('\\', "/"))).is_file().then_some(false);
    let baked = bake(&material, &shader, &exists);
    assert!(!baked.textures.is_empty());
    let white = crate::render_method::DEFAULT_BITMAP;
    assert!(baked.textures.iter().any(|texture| texture.bitmap != white), "{:?}", baked.textures);
    eprintln!("NOTE shader {} flags {:#x} blend {} layer {}", material.shader, baked.flags, baked.alpha_blend_mode, baked.sort_layer);
    for texture in &baked.textures {
        eprintln!("NOTE tex s{} {} {} addr {}/{} filter {} bias {}", texture.sampler, texture.parameter, texture.bitmap, texture.address_u, texture.address_v, texture.filter, texture.mip_bias_mode);
    }
    for (index, vector) in baked.real_vectors.iter().enumerate() {
        eprintln!("NOTE c{} {:?}", 160 + index, vector);
    }
    for (index, vector) in baked.texture_constants.iter().enumerate() {
        eprintln!("NOTE xform s{index} {vector:?}");
    }
    for function in &baked.functions {
        eprintln!("NOTE fn {} {:?} input {:?}", function.parameter, function.function.kind, function.function.input_name);
    }
}

/// Every material in the kit reads and bakes against its shader, and nearly
/// every texture finds a real bitmap.
#[test]
fn every_kit_material_bakes() {
    let Some(tags) = std::env::var_os("BLAM_TEST_H4EK").map(std::path::PathBuf::from) else {
        eprintln!("skipping every_kit_material_bakes: set BLAM_TEST_H4EK");
        return;
    };
    let tags = if tags.join("tags").is_dir() { tags.join("tags") } else { tags };
    let mut stack = vec![tags.clone()];
    let mut paths = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "material") {
                paths.push(path);
            }
        }
    }
    assert!(paths.len() > 6000, "kit materials: {}", paths.len());
    let mut shaders: std::collections::HashMap<String, Option<MaterialShader>> = std::collections::HashMap::new();
    let exists = |path: &str| tags.join(format!("{}.bitmap", path.replace('\\', "/"))).is_file().then_some(false);
    let (mut baked_count, mut missing_shader, mut textures, mut white) = (0, 0, 0, 0);
    for path in &paths {
        let material = Material::from_tag(&crate::TagFile::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display())));
        let shader = shaders
            .entry(material.shader.clone())
            .or_insert_with(|| {
                let file = tags.join(format!("{}.material_shader", material.shader.replace('\\', "/")));
                crate::TagFile::read(file).ok().map(|tag| MaterialShader::from_tag(&tag))
            })
            .clone();
        let Some(shader) = shader else {
            missing_shader += 1;
            continue;
        };
        let baked = bake(&material, &shader, &exists);
        baked_count += 1;
        textures += baked.textures.len();
        white += baked.textures.iter().filter(|texture| texture.bitmap == crate::render_method::DEFAULT_BITMAP).count();
    }
    eprintln!("NOTE baked {baked_count}, missing shader {missing_shader}, textures {textures}, white {white}");
    assert!(baked_count > 6000);
    assert!(white * 10 < textures, "most textures resolve: {white} of {textures} fell to white");
}

/// An identity curve mapped onto `min..max`.
fn ramp(min: f32, max: f32) -> TagFunction {
    let mut curve = constant(0.0);
    let blob = curve.as_blob_mut().unwrap();
    blob.set_function_type(crate::tag_function::FunctionType::Identity);
    blob.set_clamp_range(min, max);
    curve
}

struct Object {
    values: Vec<(&'static str, f32)>,
    time: f32,
}

impl FunctionContext for Object {
    fn object_function(&self, name: &str) -> f32 {
        self.values.iter().find(|(known, _)| *known == name).map_or(0.0, |(_, value)| *value)
    }
    fn time(&self) -> f32 {
        self.time
    }
    fn object_index(&self) -> u32 {
        0
    }
}

fn run(function: &MaterialFunctionParameter, object: &Object) -> [f32; 4] {
    let mut lanes = [9.0; 4];
    evaluate_function_output(function, object, &mut lanes);
    lanes
}

/// The input lambda (X360 0x83B06B48): a name is the object's value; no name
/// runs on the clock over the period, wrapped to 0..1.
#[test]
fn a_function_reads_its_objects_value_or_else_the_clock() {
    let mut named = function(MaterialFunctionType::Value, ramp(-100.0, 100.0));
    named.input_name = "cortana_lgt_key_forward".to_owned();
    named.time_period = 1.0;
    let object = Object { values: vec![("cortana_lgt_key_forward", 0.25)], time: 7.0 };
    assert_eq!(run(&named, &object)[0], -50.0);
    let mut clocked = function(MaterialFunctionType::Value, ramp(0.0, 1.0));
    clocked.time_period = 2.0;
    assert_eq!(run(&clocked, &Object { values: Vec::new(), time: 3.0 })[0], 0.5, "3 s over 2 s wraps to 0.5");
}

/// `EvaluateFunction`: a modifier with an input scales or offsets what the
/// curve maps to.
#[test]
fn an_output_modifier_scales_or_offsets_the_mapped_value() {
    let mut scaled = function(MaterialFunctionType::Value, ramp(0.0, 10.0));
    scaled.input_name = "a".to_owned();
    scaled.output_modifier = 2;
    scaled.output_modifier_input = "b".to_owned();
    scaled.time_period = 1.0;
    let object = Object { values: vec![("a", 0.5), ("b", 0.5)], time: 0.0 };
    assert_eq!(run(&scaled, &object)[0], 2.5);
    scaled.output_modifier = 1;
    assert_eq!(run(&scaled, &object)[0], 5.5);
}

/// `EvaluateFunctionOutput`: each kind writes its own lanes from the
/// register's lane, and a colour degammas into three.
#[test]
fn each_kind_writes_its_own_lanes() {
    let object = Object { values: vec![("a", 0.5)], time: 0.0 };
    let at = |kind| {
        let mut function = function(kind, ramp(0.0, 1.0));
        function.input_name = "a".to_owned();
        function.time_period = 1.0;
        run(&function, &object)
    };
    assert_eq!(at(MaterialFunctionType::ScaleUniform), [0.5, 0.5, 9.0, 9.0]);
    assert_eq!(at(MaterialFunctionType::ScaleV), [9.0, 0.5, 9.0, 9.0]);
    assert_eq!(at(MaterialFunctionType::OffsetU), [9.0, 9.0, 0.5, 9.0]);
    assert_eq!(at(MaterialFunctionType::OffsetV), [9.0, 9.0, 9.0, 0.5]);
    assert_eq!(at(MaterialFunctionType::Alpha), [0.5, 9.0, 9.0, 9.0]);
    let gray = 0.5f32.powf(2.2);
    assert_eq!(at(MaterialFunctionType::Color), [gray, gray, gray, 9.0], "a scalar graph is gray");
}

/// `MaterialSubmitUpdateConstants`: a function lands at its register's lane
/// within the range it was handed, and only for the phases asked for.
#[test]
fn constants_update_in_range_and_phase() {
    let shader = shader(vec![real("a", 161, 2, 1, [0.0; 4]), real("b", 240, 0, 1, [0.0; 4])]);
    let dynamic = |name: &str| {
        let mut function = function(MaterialFunctionType::Value, ramp(0.0, 4.0));
        function.input_name = "a".to_owned();
        MaterialParameter { functions: vec![function], ..parameter(name, MaterialParameterType::Real) }
    };
    let mut baked = bake(&material(vec![dynamic("a"), dynamic("b")]), &shader, &any_bitmap);
    let object = Object { values: vec![("a", 0.5)], time: 0.0 };
    let mut pixel = [[0.0; 4]; 24];
    assert_eq!(update_constants(&baked, 0, PIXEL_REGISTERS, &mut pixel, &object), 1, "the vertex one is not in range");
    assert_eq!(pixel[1], [0.0, 0.0, 2.0, 0.0]);
    baked.functions[0].phase_mask = 1;
    assert_eq!(update_constants(&baked, 4, PIXEL_REGISTERS, &mut pixel, &object), 0, "albedo-only, drawn single pass");
    assert_eq!(update_constants(&baked, 1, PIXEL_REGISTERS, &mut pixel, &object), 1);
}
