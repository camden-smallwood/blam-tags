//! Reach's `shader` version update on the kit's own tags. `BLAM_TEST_HREK`
//! to the kit's root.

mod common;

use blam_tags::render_method::{
    RenderMethod, RenderMethodDefinition, RenderMethodParameterType, SHADER_VERSION,
};
use blam_tags::TagFile;

struct Kit {
    tags: std::path::PathBuf,
    definition: RenderMethodDefinition,
}

impl Kit {
    fn open() -> Option<Self> {
        let kit = common::kit("BLAM_TEST_HREK")?;
        let tags = kit.tags();
        let definition = TagFile::read(tags.join("shaders/shader.render_method_definition")).unwrap();
        Some(Self { definition: RenderMethodDefinition::from_tag(&definition).unwrap(), tags })
    }

    fn shader(&self, path: &str) -> RenderMethod {
        RenderMethod::from_tag(&TagFile::read(self.tags.join(path)).unwrap()).unwrap()
    }

    fn misc(&self, shader: &RenderMethod) -> i16 {
        let index = self.definition.categories.iter().position(|c| c.category_name == "misc").unwrap();
        shader.options[index]
    }
}

fn value(shader: &RenderMethod, name: &str) -> Option<f32> {
    let parameter = shader.parameters.iter().find(|p| {
        p.parameter_name == name && p.parameter_type.map(|t| t.get()) == Some(RenderMethodParameterType::Real)
    })?;
    Some(match parameter.animated_parameters.first().and_then(|a| a.function.as_ref()) {
        Some(function) => function.evaluate(0.0, 0.0),
        None => parameter.real_parameter,
    })
}

/// The Spartan helmets and their template are version 6 with `misc` 1,
/// `rotating_bitmaps_super_slow`, which the update turns off; the Scorpion's
/// wheels are version 9, rotating on purpose, and keep it.
#[test]
fn old_misc_options_turn_off_and_current_ones_stay() {
    let Some(kit) = Kit::open() else {
        eprintln!("skipping: BLAM_TEST_HREK is not set");
        return;
    };
    for path in [
        "objects/characters/spartans/shaders/spartan_helmets/spartan_helmet_default.shader",
        "objects/characters/spartans/shaders/templates/spartan_armor_template.shader",
    ] {
        let mut shader = kit.shader(path);
        assert_eq!((shader.version, kit.misc(&shader)), (6, 1), "{path} as authored");
        shader.version_update(&kit.definition);
        assert_eq!((shader.version, kit.misc(&shader)), (SHADER_VERSION, 0), "{path} updated");
    }
    let mut wheels = kit.shader("objects/vehicles/human/scorpion/shaders/scorpion_wheels_left.shader");
    assert_eq!((wheels.version, kit.misc(&wheels)), (9, 1));
    let before = format!("{:?}", wheels.options);
    wheels.version_update(&kit.definition);
    assert_eq!(format!("{:?}", wheels.options), before, "a current tag is left alone");
}

/// Version 2: `specular_coefficient` is folded into the three contributions
/// and left at 1.
#[test]
fn the_specular_coefficient_moves_into_the_contributions() {
    let Some(kit) = Kit::open() else {
        return;
    };
    let path = "levels/reference/lighting_reference/shaders/light_cool.shader";
    let mut shader = kit.shader(path);
    assert_eq!(shader.version, 2);
    let coefficient = value(&shader, "specular_coefficient").expect("a coefficient to fold");
    let before: Vec<_> = ["analytical_specular_contribution", "area_specular_contribution", "environment_map_specular_contribution"]
        .iter()
        .map(|name| value(&shader, name))
        .collect();
    let area_before = value(&shader, "area_specular_contribution");
    shader.version_update(&kit.definition);
    assert_eq!(shader.version, SHADER_VERSION);
    assert!((value(&shader, "specular_coefficient").unwrap() - 1.0).abs() < 1e-5);
    let after = value(&shader, "analytical_specular_contribution").unwrap();
    let expected = before[0].unwrap_or(0.5) * coefficient;
    assert!((after - expected).abs() < 1e-4, "analytical {after} vs {expected}");
    // Area then loses a factor of pi at version 4.
    let area = value(&shader, "area_specular_contribution").unwrap();
    let expected = area_before.unwrap_or(0.5) * coefficient / std::f32::consts::PI;
    assert!((area - expected).abs() < 1e-4, "area {area} vs {expected}");
    assert!(value(&shader, "environment_map_specular_contribution").is_some());
}

/// Version 4: the area specular contribution is divided by pi.
#[test]
fn area_specular_loses_pi() {
    let Some(kit) = Kit::open() else {
        return;
    };
    let mut shader = kit.shader("levels/reference/material_samples/shaders/building/brushed_aluminum_2lobe.shader");
    assert_eq!(shader.version, 4);
    let before = value(&shader, "area_specular_contribution").unwrap();
    shader.version_update(&kit.definition);
    let after = value(&shader, "area_specular_contribution").unwrap();
    assert!((after - before / std::f32::consts::PI).abs() < 1e-5, "{before} -> {after}");
    assert!(value(&shader, "analytical_roughness").is_some(), "version 3 adds it from roughness when there is one");
}

/// Every step from version 0 runs on the kit's oldest shader.
#[test]
fn the_oldest_shader_comes_up_to_date() {
    let Some(kit) = Kit::open() else {
        return;
    };
    let mut shader = kit.shader("objects/characters/engineer/shaders/engineer_eyes.shader");
    assert_eq!(shader.version, 0);
    shader.version_update(&kit.definition);
    assert_eq!(shader.version, SHADER_VERSION);
}
