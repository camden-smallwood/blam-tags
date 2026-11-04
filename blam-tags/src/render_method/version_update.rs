//! `c_render_method_shader::version_update` (MCC tool.exe 0x141188ED0; the
//! MCC client runs the same code, reach_tag_test `sub_1407F57A0`): Reach
//! brings a `shader` tag up to version 9 when it loads it, rewriting options
//! and parameters older tags authored differently. A reader that skips it
//! draws those tags as the game never does: version 6 and 7 Spartan helmets
//! keep `misc` option 1, `rotating_bitmaps_super_slow`, which the update
//! turns off, and every map on them turns by a radian.
//!
//! Each step falls through to the next, as the game's switch does. Only the
//! `shader` group has the update; no other render method class runs one.

use crate::tag_function::editor::{ExponentParams, FoundationMasterType, TagFunctionEditor};
use crate::tag_function::TagFunction;

use crate::typed_enums::Enum;

use super::types::{
    RenderMethod, RenderMethodAnimatedParameter, RenderMethodAnimatedParameterType,
    RenderMethodDefinition, RenderMethodParameter, RenderMethodParameterType,
};

/// The version every step leads to.
pub const SHADER_VERSION: u8 = 9;

/// `c_function_definition_editor::set_function_type(constant)` then
/// `set_constant(0, value)`: a constant function of `value`.
fn constant(value: f32) -> Option<TagFunction> {
    let mut bytes = [0u8; 32];
    bytes[0] = 1;
    bytes[4..8].copy_from_slice(&value.to_le_bytes());
    TagFunction::parse(&bytes).ok()
}

/// `c_function_definition::evaluate_scalar` with no inputs.
fn evaluate(function: &TagFunction) -> f32 {
    function.evaluate(0.0, 0.0)
}

/// `math:functions:rescale` (tool 0x140B5DFE0): the output range and both
/// constants times `by`.
fn rescaled(function: &TagFunction, by: f32) -> Option<TagFunction> {
    let mut bytes = function.to_bytes();
    for at in [4usize, 8, 20, 24] {
        let value = f32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
        bytes[at..at + 4].copy_from_slice(&(value * by).to_le_bytes());
    }
    TagFunction::parse(&bytes).ok()
}

/// `byte_141E412B0`: which of the four stored colors a color index means
/// under each color graph type.
const COLOR_SLOTS: [[usize; 4]; 5] = [[0, 0, 0, 0], [0, 0, 0, 0], [0, 3, 0, 0], [0, 1, 3, 0], [0, 1, 2, 3]];

fn color(function: &TagFunction, index: usize) -> Option<u32> {
    let bytes = function.to_bytes();
    let slot = COLOR_SLOTS.get(*bytes.get(2)? as usize)?[index];
    Some(u32::from_le_bytes(bytes.get(4 + 4 * slot..8 + 4 * slot)?.try_into().ok()?))
}

fn with_color(function: &TagFunction, index: usize, argb: u32) -> Option<TagFunction> {
    let mut bytes = function.to_bytes();
    let slot = COLOR_SLOTS.get(*bytes.get(2)? as usize)?[index];
    bytes.get_mut(4 + 4 * slot..8 + 4 * slot)?.copy_from_slice(&argb.to_le_bytes());
    TagFunction::parse(&bytes).ok()
}

fn with_color_graph_type(function: &TagFunction, kind: u8) -> Option<TagFunction> {
    let mut bytes = function.to_bytes();
    *bytes.get_mut(2)? = kind;
    TagFunction::parse(&bytes).ok()
}

fn function_type(function: &TagFunction) -> u8 {
    function.to_bytes().first().copied().unwrap_or(0)
}

/// `set_function_type(exponent)`, the function relaid as its editor does.
fn as_exponent(function: &TagFunction) -> Option<TagFunction> {
    let mut editor = TagFunctionEditor::from_function(function.clone());
    editor.set_master_type(FoundationMasterType::Exponent).ok()?;
    Some(editor.into_function())
}

fn with_exponent(function: &TagFunction, exponent: f32) -> Option<TagFunction> {
    let mut editor = TagFunctionEditor::from_function(function.clone());
    let params = editor.exponent_params(0)?;
    editor.set_exponent_params(0, ExponentParams { exponent: exponent.max(0.0), ..params }).ok()?;
    Some(editor.into_function())
}

fn kind_of(kind: RenderMethodParameterType) -> Option<Enum<RenderMethodParameterType, i32>> {
    Some(Enum::from_variant(kind))
}

