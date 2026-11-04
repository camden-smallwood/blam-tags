//! Reach's reference shaders: a shader names a template (`reference`) whose
//! options and parameters fill in whatever it leaves unset, before the bake.
//!
//! Mirrors Reach tool.exe's `c_render_method::postprocess_as_child`
//! (0x141166270), which inlines `get_reference_shader` (0x1411653D0), then
//! runs `fix_template_options` (0x141164630) and, inside `postprocess`
//! (0x141165700), `append_parameters_from_reference_shader` (0x14115FD30).
//! Halo 3 and ODST have no `reference`.

use crate::typed_enums::Enum;

use super::types::{
    GlobalSortLayer, RenderMethod, RenderMethodAnimatedParameter, RenderMethodDefinition,
    RenderMethodParameter, RenderMethodParameterType,
};

/// `bitmap_flags` bits: each says the shader sets that sampler field itself.
const FLAG_FILTER: i16 = 0x01;
const FLAG_ADDRESS: i16 = 0x02;
const FLAG_ADDRESS_X: i16 = 0x04;
const FLAG_ADDRESS_Y: i16 = 0x08;
const FLAG_EXTERN: i16 = 0x10;
const FLAG_SHARPEN: i16 = 0x20;

impl RenderMethod {
    /// The reference shader's options and parameters laid under this one's,
    /// as Reach's tool does before baking it. `reference` is the tag
    /// [`Self::reference`] names, loaded by the caller, or `None` without one.
    ///
    /// The reference counts only when it is a template of the same
    /// definition, and this shader is not a template itself; the caller must
    /// not pass a shader as its own reference, which tool refuses. Options
    /// are fixed either way: one per category, an unset one (-1) taking the
    /// reference's, and one still out of range taking the first.
    pub fn apply_reference_shader(&mut self, definition: &RenderMethodDefinition, reference: Option<&RenderMethod>) {
        let reference = reference.filter(|reference| {
            !self.is_template
                && reference.is_template
                && reference.definition_path.eq_ignore_ascii_case(&self.definition_path)
        });
        self.fix_template_options(definition, reference);
        if let Some(reference) = reference {
            self.append_parameters_from_reference_shader(reference);
        }
    }

    /// `fix_template_options`, outside the editor.
    fn fix_template_options(&mut self, definition: &RenderMethodDefinition, reference: Option<&RenderMethod>) {
        // `resize_options`: options added for new categories are NONE.
        self.options.resize(definition.categories.len(), -1);
        for (index, category) in definition.categories.iter().enumerate() {
            let option = &mut self.options[index];
            if *option == -1 {
                *option = reference.and_then(|reference| reference.options.get(index).copied()).unwrap_or(0);
            }
            if *option < 0 || usize::try_from(*option).is_ok_and(|at| at >= category.options.len()) {
                *option = 0;
            }
        }
        if self.sort_layer.get() == GlobalSortLayer::Invalid {
            self.sort_layer = Enum::from_variant(GlobalSortLayer::Normal);
        }
    }

    /// `append_parameters_from_reference_shader`: each reference parameter
    /// this shader lacks (by name and type) is copied; for a bitmap or ARGB
    /// color it has, the reference supplies what it leaves unset.
    fn append_parameters_from_reference_shader(&mut self, reference: &RenderMethod) {
        for from in &reference.parameters {
            let own = self
                .parameters
                .iter_mut()
                .find(|own| own.parameter_name == from.parameter_name && own.parameter_type == from.parameter_type);
            let Some(own) = own else {
                let mut copy = from.clone();
                copy.animated_parameters = from.animated_parameters.iter().map(cloned_animation).collect();
                self.parameters.push(copy);
                continue;
            };
            let kind = from.parameter_type.map(|kind| kind.get());
            if !matches!(kind, Some(RenderMethodParameterType::Bitmap | RenderMethodParameterType::ArgbColor)) {
                continue;
            }
            if kind == Some(RenderMethodParameterType::Bitmap) {
                inherit_bitmap(own, from);
            }
            for animation in &from.animated_parameters {
                let has = own.animated_parameters.iter().any(|known| known.parameter_type == animation.parameter_type);
                if !has {
                    own.animated_parameters.push(cloned_animation(animation));
                }
            }
        }
    }
}

