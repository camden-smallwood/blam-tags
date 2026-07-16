//! Halo scenario tag (`scnr`) walker.
//!
//! Mirrors the rendering-relevant subset of Bungie's `struct scenario`
//! (Ares `source/scenario/scenario_definitions.h`). Field names follow
//! the **MCC tag schema** (with spaces) since that's the authoritative
//! source for parsing — Ares is older and field offsets/names have
//! drifted across MCC builds.
//!
//! See [`Scenario`] for the entry point.

mod types;

pub use types::{
    CubemapEntry, DecalPaletteEntry, DecalPlacement, DecoratorPalette, DecoratorSetEntry,
    DevicePortalAssociation, GamePortalToPortalMapping, ObjectName, ObjectPlacement, ParentId,
    PlacementMultiplayerData, PlacementObjectData, PlacementPermutationData, Scenario,
    ScenarioDecoratorBlock, ScenarioDecoratorPlacement, ScenarioError, ScenarioLightDatum,
    ScenarioLightFlags, ScenarioLightLightmapType, SkyReference,
    StructureBspReference, TagReferencePalette, ZoneSet, ZoneSetBspPvs, ZoneSetClusterPvs,
    ZoneSetPortalDeviceMapping, ZoneSetPvs,
};
