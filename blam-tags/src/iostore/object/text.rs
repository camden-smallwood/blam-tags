//! `FText` and its format arguments.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::archive::Reader;
use super::common::native_count;
use super::value::{BlockLayout, PropValue};

/// The payload struct a universal-object-locator fragment type serializes, by
/// its registered `FName`. An empty name means the fragment carries no payload.
///
/// `subobj` is the only type this build's content uses — swept across all 121
/// `LevelSequence` packages. Anything else surfaces as an error naming the
/// unmapped type rather than silently mis-consuming the stream.
pub(super) fn locator_fragment_payload(fragment_type: &str) -> Option<&'static str> {
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
    let start = r.o;
    let v = read_text_inner(r, depth)?;
    Ok(match v {
        PropValue::Struct(mut b) => {
            b.layout = BlockLayout::Native { name: Arc::from("Text"), bytes: r.since(start) };
            PropValue::Struct(b)
        }
        other => other,
    })
}

/// `FText::Serialize` is hand-written, so like the structs in
/// [`super::structs`] the block it decodes to carries its own bytes.
fn read_text_inner(r: &mut Reader, depth: usize) -> Result<PropValue> {
    if depth > 16 {
        bail!("FText nesting too deep @ {}", r.o);
    }
    let mut s = BTreeMap::new();
    s.insert("Flags".to_string(), PropValue::Int(r.u32()? as i64));
    let history = r.u8()? as i8;
    s.insert("HistoryType".to_string(), PropValue::Int(history as i64));
    match history {
        // `ETextHistoryType::None` still writes a four-byte
        // `bHasCultureInvariantString` (and the string itself when set).
        // Measured on `NS_collision`: skipping it left the emitter four bytes
        // adrift, and consuming it lands `FixedBounds` exactly on a ±1000 box
        // with an empty event-handler array right after.
        -1 => {
            if r.u32()? != 0 {
                s.insert("CultureInvariantString".to_string(), PropValue::Str(r.fstring()?));
            }
        }
        // `Base`: namespace, key and source string.
        0 => {
            s.insert("Namespace".to_string(), PropValue::Str(r.fstring()?));
            s.insert("Key".to_string(), PropValue::Str(r.fstring()?));
            s.insert("SourceString".to_string(), PropValue::Str(r.fstring()?));
        }
        // `StringTableEntry`: the table id and the row key.
        11 => {
            s.insert("TableId".to_string(), PropValue::Name(r.fname()?));
            s.insert("Key".to_string(), PropValue::Str(r.fstring()?));
        }
        // `OrderedFormat`: the source format text, then **positional**
        // arguments — bare values, with none of the names `ArgumentDataFormat`
        // carries. (`FTextHistory_Generated`, which both derive from, writes
        // nothing itself.)
        2 => {
            s.insert("SourceFmt".to_string(), read_text(r, depth + 1)?);
            let n = native_count(r, "FText ordered arguments")?;
            let mut args = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                args.push(read_format_argument(r, depth + 1)?);
            }
            s.insert("Arguments".to_string(), PropValue::Array(args));
        }
        // `NamedFormat` (a `TMap<FString, FFormatArgumentValue>`) and
        // `ArgumentDataFormat` (a `TArray<FFormatArgumentData>`) both come out
        // as a count followed by name/value pairs. Only the latter has been
        // seen in Campaign Evolved.
        1 | 3 => {
            s.insert("SourceFmt".to_string(), read_text(r, depth + 1)?);
            let n = native_count(r, "FText arguments")?;
            let mut args = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                let mut a = BTreeMap::new();
                a.insert("ArgumentName".to_string(), PropValue::Str(r.fstring()?));
                a.insert("ArgumentValue".to_string(), read_format_argument(r, depth + 1)?);
                args.push(PropValue::Struct(a.into()));
            }
            s.insert("Arguments".to_string(), PropValue::Array(args));
        }
        // `AsNumber`/`AsPercent`/`AsCurrency`: the source value, optional
        // number-formatting options, and the target culture. `AsCurrency` leads
        // with the currency code.
        4 | 5 | 6 => {
            if history == 6 {
                s.insert("CurrencyCode".to_string(), PropValue::Str(r.fstring()?));
            }
            s.insert("SourceValue".to_string(), read_format_argument(r, depth + 1)?);
            if r.u32()? != 0 {
                // `FNumberFormattingOptions`: three `FArchive` bools, a rounding
                // mode, then four digit counts.
                let mut o = BTreeMap::new();
                o.insert("AlwaysSign".to_string(), PropValue::Bool(r.u32()? != 0));
                o.insert("UseGrouping".to_string(), PropValue::Bool(r.u32()? != 0));
                o.insert("RoundingMode".to_string(), PropValue::Int(r.u8()? as i64));
                for f in [
                    "MinimumIntegralDigits",
                    "MaximumIntegralDigits",
                    "MinimumFractionalDigits",
                    "MaximumFractionalDigits",
                ] {
                    o.insert(f.to_string(), PropValue::Int(r.i32()? as i64));
                }
                s.insert("FormatOptions".to_string(), PropValue::Struct(o.into()));
            }
            s.insert("TargetCulture".to_string(), PropValue::Str(r.fstring()?));
        }
        other => bail!("FText history type {other} not modeled (@ {})", r.o - 1),
    }
    Ok(PropValue::Struct(s.into()))
}

/// `FFormatArgumentValue`: an `EFormatArgumentType` tag then the value.
pub(super) fn read_format_argument(r: &mut Reader, depth: usize) -> Result<PropValue> {
    let ty = r.u8()? as i8;
    Ok(match ty {
        0 => PropValue::Int(r.u64()? as i64), // Int (64-bit in this stream version)
        1 => PropValue::Int(r.u64()? as i64), // UInt
        2 => PropValue::Float(r.f32()? as f64),
        3 => PropValue::Float(r.f64()?),
        4 => read_text(r, depth)?,
        other => bail!("FText format argument type {other} not modeled (@ {})", r.o - 1),
    })
}