fn animated(kind: RenderMethodAnimatedParameterType, function: Option<TagFunction>) -> RenderMethodAnimatedParameter {
    RenderMethodAnimatedParameter {
        parameter_type: Some(Enum::from_variant(kind)),
        input_name: String::new(),
        range_name: String::new(),
        time_period_in_seconds: 0.0,
        function,
    }
}

impl RenderMethod {
    /// `c_render_method::find_parameter_index`: by name and type.
    fn find(&self, name: &str, kind: RenderMethodParameterType) -> Option<usize> {
        self.parameters
            .iter()
            .position(|parameter| parameter.parameter_name == name && parameter.parameter_type.map(|t| t.get()) == Some(kind))
    }

    /// A parameter of `name` and `kind`, added empty if there is none.
    fn find_or_add(&mut self, name: &str, kind: RenderMethodParameterType) -> usize {
        self.find(name, kind).unwrap_or_else(|| {
            self.parameters.push(RenderMethodParameter {
                parameter_name: name.to_owned(),
                parameter_type: kind_of(kind),
                ..Default::default()
            });
            self.parameters.len() - 1
        })
    }

    /// `s_render_method_parameter::clone` of `from` into `name`.
    fn clone_parameter(&mut self, from: usize, name: &str, kind: RenderMethodParameterType) -> usize {
        let at = self.find_or_add(name, kind);
        let mut copy = self.parameters[from].clone();
        copy.parameter_name = name.to_owned();
        self.parameters[at] = copy;
        at
    }

    /// The first animated function of a parameter, made to exist.
    fn first_function(&mut self, at: usize) -> &mut RenderMethodAnimatedParameter {
        let parameter = &mut self.parameters[at];
        if parameter.animated_parameters.is_empty() {
            parameter.animated_parameters.push(animated(RenderMethodAnimatedParameterType::Value, None));
        }
        &mut parameter.animated_parameters[0]
    }

    /// `sub_141166E60`: whether a category is set to the option of that name.
    fn has_option(&self, definition: &RenderMethodDefinition, category: &str, option: &str) -> bool {
        let Some(index) = definition.categories.iter().position(|c| c.category_name == category) else {
            return false;
        };
        let wanted = definition.categories[index].options.iter().position(|o| o.option_name == option);
        let current = self.options.get(index).copied().unwrap_or(-1);
        wanted.is_some_and(|wanted| current >= 0 && wanted == current as usize)
    }

    /// `sub_141166D90`: set a category to the option of that name.
    fn set_option(&mut self, definition: &RenderMethodDefinition, category: &str, option: &str) {
        let Some(index) = definition.categories.iter().position(|c| c.category_name == category) else {
            return;
        };
        if let Some(wanted) = definition.categories[index].options.iter().position(|o| o.option_name == option) {
            if let Some(slot) = self.options.get_mut(index) {
                *slot = wanted as i16;
            }
        }
    }

