//! Where `FText` is reached from, and the locator-fragment payload table.
//!
//! `FText`'s own layout is hand-written rather than schema-driven, so the model
//! lives with the other such shapes in [`super::hand_written`]. This is the
//! entry point the property reader calls.

use anyhow::Result;

use super::archive::Reader;
use super::hand_written::{HandWritten, TextValue};
use super::value::PropValue;

/// The payload struct a universal-object-locator fragment type serializes, by
/// its registered `FName`. An empty name means the fragment carries no payload.
///
/// `subobj` is the only type this build's content uses — swept across all 121
/// `LevelSequence` packages. Anything else surfaces as an error naming the
/// unmapped type rather than silently mis-consuming the stream.
pub(crate) fn locator_fragment_payload(fragment_type: &str) -> Option<&'static str> {
    Some(match fragment_type {
        "subobj" => "SubObjectLocator",
        "actor" => "ActorLocatorFragment",
        _ => return None,
    })
}

/// `FText`: `uint32 Flags`, an `int8` history type, then that history's own
/// payload.
///
/// Derived from `DA_VideoHDRSettingsItems`, where the whole 65-byte export
/// resolves exactly: `00 00 00 00` (Flags), `0b` (history type 11 =
/// `StringTableEntry`), the table `FName`, and the 31-byte key
/// `"settings_header_controlpresets"`. Reading the fields in the other order
/// would make Flags `0x0b000000`, which is how the order was settled.
///
/// Unmodeled history types surface as an error naming the type number rather
/// than silently mis-consuming the stream.
pub(super) fn read_text(r: &mut Reader, depth: usize) -> Result<PropValue> {
    Ok(PropValue::HandWritten(HandWritten::Text(TextValue::read(r, depth)?)))
}
