//! Which format versions this codec is measured against, and refusing the rest.
//!
//! Every size in [`native_struct_size`](super::object::structs::native_struct_size),
//! every arm of the class-tail dispatcher and every serialization rule here was
//! measured against one build: Halo: Campaign Evolved on UE 5.5.4. None of it is
//! version-negotiated. A different build can change a struct's cooked size
//! without changing anything this reader inspects, and the failure mode is a
//! desynced walk that produces plausible values — so the version has to be
//! checked rather than assumed.
//!
//! # Campaign Evolved packages carry no engine version
//!
//! Measured across all 103,867 packages: every one has `has_versioning_info = 0`.
//! The versioning block is simply absent, so
//! [`heuristic_zen_package_version`](super::package::zen) *derives* the engine
//! version from the container's TOC version and header version instead. That
//! derivation is the thing to gate on, because there is nothing else.
//!
//! # The write side is where this actually bites
//!
//! Reading an unexpected version fails loudly — the heuristic returns an error
//! for combinations it does not recognise. Writing one does not fail at all: it
//! produces a container the game rejects in silence. `HaloCampaignEvolved.exe`'s
//! `operator<<(FArchive&, FIoContainerHeader&)` checks `version == 4` on load and
//! sets the archive error flag otherwise, with no diagnostic. And
//! [`EIoContainerHeaderVersion`]'s `Default` is `SoftPackageReferencesOffset`
//! (5, UE 5.6) — so a writer that ever falls back to it emits something the game
//! will not load.

use anyhow::{bail, Result};

use super::container::header::EIoContainerHeaderVersion;
use super::package::ue_types::EIoStoreTocVersion;
use super::package::zen::{EUnrealEngineObjectUE5Version, FPackageFileVersion};

/// The `.utoc` version Campaign Evolved ships (UE 5.5).
pub const CE_TOC_VERSION: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;

/// The container header version Campaign Evolved ships — and the only one its
/// executable will load.
///
/// Confirmed in `HaloCampaignEvolved.exe` at `sub_143614290`, reached from the
/// container-header signature constant `0x496F436E`: on load it compares the
/// version against 4 and flags the archive as errored otherwise.
pub const CE_CONTAINER_HEADER_VERSION: EIoContainerHeaderVersion =
    EIoContainerHeaderVersion::SoftPackageReferences;

/// The UE5 object version the decode tables were measured against.
///
/// Derived, not read: Campaign Evolved's packages are unversioned, so this is
/// what `heuristic_zen_package_version` produces for
/// (`CE_TOC_VERSION`, `CE_CONTAINER_HEADER_VERSION`) — value 1013, and the same
/// for all 103,867 packages.
pub const MEASURED_UE5_VERSION: EUnrealEngineObjectUE5Version =
    EUnrealEngineObjectUE5Version::AssetRegistryPackageBuildDependencies;

/// Refuse to write a container the target build will not load.
///
/// This is a *writer* guard on purpose. The game gives no diagnostic for a
/// version it rejects, so the only place the mistake can be caught is here.
pub fn check_writable_container_header_version(version: EIoContainerHeaderVersion) -> Result<()> {
    if version != CE_CONTAINER_HEADER_VERSION {
        bail!(
            "container header version {version:?} cannot be written for this target: the \
             executable accepts only {CE_CONTAINER_HEADER_VERSION:?} and rejects anything else \
             without a diagnostic"
        );
    }
    Ok(())
}

/// Whether the decode tables were measured against this package's engine
/// version.
pub fn is_measured_package_version(version: FPackageFileVersion) -> bool {
    version.file_version_ue5 == MEASURED_UE5_VERSION as i32
}

/// Refuse to decode a package from a build the tables were not measured
/// against.
///
/// Deliberately not called on the read path by default — see
/// [`check_decodable_package_version`]'s note — but available to a caller that
/// would rather fail than decode plausible nonsense.
pub fn check_decodable_package_version(version: FPackageFileVersion) -> Result<()> {
    if !is_measured_package_version(version) {
        bail!(
            "package reports UE5 object version {} but the struct-size and tail tables were \
             measured against {} ({MEASURED_UE5_VERSION:?}); sizes may differ and a wrong one \
             desyncs the walk into plausible-looking values",
            version.file_version_ue5,
            MEASURED_UE5_VERSION as i32
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exe compares against 4. If this constant ever moves, the containers
    /// this crate writes stop loading — silently.
    #[test]
    fn the_container_header_version_is_the_one_the_exe_accepts() {
        assert_eq!(CE_CONTAINER_HEADER_VERSION as i32, 4);
        assert_eq!(MEASURED_UE5_VERSION as i32, 1013);
    }

    /// `EIoContainerHeaderVersion::default()` is 5 (UE 5.6), which the target
    /// build rejects — so the guard has to catch the default, not just exotic
    /// values.
    #[test]
    fn the_enum_default_is_refused() {
        let d = EIoContainerHeaderVersion::default();
        assert_ne!(d, CE_CONTAINER_HEADER_VERSION, "default is not the shipping version");
        assert!(check_writable_container_header_version(d).is_err());
        assert!(check_writable_container_header_version(CE_CONTAINER_HEADER_VERSION).is_ok());
    }

    #[test]
    fn a_different_engine_version_is_refused() {
        let ok = FPackageFileVersion::create_ue5(MEASURED_UE5_VERSION);
        assert!(check_decodable_package_version(ok).is_ok());
        let other =
            FPackageFileVersion::create_ue5(EUnrealEngineObjectUE5Version::DataResources);
        assert!(check_decodable_package_version(other).is_err());
    }
}