    /// Bring a Reach `shader` tag's options and parameters up to
    /// [`SHADER_VERSION`] as the game does on load; `definition` is its
    /// render method definition. Nothing happens to one already current.
    pub fn version_update(&mut self, definition: &RenderMethodDefinition) {
        use RenderMethodParameterType::{Bitmap, Real};
        let mut version = self.version;
        if version == 0 {
            // `material_model` 7, as category 4 held it.
            if self.options.len() >= 5 && self.options[4] == 7 {
                let roughness = self.find("roughness", Real).map_or(1.0, |at| {
                    // `sub_141188930`: the first value function, evaluated.
                    let parameter = &self.parameters[at];
                    let value = parameter.animated_parameters.iter().find(|animated| {
                        animated.parameter_type.map(|t| t.get()) == Some(RenderMethodAnimatedParameterType::Value)
                    });
                    value.and_then(|animated| animated.function.as_ref()).map_or(0.0, evaluate)
                });
                if let Some(at) = self.find("analytical_specular_contribution", Real) {
                    let by = roughness * roughness * roughness * roughness * 9.869_605;
                    let parameter = &mut self.parameters[at];
                    parameter.real_parameter *= by;
                    if let Some(function) = parameter.animated_parameters.first_mut().and_then(|a| a.function.as_mut()) {
                        if let Some(scaled) = rescaled(function, by) {
                            *function = scaled;
                        }
                    }
                }
            }
            version = 1;
        }
        if version == 1 {
            if self.has_option(definition, "material_model", "single_lobe_phong") {
                self.set_option(definition, "material_model", "two_lobe_phong");
            }
            if self.has_option(definition, "material_model", "hair") {
                self.set_option(definition, "material_model", "none");
            }
            self.specular_color_by_angle(definition);
            self.powers_to_roughness();
            version = 2;
        }
        if version == 2 {
            self.split_specular_coefficient();
            if let (Some(scale), Some(roughness)) = (self.find("env_roughness_scale", Real), self.find("roughness", Real)) {
                let value = self.parameter_value(roughness);
                let scale = self.parameters[scale].real_parameter;
                let at = self.find_or_add("env_roughness_offset", Real);
                self.parameters[at].real_parameter = (value * scale - value) * 0.5 + 0.5;
            }
            version = 3;
        }
        if version == 3 {
            if self.find("analytical_roughness", Real).is_none() {
                if let Some(roughness) = self.find("roughness", Real) {
                    let value = self.parameter_value(roughness);
                    let at = self.find_or_add("analytical_roughness", Real);
                    self.parameters[at].real_parameter = value;
                    self.parameters[at].animated_parameters =
                        vec![animated(RenderMethodAnimatedParameterType::Value, constant(value))];
                }
            }
            version = 4;
        }
        if version == 4 {
            if let Some(at) = self.find("area_specular_contribution", Real) {
                let pi = std::f32::consts::PI;
                self.parameters[at].real_parameter /= pi;
                // The game evaluates whatever function sits first; one the
                // tag lacks is taken as the stored value [inferred].
                let existing = self.parameters[at].animated_parameters.first().and_then(|a| a.function.clone());
                let value = existing.as_ref().map_or(self.parameters[at].real_parameter * pi, evaluate);
                let slot = self.first_function(at);
                slot.parameter_type = Some(Enum::from_variant(RenderMethodAnimatedParameterType::Value));
                slot.function = constant(value / pi);
                self.parameters[at].animated_parameters.truncate(1);
            }
            version = 5;
        }
        if version == 5 {
            // `sub_141188D40`: a bitmap's sharpen mode, under flag 0x20, moves
            // up past the eight modes inserted ahead of it.
            for parameter in &mut self.parameters {
                if parameter.parameter_type.map(|t| t.get()) == Some(Bitmap) && parameter.bitmap_flags & 0x20 != 0 {
                    parameter.bitmap_sharpen_mode += 8;
                }
            }
            version = 6;
        }
        if version == 6 || version == 7 {
            // `misc`: the old option 3 is now 1; any other set option is the
            // default, so old `rotating_bitmaps_super_slow` (1) turns off.
            if let Some(index) = definition.categories.iter().position(|c| c.category_name == "misc") {
                if let Some(option) = self.options.get_mut(index) {
                    if *option == 3 {
                        *option = 1;
                    } else if *option != -1 {
                        *option = 0;
                    }
                }
            }
            version = 8;
        }
        if version == 8 {
            // `fix_real_paramter_animation_block`.
            for name in ["analytical_roughness", "env_roughness_offset"] {
                if let Some(at) = self.find(name, Real) {
                    if self.parameters[at].animated_parameters.is_empty() {
                        let value = self.parameters[at].real_parameter;
                        self.parameters[at].animated_parameters =
                            vec![animated(RenderMethodAnimatedParameterType::Value, constant(value))];
                    }
                }
            }
            version = SHADER_VERSION;
        }
        self.version = version;
    }

    /// A real parameter's value: its stored real, or its first function when
    /// it has one (the game asserts the real is 0 then).
    fn parameter_value(&self, at: usize) -> f32 {
        let parameter = &self.parameters[at];
        match parameter.animated_parameters.first().and_then(|a| a.function.as_ref()) {
            Some(function) => evaluate(function),
            None => parameter.real_parameter,
        }
    }

    /// `sub_1411880B0`: `specular_coefficient` folded into the three
    /// contributions, each scaled by it, and left at 1.
    fn split_specular_coefficient(&mut self) {
        use RenderMethodParameterType::Real;
        let Some(coefficient) = self.find("specular_coefficient", Real) else {
            return;
        };
        let Some(scale) = self.parameters[coefficient]
            .animated_parameters
            .first()
            .and_then(|a| a.function.as_ref())
            .map(evaluate)
        else {
            return;
        };
        let parts = [
            ("analytical_specular_contribution", 0.5),
            ("area_specular_contribution", 0.5),
            ("environment_map_specular_contribution", 0.0),
        ];
        for (name, default) in parts {
            let at = match self.find(name, Real) {
                Some(at) => at,
                None => {
                    let at = self.clone_parameter(coefficient, name, Real);
                    if let Some(first) = self.parameters[at].animated_parameters.first_mut() {
                        first.parameter_type = Some(Enum::from_variant(RenderMethodAnimatedParameterType::Value));
                        first.function = constant(default);
                    }
                    at
                }
            };
            if let Some(function) = self.parameters[at].animated_parameters.first_mut().and_then(|a| a.function.as_mut()) {
                if let Some(scaled) = rescaled(function, scale) {
                    *function = scaled;
                }
            }
        }
        if let Some(first) = self.parameters[coefficient].animated_parameters.first_mut() {
            first.function = constant(1.0);
        }
    }

