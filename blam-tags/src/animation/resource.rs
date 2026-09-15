//! Reading a `model_animation_graph`'s runtime payload out of an Xbox 360
//! monolithic build's pageable resource.
//!
//! It owns the control-data walk that turns `tag resource groups[i]/tag_resource`
//! into the `group_members` a loose tag holds inline; the codec stream inside
//! each member's `animation_data` is [`super::codec`]'s business, and writing
//! the members back into a tag is the converter's.
//!
//! A loose MCC tag keeps the same struct tree as a chunked resource payload:
//! `model_animation_tag_resource_struct` is one `tag_block` of
//! `model_animation_tag_resource_member`, each 100 bytes of header plus a
//! `tag_data` pointing at the animation blob. A monolithic 360 build instead
//! keeps that tree as the engine had it in memory — a flat control-data buffer
//! with the pointers stubbed out and a fixup list to fill them back in — so the
//! same shape has to be read by address rather than by chunk.

use crate::monolithic::{FixupAddress, FixupTier, XSyncState};

/// One `group_members[m]` element read out of a 360 resource.
///
/// Field names follow the schema. `data_sizes` is left as the raw seventeen
/// words rather than a named struct: what each one means shifts between engine
/// versions, and [`crate::animation::PackedDataSizes`] is the thing that knows.
#[derive(Debug, Clone)]
pub struct AnimationResourceMember {
    pub animation_index: i32,
    pub animation_checksum: i32,
    pub frame_count: i16,
    pub node_count: i8,
    pub movement_data_type: i8,
    /// `data sizes`, in schema order.
    pub data_sizes: [i32; DATA_SIZE_COUNT],
    /// The codec stream, in the byte order the 360 wrote it.
    pub animation_data: Vec<u8>,
}

/// How many words `packed_data_sizes_struct` declares in Reach.
pub const DATA_SIZE_COUNT: usize = 17;

/// On-disk size of one `model_animation_tag_resource_member`.
const MEMBER_SIZE: usize = 100;
/// `s_tag_block`: count, address, and a runtime word.
const TAG_BLOCK_SIZE: usize = 12;
/// `s_tag_data`: size, then flags and stream position, then the address.
const TAG_DATA_ADDRESS: usize = 12;

/// Walk a 360 animation resource and return its members.
///
/// `primary` is the resource's always-resident buffer, which is where the blobs
/// themselves live; the control data holds only the structs pointing at them.
/// Returns `None` when the state carries no control data or its root does not
/// point where a resource root has to.
pub fn read_members(state: &XSyncState, primary: &[u8]) -> Option<Vec<AnimationResourceMember>> {
    let control = state.apply_control_fixups();
    let root = FixupAddress(state.header.root_address);
    if root.tier() != FixupTier::Control {
        return None;
    }
    let (count, address) = read_tag_block(&control, root.offset() as usize)?;
    // A resource with no members is a real thing -- an inheriting graph carries
    // one and leaves it empty -- so an empty list is a success, not a miss.
    if count == 0 {
        return Some(Vec::new());
    }
    if address.tier() != FixupTier::Control {
        return None;
    }
    let first = address.offset() as usize;
    let mut members = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let at = first.checked_add(index.checked_mul(MEMBER_SIZE)?)?;
        members.push(read_member(&control, primary, at)?);
    }
    Some(members)
}

fn read_member(
    control: &[u8],
    primary: &[u8],
    at: usize,
) -> Option<AnimationResourceMember> {
    let raw = control.get(at..at + MEMBER_SIZE)?;
    let mut data_sizes = [0i32; DATA_SIZE_COUNT];
    for (index, size) in data_sizes.iter_mut().enumerate() {
        *size = be_i32(raw, 12 + index * 4)?;
    }
    let (length, address) = read_tag_data(raw, 80)?;
    // The blob is in whichever buffer the address names. Control-tier is
    // unobserved for animation data but costs nothing to accept.
    let source: &[u8] = match address.tier() {
        FixupTier::Primary => primary,
        FixupTier::Control => control,
        // A zero-length blob has a null address, which is not a failure.
        _ if length == 0 => &[],
        _ => return None,
    };
    let start = address.offset() as usize;
    let animation_data = source.get(start..start.checked_add(length as usize)?)?.to_vec();
    Some(AnimationResourceMember {
        animation_index: be_i32(raw, 0)?,
        animation_checksum: be_i32(raw, 4)?,
        frame_count: i16::from_be_bytes(raw.get(8..10)?.try_into().ok()?),
        node_count: raw[10] as i8,
        movement_data_type: raw[11] as i8,
        data_sizes,
        animation_data,
    })
}

fn read_tag_block(bytes: &[u8], at: usize) -> Option<(u32, FixupAddress)> {
    let raw = bytes.get(at..at + TAG_BLOCK_SIZE)?;
    Some((be_u32(raw, 0)?, FixupAddress(be_u32(raw, 4)?)))
}

fn read_tag_data(bytes: &[u8], at: usize) -> Option<(u32, FixupAddress)> {
    let raw = bytes.get(at..at + 20)?;
    Some((be_u32(raw, 0)?, FixupAddress(be_u32(raw, TAG_DATA_ADDRESS)?)))
}

fn be_u32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn be_i32(bytes: &[u8], at: usize) -> Option<i32> {
    be_u32(bytes, at).map(|value| value as i32)
}