/// A bitmap's reference and sampler fields from `from`, where `own` sets none.
fn inherit_bitmap(own: &mut RenderMethodParameter, from: &RenderMethodParameter) {
    if own.bitmap_path.is_empty() {
        own.bitmap_path = from.bitmap_path.clone();
    }
    let flags = own.bitmap_flags;
    let set = |flag: i16| flags & flag != 0;
    if !set(FLAG_FILTER) {
        own.bitmap_filter_mode = from.bitmap_filter_mode;
    }
    // Tool takes the reference's y mode for an unset x (+38 from +40), as
    // the 360 build does. Reproduced, not corrected.
    if !set(FLAG_ADDRESS_X) {
        own.bitmap_address_mode_x = from.bitmap_address_mode_y;
    }
    if !set(FLAG_ADDRESS_Y) {
        own.bitmap_address_mode_y = from.bitmap_address_mode_y;
    }
    if !set(FLAG_ADDRESS) {
        own.bitmap_address_mode = from.bitmap_address_mode;
    }
    if !set(FLAG_EXTERN) {
        own.bitmap_extern_mode = from.bitmap_extern_mode;
    }
    if !set(FLAG_SHARPEN) {
        own.bitmap_sharpen_mode = from.bitmap_sharpen_mode;
    }
    own.bitmap_flags = flags | from.bitmap_flags;
}

