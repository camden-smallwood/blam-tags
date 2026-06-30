//! The `structure_bsp` audio subsystem: the acoustics palette
//! (reverb + ambience), the legacy background-sound / sound-environment
//! palettes, the per-type sound clusters, and the audibility
//! (encoded-PAS) block. Mirrors the MCC schema 1:1.

use crate::api::TagStruct;
use crate::math::RealBounds;

use super::common::{read_block, read_struct};

// --- acoustics palette ------------------------------------------------------

/// `scenario_acoustics_palette_block` — a named reverb+ambience slot.
#[derive(Debug, Clone, Default)]
pub struct AcousticsPalette {
    pub name: String,
    pub reverb: AcousticsEnvironment,
    pub ambience: AcousticsAmbience,
}

/// `scenario_acoustics_environment_definition`.
#[derive(Debug, Clone, Default)]
pub struct AcousticsEnvironment {
    pub sound_environment: String,
    pub cutoff_distance: f32,
    pub interpolation_time: f32,
}

/// `scenario_acoustics_ambience_definition`.
#[derive(Debug, Clone, Default)]
pub struct AcousticsAmbience {
    pub background_sound: String,
    pub inside_cluster_sound: String,
    pub cutoff_distance: f32,
    pub scale_flags: u32,
    pub interior_scale: f32,
    pub portal_scale: f32,
    pub exterior_scale: f32,
    pub interpolation_time: f32,
}

impl AcousticsEnvironment {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sound_environment: s.read_tag_ref_path("sound environment").unwrap_or_default(),
            cutoff_distance: s.read_real("cutoff distance").unwrap_or(0.0),
            interpolation_time: s.read_real("interpolation time").unwrap_or(0.0),
        }
    }
}

impl AcousticsAmbience {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            background_sound: s.read_tag_ref_path("background sound").unwrap_or_default(),
            inside_cluster_sound: s.read_tag_ref_path("inside cluster sound").unwrap_or_default(),
            cutoff_distance: s.read_real("cutoff distance").unwrap_or(0.0),
            scale_flags: s.read_int_any("scale flags").unwrap_or(0) as u32,
            interior_scale: s.read_real("interior scale").unwrap_or(0.0),
            portal_scale: s.read_real("portal scale").unwrap_or(0.0),
            exterior_scale: s.read_real("exterior scale").unwrap_or(0.0),
            interpolation_time: s.read_real("interpolation time").unwrap_or(0.0),
        }
    }
}

impl AcousticsPalette {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            reverb: read_struct(s, "reverb", AcousticsEnvironment::from_struct),
            ambience: read_struct(s, "ambience", AcousticsAmbience::from_struct),
        }
    }
}

// --- legacy background-sound / sound-environment palettes -------------------

/// `scenario_acoustics_ambience_palette_block` — legacy per-string-name
/// background-sound palette.
#[derive(Debug, Clone, Default)]
pub struct BackgroundSoundPalette {
    pub name: String,
    pub background_sound: String,
    pub inside_cluster_sound: String,
    pub cutoff_distance: f32,
    pub scale_flags: u32,
    pub interior_scale: f32,
    pub portal_scale: f32,
    pub exterior_scale: f32,
    pub interpolation_speed: f32,
}

impl BackgroundSoundPalette {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string("name").unwrap_or_default(),
            background_sound: s.read_tag_ref_path("background sound").unwrap_or_default(),
            inside_cluster_sound: s.read_tag_ref_path("inside cluster sound").unwrap_or_default(),
            cutoff_distance: s.read_real("cutoff distance").unwrap_or(0.0),
            scale_flags: s.read_int_any("scale flags").unwrap_or(0) as u32,
            interior_scale: s.read_real("interior scale").unwrap_or(0.0),
            portal_scale: s.read_real("portal scale").unwrap_or(0.0),
            exterior_scale: s.read_real("exterior scale").unwrap_or(0.0),
            interpolation_speed: s.read_real("interpolation speed").unwrap_or(0.0),
        }
    }
}

/// `scenario_acoustics_environment_palette_block` — legacy sound-environment palette.
#[derive(Debug, Clone, Default)]
pub struct SoundEnvironmentPalette {
    pub name: String,
    pub sound_environment: String,
    pub cutoff_distance: f32,
    pub interpolation_speed: f32,
}

impl SoundEnvironmentPalette {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string("name").unwrap_or_default(),
            sound_environment: s.read_tag_ref_path("sound environment").unwrap_or_default(),
            cutoff_distance: s.read_real("cutoff distance").unwrap_or(0.0),
            interpolation_speed: s.read_real("interpolation speed").unwrap_or(0.0),
        }
    }
}

// --- sound clusters (acoustics / ambience / reverb) ------------------------

/// `structure_bsp_sound_cluster_block` — a palette-indexed sound cluster
/// bounded by enclosing portals + interior clusters. Used for all three
/// of the acoustics/ambience/reverb cluster lists.
#[derive(Debug, Clone, Default)]
pub struct SoundCluster {
    pub palette_index: i16,
    /// `enclosing portal designators`.
    pub enclosing_portal_designators: Vec<i16>,
    /// `interior cluster indices`.
    pub interior_cluster_indices: Vec<i16>,
}

impl SoundCluster {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            palette_index: s.read_int_any("palette index").unwrap_or(-1) as i16,
            enclosing_portal_designators: read_block(s, "enclosing portal designators", |e| {
                e.read_int_any("portal designator").unwrap_or(-1) as i16
            }),
            interior_cluster_indices: read_block(s, "interior cluster indices", |e| {
                e.read_int_any("interior cluster index").unwrap_or(-1) as i16
            }),
        }
    }
}

// --- audibility -------------------------------------------------------------

/// `structure_bsp_audibility_block` — the precomputed door/cluster
/// audibility (encoded potentially-audible-set) data.
#[derive(Debug, Clone, Default)]
pub struct Audibility {
    pub door_portal_count: i32,
    pub cluster_distance_bounds: RealBounds,
    /// `encoded door pas` — packed door PAS words.
    pub encoded_door_pas: Vec<i32>,
    /// `cluster door portal encoded pas`.
    pub cluster_door_portal_encoded_pas: Vec<i32>,
    /// `ai deafening pas`.
    pub ai_deafening_pas: Vec<i32>,
    /// `cluster distances` — packed `char` distances.
    pub cluster_distances: Vec<i8>,
    /// `machine door mapping` — occluder→machine-door indices.
    pub machine_door_mapping: Vec<i8>,
}

impl Audibility {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            door_portal_count: s.read_int_any("door portal count").unwrap_or(0) as i32,
            cluster_distance_bounds: s.read_real_bounds("cluster distance bounds"),
            encoded_door_pas: read_block(s, "encoded door pas", |e| {
                e.read_int_any("encoded data").unwrap_or(0) as i32
            }),
            cluster_door_portal_encoded_pas: read_block(
                s,
                "cluster door portal encoded pas",
                |e| e.read_int_any("encoded data").unwrap_or(0) as i32,
            ),
            ai_deafening_pas: read_block(s, "ai deafening pas", |e| {
                e.read_int_any("encoded data").unwrap_or(0) as i32
            }),
            cluster_distances: read_block(s, "cluster distances", |e| {
                e.read_int_any("encoded data").unwrap_or(0) as i8
            }),
            machine_door_mapping: read_block(s, "machine door mapping", |e| {
                e.read_int_any("machine door index").unwrap_or(-1) as i8
            }),
        }
    }
}
