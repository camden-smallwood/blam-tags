//! A whole export: the property block, `UObject`'s trailer, and each class in
//! the inheritance chain's natively serialized tail.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::archive::{tail_why, ExportContext, Reader};
use super::block::read_struct;
use super::tails::{read_class_native_tail, read_rig_hierarchy, read_rigvm};
use super::usmap::Usmap;
use super::value::PropValue;

/// Decode a cooked object export's unversioned property block for a known
/// native `class` (present in the `.usmap`), returning present property
/// name→value. General entry point for simple UObject exports (e.g.
/// `SkeletalMeshSocket`) whose serial data is just their reflected properties.
pub fn read_export_struct(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<BTreeMap<String, PropValue>> {
    read_export_struct_len(export, names, usmap, class).map(|(props, _)| props)
}

/// As [`read_export_struct`], but also returning how many bytes the property
/// block consumed.
///
/// This is the corpus gate for this reader. "Decoded without error" is a weak
/// claim — a desynced walk keeps reading plausible values and only trips much
/// later, or not at all. For a class whose export is *nothing but* its property
/// block, `consumed == export.len()` (modulo zero padding) is a real check that
/// every byte was accounted for; for classes with a native tail (a mesh's render
/// data, a texture's platform data) it says where that tail begins.
pub fn read_export_struct_len(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<(BTreeMap<String, PropValue>, usize)> {
    let mut r = Reader::new(export, names);
    let props = read_struct(&mut r, class, usmap, 0)?;
    Ok((props, r.o))
}

pub fn read_export_with_trailer(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
    object_flags: u32,
    ctx: &ExportContext<'_>,
) -> Result<(BTreeMap<String, PropValue>, usize)> {
    /// `RF_ClassDefaultObject`.
    const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;

    let mut r = Reader::with_ctx(export, names, ctx);
    // A handful of classes override `Serialize` and deliberately do **not**
    // call `Super::Serialize` on the load path, so the export carries no
    // property block, no `UObject` GUID trailer and no inherited tails — it
    // begins immediately with the class's own data. `URigVM::Serialize`
    // (RigVM.cpp:109) only calls up for reference collection and memory
    // counting; loading goes straight to `Load`.
    if class == "RigVM" || class == "RigHierarchy" {
        if class == "RigHierarchy" {
            let props = BTreeMap::new();
            let at = r.o;
            if read_rig_hierarchy(&mut r).is_err() {
                r.o = at;
            }
            return Ok((props, r.o));
        }
        let props = BTreeMap::new();
        read_rigvm(&mut r, usmap)?;
        return Ok((props, r.o));
    }
    let props = read_struct(&mut r, class, usmap, 0)?;
    if object_flags & RF_CLASS_DEFAULT_OBJECT == 0 && export.len() >= r.o + 4 {
        let at = r.o;
        match r.u32()? {
            0 => {}
            1 => {
                r.take(16)?;
            }
            // Not a boolean, so this export does not follow the trailer model
            // (its property walk stopped early, or the class serializes
            // something else here). Rewind and leave the rest as an unmodeled
            // tail rather than failing an otherwise good property decode.
            _ => {
                r.o = at;
                return Ok((props, r.o));
            }
        }
    }
    // Walk to the root of the chain, then replay it base → derived.
    let mut chain = Vec::new();
    let mut cur = Some(class.to_string());
    while let Some(c) = cur {
        if chain.len() > 64 {
            break;
        }
        cur = usmap.get(&c).and_then(|s| s.super_name.clone());
        chain.push(c);
    }
    chain.reverse();
    let why = tail_why();
    for c in &chain {
        let at = r.o;
        let keep_going = read_class_native_tail(&mut r, c, &props, usmap, ctx, object_flags)
            .with_context(|| format!("native tail of {c} (in {class})"))?;
        if why {
            eprintln!(
                "  chain: {c} {at}..{}{}",
                r.o,
                if keep_going { "" } else { "  <- STOPPED" }
            );
        }
        if !keep_going {
            break;
        }
    }
    Ok((props, r.o))
}

/// The `UObject` trailer every non-CDO export writes after its property block:
/// a four-byte `hasGuid` and, when set, the 16-byte GUID.
pub(super) fn read_uobject_trailer(r: &mut Reader, object_flags: u32) -> Result<()> {
    const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
    if object_flags & RF_CLASS_DEFAULT_OBJECT != 0 || r.b.len() < r.o + 4 {
        return Ok(());
    }
    match r.u32()? {
        0 => Ok(()),
        1 => r.take(16).map(|_| ()),
        other => bail!("UObject hasGuid is {other}, not a bool (@ {})", r.o - 4),
    }
}