/// An animated parameter as tool copies one: a period of about 0 becomes 1.
fn cloned_animation(animation: &RenderMethodAnimatedParameter) -> RenderMethodAnimatedParameter {
    let mut copy = animation.clone();
    if copy.time_period_in_seconds.abs() < 0.0001 {
        copy.time_period_in_seconds = 1.0;
    }
    copy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_method::types::{
        BitmapAddressMode, BitmapComparisonFunction, BitmapFilterMode, RenderMethodAnimatedParameterType,
        RenderMethodDefinitionCategory, RenderMethodDefinitionCategoryOption,
    };

    fn definition(option_counts: &[usize]) -> RenderMethodDefinition {
        RenderMethodDefinition {
            global_options_path: String::new(),
            categories: option_counts
                .iter()
                .enumerate()
                .map(|(at, count)| RenderMethodDefinitionCategory {
                    category_name: format!("category{at}"),
                    vertex_function: String::new(),
                    pixel_function: String::new(),
                    options: (0..*count)
                        .map(|option| RenderMethodDefinitionCategoryOption {
                            option_name: format!("option{option}"),
                            option_path: String::new(),
                            vertex_function: String::new(),
                            pixel_function: String::new(),
                        })
                        .collect(),
                })
                .collect(),
            shared_pixel_shaders_path: String::new(),
            shared_vertex_shaders_path: String::new(),
            flags: 0,
            version: 0,
        }
    }

    fn shader(options: &[i16], template: bool, parameters: Vec<RenderMethodParameter>) -> RenderMethod {
        RenderMethod {
            definition_path: "shaders\\shader".to_owned(),
            version: 9,
            reference: None,
            is_template: template,
            options: options.to_vec(),
            parameters,
            postprocess_definition: None,
            flags: Default::default(),
            sort_layer: Default::default(),
            runtime_flags: Default::default(),
            custom_fog_setting_index: 0,
            prediction_atom_index: 0,
            group_tag: 0,
            class: Default::default(),
            material_names: Vec::new(),
        }
    }

    fn parameter(name: &str, kind: RenderMethodParameterType) -> RenderMethodParameter {
        RenderMethodParameter {
            parameter_name: name.to_owned(),
            parameter_type: Some(Enum::from_variant(kind)),
            bitmap_path: String::new(),
            real_parameter: 0.0,
            int_parameter: 0,
            color_parameter: Default::default(),
            bitmap_flags: 0,
            bitmap_filter_mode: BitmapFilterMode::Trilinear,
            bitmap_comparison_function: BitmapComparisonFunction::Never,
            bitmap_address_mode: BitmapAddressMode::Wrap,
            bitmap_address_mode_x: BitmapAddressMode::Wrap,
            bitmap_address_mode_y: BitmapAddressMode::Wrap,
            bitmap_anisotropy_amount: 0,
            bitmap_extern_mode: None,
            bitmap_sharpen_mode: 0,
            animated_parameters: Vec::new(),
        }
    }

    fn animation(kind: RenderMethodAnimatedParameterType, period: f32) -> RenderMethodAnimatedParameter {
        RenderMethodAnimatedParameter {
            parameter_type: Some(Enum::from_variant(kind)),
            input_name: String::new(),
            range_name: String::new(),
            time_period_in_seconds: period,
            function: None,
        }
    }

    #[test]
    fn unset_options_take_the_references_and_out_of_range_ones_the_first() {
        let template = shader(&[2, 1, 3], true, Vec::new());
        let mut own = shader(&[-1, 5], false, Vec::new());
        own.apply_reference_shader(&definition(&[3, 2, 4]), Some(&template));
        assert_eq!(own.options, [2, 0, 3], "-1 from the reference, 5 out of range, the new category NONE then the reference's");
        assert_eq!(own.sort_layer.get(), GlobalSortLayer::Normal);

        let mut alone = shader(&[-1, 5], false, Vec::new());
        alone.apply_reference_shader(&definition(&[3, 2, 4]), None);
        assert_eq!(alone.options, [0, 0, 0]);
    }

    #[test]
    fn only_a_template_of_the_same_definition_is_a_reference() {
        let mut not_template = shader(&[2], false, Vec::new());
        let mut own = shader(&[-1], false, Vec::new());
        own.apply_reference_shader(&definition(&[3]), Some(&not_template));
        assert_eq!(own.options, [0]);

        not_template.is_template = true;
        not_template.definition_path = "shaders\\custom".to_owned();
        let mut own = shader(&[-1], false, Vec::new());
        own.apply_reference_shader(&definition(&[3]), Some(&not_template));
        assert_eq!(own.options, [0], "another definition");

        let template = shader(&[2], true, Vec::new());
        let mut itself_a_template = shader(&[-1], true, Vec::new());
        itself_a_template.apply_reference_shader(&definition(&[3]), Some(&template));
        assert_eq!(itself_a_template.options, [0]);
    }

    #[test]
    fn missing_parameters_are_copied_with_a_zero_period_made_one() {
        let mut real = parameter("specular_power", RenderMethodParameterType::Real);
        real.real_parameter = 0.5;
        real.animated_parameters = vec![animation(RenderMethodAnimatedParameterType::Value, 0.0)];
        let template = shader(&[0], true, vec![real]);
        let mut own = shader(&[0], false, vec![parameter("specular_power", RenderMethodParameterType::Bool)]);
        own.apply_reference_shader(&definition(&[1]), Some(&template));
        assert_eq!(own.parameters.len(), 2, "a name of another type is another parameter");
        assert_eq!(own.parameters[1].real_parameter, 0.5);
        assert_eq!(own.parameters[1].animated_parameters[0].time_period_in_seconds, 1.0);
    }

    #[test]
    fn a_bitmap_takes_what_it_does_not_set_and_x_follows_the_references_y() {
        let mut from = parameter("base_map", RenderMethodParameterType::Bitmap);
        from.bitmap_path = "objects\\template\\base".to_owned();
        from.bitmap_flags = 0x40;
        from.bitmap_filter_mode = BitmapFilterMode::Point;
        from.bitmap_address_mode = BitmapAddressMode::Clamp;
        from.bitmap_address_mode_x = BitmapAddressMode::Mirror;
        from.bitmap_address_mode_y = BitmapAddressMode::BlackBorder;
        from.bitmap_sharpen_mode = 3;
        from.animated_parameters = vec![
            animation(RenderMethodAnimatedParameterType::ScaleUniform, 2.0),
            animation(RenderMethodAnimatedParameterType::FrameIndex, 0.0),
        ];
        let template = shader(&[0], true, vec![from]);

        let mut set = parameter("base_map", RenderMethodParameterType::Bitmap);
        set.bitmap_path = "objects\\own\\base".to_owned();
        set.bitmap_flags = FLAG_FILTER | FLAG_ADDRESS_Y;
        set.bitmap_filter_mode = BitmapFilterMode::Bilinear;
        set.bitmap_address_mode_y = BitmapAddressMode::Mirror;
        set.animated_parameters = vec![animation(RenderMethodAnimatedParameterType::ScaleUniform, 4.0)];
        let mut own = shader(&[0], false, vec![set]);
        own.apply_reference_shader(&definition(&[1]), Some(&template));

        let got = &own.parameters[0];
        assert_eq!(got.bitmap_path, "objects\\own\\base", "its own bitmap stays");
        assert_eq!(got.bitmap_filter_mode, BitmapFilterMode::Bilinear, "flag 0x1: its own");
        assert_eq!(got.bitmap_address_mode_y, BitmapAddressMode::Mirror, "flag 0x8: its own");
        assert_eq!(got.bitmap_address_mode_x, BitmapAddressMode::BlackBorder, "the reference's y, not its x");
        assert_eq!(got.bitmap_address_mode, BitmapAddressMode::Clamp);
        assert_eq!(got.bitmap_sharpen_mode, 3);
        assert_eq!(got.bitmap_flags, FLAG_FILTER | FLAG_ADDRESS_Y | 0x40);
        assert_eq!(got.animated_parameters.len(), 2, "only the type it lacks");
        assert_eq!(got.animated_parameters[0].time_period_in_seconds, 4.0);
        assert_eq!(got.animated_parameters[1].time_period_in_seconds, 1.0);

        let mut empty = parameter("base_map", RenderMethodParameterType::Bitmap);
        empty.bitmap_flags = FLAG_FILTER;
        let mut own = shader(&[0], false, vec![empty]);
        own.apply_reference_shader(&definition(&[1]), Some(&template));
        assert_eq!(own.parameters[0].bitmap_path, "objects\\template\\base", "a bitmap pointing nowhere takes the reference's");
    }

    /// `BLAM_TEST_HREK`: the Reach kit root, or its tags folder.
    #[test]
    fn spartan_armor_takes_the_parameters_its_template_has_and_it_lacks() {
        let Some(root) = std::env::var_os("BLAM_TEST_HREK").map(std::path::PathBuf::from) else {
            eprintln!("skipping: set BLAM_TEST_HREK");
            return;
        };
        let tags = if root.join("tags").is_dir() { root.join("tags") } else { root };
        let read = |path: &str| {
            let tag = crate::TagFile::read(tags.join(path)).unwrap_or_else(|error| panic!("{path}: {error}"));
            RenderMethod::from_tag(&tag).unwrap()
        };
        let mut armor = read("objects/characters/spartans/shaders/spartan_armor.shader");
        let template = read("objects/characters/spartans/shaders/templates/spartan_armor_template.shader");
        assert_eq!(
            armor.reference,
            Some((u32::from_be_bytes(*b"rmsh"), "objects\\characters\\spartans\\shaders\\templates\\spartan_armor_template".to_owned())),
        );
        assert!(template.is_template && !armor.is_template);
        let definition = crate::TagFile::read(tags.join("shaders/shader.render_method_definition")).unwrap();
        let definition = RenderMethodDefinition::from_tag(&definition).unwrap();
        let has = |method: &RenderMethod, name: &str| method.parameters.iter().any(|p| p.parameter_name == name);
        assert!(!has(&armor, "camouflage_change_color_map"));
        let own_options = armor.options.clone();
        armor.apply_reference_shader(&definition, Some(&template));
        assert!(has(&armor, "camouflage_change_color_map"));
        assert!(has(&armor, "detail_map"));
        assert_eq!(armor.options, own_options, "every option set, so none from the template (its own 9th is 1)");
        assert_ne!(template.options[9], armor.options[9]);
    }

    #[test]
    fn a_real_it_has_takes_nothing_from_the_reference() {
        let mut from = parameter("specular_power", RenderMethodParameterType::Real);
        from.real_parameter = 0.5;
        from.animated_parameters = vec![animation(RenderMethodAnimatedParameterType::Value, 1.0)];
        let template = shader(&[0], true, vec![from]);
        let mut own = shader(&[0], false, vec![parameter("specular_power", RenderMethodParameterType::Real)]);
        own.apply_reference_shader(&definition(&[1]), Some(&template));
        assert_eq!(own.parameters[0].real_parameter, 0.0);
        assert!(own.parameters[0].animated_parameters.is_empty());
    }
}