    /// `sub_1411873D0` (direction 0): specular powers become roughnesses,
    /// `pow(power, -0.4551) * 0.553773` clamped to [0, 1].
    fn powers_to_roughness(&mut self) {
        use RenderMethodParameterType::Real;
        let power = self.find("normal_specular_power", Real).or_else(|| self.find("specular_power", Real));
        let pairs = [(power, "roughness"), (self.find("glancing_specular_power", Real), "glancing_roughness")];
        for (from, name) in pairs {
            let Some(from) = from else {
                continue;
            };
            let at = self.clone_parameter(from, name, Real);
            let Some(first) = self.parameters[at].animated_parameters.first_mut() else {
                continue;
            };
            let value = first.function.as_ref().map_or(0.0, evaluate);
            let value = if value.max(0.01) >= 2000.0 { 2000.0 } else if value <= 0.01 { 0.01 } else { value };
            let roughness = (value.powf(-0.4551) * 0.553_773).clamp(0.0, 1.0);
            first.function = constant(roughness);
        }
    }

    /// `sub_1411876A0` (direction 0): the two tints and the fresnel
    /// steepness gathered into `specular_color_by_angle`, a two-color
    /// exponent function.
    fn specular_color_by_angle(&mut self, definition: &RenderMethodDefinition) {
        use RenderMethodParameterType::{Color, Real};
        let names = if self.has_option(definition, "material_model", "two_lobe_phong") {
            Some(["normal_specular_tint", "glancing_specular_tint"])
        } else if self.has_option(definition, "material_model", "cook_torrance")
            || self.has_option(definition, "material_model", "organism")
        {
            Some(["specular_tint", "fresnel_color"])
        } else {
            None
        };
        let Some(names) = names else {
            return;
        };
        let sources = [self.find(names[0], Color), self.find(names[1], Color), self.find("fresnel_curve_steepness", Real)];
        if sources.iter().all(Option::is_none) {
            return;
        }
        let target = match self.find("specular_color_by_angle", Color) {
            Some(at) => at,
            None => {
                let at = self.find_or_add("specular_color_by_angle", Color);
                let mut bytes = [0u8; 32];
                bytes[0] = 1;
                let seed = TagFunction::parse(&bytes).ok().and_then(|f| as_exponent(&f));
                let seed = seed.and_then(|f| with_color_graph_type(&f, 2));
                let seed = (0..4).try_fold(seed, |f, index| Some(f.and_then(|f| with_color(&f, index, 0xFFFF_FFFF))));
                self.parameters[at].animated_parameters =
                    vec![animated(RenderMethodAnimatedParameterType::Color, seed.flatten())];
                at
            }
        };
        let Some(mut function) = self.parameters[target].animated_parameters.first().and_then(|a| a.function.clone()) else {
            return;
        };
        let kind = function_type(&function);
        if kind != 9 && kind != 0 {
            function = as_exponent(&function).unwrap_or(function);
        }
        for (index, source) in sources.into_iter().enumerate() {
            let Some(source) = source else {
                continue;
            };
            match index {
                0 | 1 => {
                    function = with_color_graph_type(&function, 2).unwrap_or(function);
                    let color = self.first_function(source).function.as_ref().and_then(|f| color(f, 0));
                    if let Some(argb) = color {
                        function = with_color(&function, index, argb).unwrap_or(function);
                    }
                }
                _ => {
                    let parameter = &self.parameters[source];
                    let exponent = parameter
                        .animated_parameters
                        .first()
                        .and_then(|a| a.function.as_ref())
                        .map_or(parameter.real_parameter, evaluate);
                    function = with_exponent(&function, exponent).unwrap_or(function);
                }
            }
        }
        if let Some(first) = self.parameters[target].animated_parameters.first_mut() {
            first.function = Some(function);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_constant_evaluates_to_its_value() {
        assert!((evaluate(&constant(0.25).unwrap()) - 0.25).abs() < 1e-6);
        assert!((evaluate(&rescaled(&constant(0.25).unwrap(), 4.0).unwrap()) - 1.0).abs() < 1e-6);
    }
}
