//! Carrying a pageable `tag_resource` across a profile pair.
//! It owns deciding whether a resource can cross and reporting when it cannot;
//! field matching and the conversion walk belong to the parent module.

use super::*;
use crate::monolithic::{ControlReadTally, FixupAddress, FixupTier};

/// Move `source`'s pageable resource onto `target`, returning whether anything
/// was carried.
///
/// A null resource is nothing to carry and is not a loss. A non-null one either
/// crosses whole or is recorded in [`ConversionContext::resources_left_behind`],
/// which is what makes the tag refuse to be written rather than land looking
/// converted and playing nothing.
///
/// The decision is the engine's: `copy_resource_from` proves the two resource
/// definitions describe the same shape before it moves a byte. Past the header
/// struct a resource payload is an opaque codec stream — for an animation graph
/// it is the compressed animation data itself — so there is no partial or
/// best-effort translation to attempt. It fits or it does not.
pub fn transfer_resource(
    source: TagField<'_>,
    target: &mut TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) -> bool {
    let Some(resource) = source.as_resource() else {
        return false;
    };
    if matches!(resource.kind(), TagResourceKind::Null) {
        // Nothing to carry. Clear the destination so a template's own resource
        // cannot survive underneath a source that had none.
        let _ = target.clear_resource();
        return false;
    }

    match target.copy_resource_from(&resource) {
        Ok(()) => {
            context.report.transferred_resources += 1;
            true
        }
        Err(error) => {
            context.resources_left_behind.push(path.to_owned());
            record_unsupported(
                context,
                path.to_owned(),
                format!("The pageable resource could not be carried across: {error}"),
            );
            false
        }
    }
}

/// Stop counting a render-geometry resource as lost when the tag already has the
/// geometry inline.
///
/// An Xbox 360 build keeps its per-mesh vertex and index buffers in the pageable
/// cache; `MonolithicCache::read_tag` hydrates them into the author-format
/// `per mesh temporary[i]/{raw vertices, raw indices}` blocks on the way out,
/// which is where an MCC PC tag keeps them natively. Those blocks are ordinary
/// fields, so the walk has already carried them to the target. What
/// `transfer_resource` could not copy is the GPU resource that wrapped them, and
/// by this point that is a note about where the 360 stored its copy rather than
/// the data itself.
///
/// Only forgives what the target can show for itself
/// ([`crate::render_geometry::author_geometry_populated`] is all-or-nothing per
/// group), and only the api-resource field — a bitmap's texture resource or an
/// animation graph's payload sitting in the same list is still a real loss.
pub(super) fn forgive_hydrated_geometry(target: &TagFile, context: &mut ConversionContext<'_>) {
    if context.resources_left_behind.is_empty() {
        return;
    }
    if crate::render_geometry::author_geometry_populated(target) != Some(true) {
        return;
    }
    let field_key = clean_field_key(crate::render_geometry::API_RESOURCE_FIELD);
    forgive_resources(
        context,
        |path| {
            path.rsplit('/')
                .next()
                .is_some_and(|leaf| clean_field_key(leaf) == field_key)
        },
        |_| {
            "The GPU geometry resource was dropped; the mesh data came across in the \
             author-format raw vertex and index blocks instead"
                .to_owned()
        },
    );
}

/// Put a hydrated tag's compiled-geometry bookkeeping back to the shape an
/// uncompiled kit tag carries.
///
/// [`forgive_hydrated_geometry`] drops the GPU resource once the mesh data has
/// landed in the author-format blocks, which is right -- the buffers inside it
/// are Xenos-shaped and mean nothing to this engine. What it leaves behind is a
/// tag that still describes those buffers: every mesh points at a buffer slot,
/// the geometry claims it has been `processed`, and the meshes name vertex
/// formats only the 360 build compiles. The engine believes all of it, looks
/// for buffers that are not there, and draws the result.
///
/// The three things a kit's own tags say instead, measured across untouched
/// Reach BSPs and render models rather than assumed:
///
/// - `runtime flags` never has `processed` set. That is the bit that claims the
///   compiled buffers exist.
/// - `index buffer index` is always `-1`, and every slot of
///   `vertex buffer indices` is always `0`. Not the same sentinel, which is why
///   these are set separately rather than cleared to one value.
/// - `vertex type` is only ever `world`, `rigid` or `skinned`. The 360's
///   `rigid compressed` and `skinned compressed` describe the same vertex in a
///   packed buffer, and the packing is a property of the buffer this tag no
///   longer has.
///
/// Left alone deliberately: `analytical light index`, which holds the same
/// uninitialised value in tags the kit ships itself, and the budget flags, which
/// describe the geometry rather than the resource.
pub(super) fn settle_uncompiled_geometry(
    byte_order_upgrade: bool,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if !byte_order_upgrade
        || crate::render_geometry::author_geometry_populated(target) != Some(true)
    {
        return;
    }
    let mut settled = 0usize;
    settle_geometry_in(&mut target.root_mut(), &mut settled);
    if settled > 0 {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: String::new(),
            message: format!(
                "{settled} mesh(es) were re-described as uncompiled, because the compiled \
                 geometry buffers they named did not come across"
            ),
        });
    }
}

/// Walk for `render_geometry` structs. Identified by shape rather than by a
/// path table: the same struct appears under half a dozen field names across
/// the groups that carry geometry, and a tag that grows another one should not
/// need a list edited.
fn settle_geometry_in(value: &mut TagStructMut<'_>, settled: &mut usize) {
    let is_geometry = {
        let view = value.as_ref();
        view.field("meshes").and_then(|f| f.as_block()).is_some()
            && view.field("runtime flags").is_some()
    };
    if is_geometry {
        clear_processed_flag(value);
        if let Some(mut field) = value.field_mut("meshes")
            && let Some(mut meshes) = field.as_block_mut()
        {
            for index in 0..meshes.len() {
                if let Some(mut mesh) = meshes.element_mut(index) {
                    settle_mesh(&mut mesh);
                    *settled += 1;
                }
            }
        }
    }
    for index in 0..value.as_ref().fields().count() {
        let Some(mut field) = value.field_at_mut(index) else {
            continue;
        };
        if let Some(mut nested) = field.as_struct_mut() {
            settle_geometry_in(&mut nested, settled);
            continue;
        }
        if let Some(mut block) = field.as_block_mut() {
            for element in 0..block.len() {
                if let Some(mut element) = block.element_mut(element) {
                    settle_geometry_in(&mut element, settled);
                }
            }
        }
    }
}

/// Clear the `processed` bit and leave the rest of `runtime flags` alone.
fn clear_processed_flag(geometry: &mut TagStructMut<'_>) {
    let Some(mut field) = geometry.field_mut("runtime flags") else {
        return;
    };
    // By name: the bit's position is the schema's business, and `names` lists
    // only the bits actually set, so an absent entry means nothing to clear.
    let cleared = match field.as_ref().value() {
        Some(TagFieldData::LongFlags { value, names }) => {
            let Some((bit, _)) = names.iter().find(|(_, name)| name == "processed") else {
                return;
            };
            TagFieldData::LongFlags { value: value & !(1i32 << bit), names: names.clone() }
        }
        _ => return,
    };
    let _ = field.set(cleared);
}

/// Un-describe one mesh's compiled buffers.
fn settle_mesh(mesh: &mut TagStructMut<'_>) {
    set_int_field(mesh, "index buffer index", -1);
    // The tessellated sibling of the same index, and the same -1 when there is
    // no buffer to name.
    set_int_field(mesh, "index buffer tessellation", -1);
    if let Some(mut field) = mesh.field_mut("vertex buffer indices")
        && let Some(mut slots) = field.as_array_mut()
    {
        for index in 0..slots.len() {
            if let Some(mut slot) = slots.element_mut(index) {
                set_int_field(&mut slot, "vertex buffer index", 0);
            }
        }
    }
    let Some(mut field) = mesh.field_mut("vertex type") else {
        return;
    };
    // Named rather than numbered: the pair differs by how a buffer packs the
    // vertex, and the buffer is gone, so what is left is the plain format.
    let plain = match field.as_ref().value() {
        Some(TagFieldData::CharEnum { name: Some(name), .. }) => match name.as_str() {
            "rigid compressed" => "rigid",
            "skinned compressed" => "skinned",
            _ => return,
        },
        _ => return,
    };
    let Some(TagOptions::Enum { names, .. }) = field.as_ref().options() else {
        return;
    };
    let Some(ordinal) = names.iter().position(|name| *name == plain) else {
        return;
    };
    let _ = field.set(TagFieldData::CharEnum { value: ordinal as i8, name: None });
}

/// Put a geometry `user data` blob into the destination's byte order.
///
/// `render geometry/user data` pairs a header naming the payload's type, count
/// and size with an opaque `data` blob, so the field walk carries the blob
/// verbatim and every word inside it stays big-endian. The one type Reach
/// declares is `PRT Info`, whose 20 bytes are five longs; read the wrong way
/// round the first of them is 50331648 rather than 3.
///
/// Gated on the type name rather than on the blob looking word-shaped, so a
/// payload type added later is left alone instead of quietly mangled.
pub(super) fn swap_geometry_user_data(
    byte_order_upgrade: bool,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if !byte_order_upgrade {
        return;
    }
    let mut swapped = 0usize;
    swap_user_data_in(&mut target.root_mut(), &mut swapped);
    if swapped > 0 {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: String::new(),
            message: format!("{swapped} geometry user-data blob(s) were byte-swapped for the target"),
        });
    }
}

fn swap_user_data_in(value: &mut TagStructMut<'_>, swapped: &mut usize) {
    let is_word_payload = {
        let view = value.as_ref();
        view.field_path("user data header")
            .and_then(|f| f.as_struct())
            .and_then(|header| header.read_enum_name("data type"))
            .is_some_and(|kind| kind == "PRT Info")
    };
    if is_word_payload
        && let Some(mut field) = value.field_mut("user data")
    {
        let payload = field.as_ref().as_data().map(<[u8]>::to_vec);
        if let Some(mut bytes) = payload
            && bytes.len() % 4 == 0
        {
            for word in bytes.chunks_exact_mut(4) {
                word.reverse();
            }
            let _ = field.set(TagFieldData::Data(bytes));
            *swapped += 1;
        }
    }
    for index in 0..value.as_ref().fields().count() {
        let Some(mut field) = value.field_at_mut(index) else {
            continue;
        };
        if let Some(mut nested) = field.as_struct_mut() {
            swap_user_data_in(&mut nested, swapped);
            continue;
        }
        if let Some(mut block) = field.as_block_mut() {
            for element in 0..block.len() {
                if let Some(mut element) = block.element_mut(element) {
                    swap_user_data_in(&mut element, swapped);
                }
            }
        }
    }
}

/// Bring a structure BSP's resource interface inline.
///
/// A BSP keeps its collision hierarchy and its instanced geometry definitions --
/// which mesh each instance draws, which compression box it is quantized in,
/// and the collision to go with it -- in a structure the two builds put in
/// different places. A loose MCC tag holds it in `raw_resources[0]/raw_items`
/// and sets `use resource items` to 0. A 360 build holds it in the pageable
/// `tag_resources` and sets that field to 1, meaning "it is in the resource".
///
/// Carried across unchanged, the tag still says the definitions are in a
/// resource, and the resource is gone: 1,795 instances with nothing to point at
/// on one level alone. So the structure is read out of the build's control data
/// and written where a loose tag keeps it, and the flag is set to say so.
pub(super) fn carry_structure_resources(
    byte_order_upgrade: bool,
    source: &TagFile,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if !byte_order_upgrade || source.header.group_tag != u32::from_be_bytes(*b"sbsp") {
        return;
    }
    let root = source.root();
    let Some(resource) = root
        .field_path("resource interface/tag_resources")
        .and_then(|field| field.as_resource())
    else {
        return;
    };
    let Some(state) = resource.xsync_state() else {
        // Already in the destination's shape, or empty.
        return;
    };
    let control = state.apply_control_fixups();
    let primary = resource.exploded_payload().unwrap_or(&[]);
    let address = FixupAddress(state.header.root_address);
    if address.tier() != FixupTier::Control {
        record_unsupported(
            context,
            "resource interface/tag_resources".to_owned(),
            "The Xbox 360 structure resource's root does not point into its control data"
                .to_owned(),
        );
        return;
    }

    let mut tally = ControlReadTally::default();
    let outcome = {
        let mut target_root = target.root_mut();
        let Some(mut interface_field) = target_root.field_mut("resource interface") else {
            return;
        };
        let Some(mut interface) = interface_field.as_struct_mut() else {
            return;
        };
        let Some(mut raw_field) = interface.field_mut("raw_resources") else {
            return;
        };
        let Some(mut raw) = raw_field.as_block_mut() else {
            return;
        };
        // One element, whatever the source left here: the destination keeps
        // exactly one and reads `raw_items` out of it.
        raw.clear();
        let index = raw.add_element();
        let Some(mut element) = raw.element_mut(index) else {
            return;
        };
        let Some(mut items_field) = element.field_mut("raw_items") else {
            return;
        };
        let Some(mut items) = items_field.as_struct_mut() else {
            return;
        };
        // The resource's own root struct is `resource_items`, which is the same
        // struct `raw_items` is, so its offset is the root's.
        crate::monolithic::read_struct_into(
            &control,
            primary,
            address.offset() as usize,
            &mut items,
            &mut tally,
        )
    };

    if let Err(why) = outcome {
        record_unsupported(
            context,
            "resource interface/tag_resources".to_owned(),
            format!("The Xbox 360 structure resource could not be read: {why}"),
        );
        return;
    }
    // And say the definitions are here now rather than in a resource.
    if let Some(mut interface_field) = target.root_mut().field_mut("resource interface")
        && let Some(mut interface) = interface_field.as_struct_mut()
    {
        set_int_field(&mut interface, "use resource items", 0);
        if let Some(mut field) = interface.field_mut("tag_resources") {
            let _ = field.clear_resource();
        }
        if let Some(mut field) = interface.field_mut("cache_file_resources") {
            let _ = field.clear_resource();
        }
    }

    let counted = tally;
    forgive_resources(
        context,
        |path| path.starts_with("resource interface"),
        move |_| {
            format!(
                "The Xbox 360 structure resource was read out of the build's control data and \
                 written where a loose tag keeps it ({} struct(s), {} block element(s), {} byte(s) \
                 of payload)",
                counted.structs, counted.block_elements, counted.data_bytes
            )
        },
    );
}

/// Carry an Xbox 360 animation graph's payload across.
///
/// An animation graph keeps its substance in pageable resources: one per
/// `tag resource groups[i]`, each a list of members with a header and a codec
/// stream. A loose MCC tag stores that list inline; a monolithic 360 build
/// stores it as the engine had it in memory, a flat control-data buffer with
/// the pointers stubbed out. So there is nothing to copy -- the members have to
/// be read out of the one shape and written into the other, and the streams
/// turned round on the way.
///
/// All or nothing per tag. A graph with half its animations is worse than one
/// that says why it did not convert, and the streams that refuse are the four
/// in a 2011 build whose sections claim bytes the blob does not have.
pub(super) fn carry_animation_resources(
    byte_order_upgrade: bool,
    source: &TagFile,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if !byte_order_upgrade || source.header.group_tag != u32::from_be_bytes(*b"jmad") {
        return;
    }
    if context.resources_left_behind.is_empty() {
        return;
    }
    // Read every group first, so a stream that will not turn round is found
    // before anything has been written.
    let root = source.root();
    let Some(groups) = root.field_path("tag resource groups").and_then(|f| f.as_block()) else {
        return;
    };
    let mut carried: Vec<Vec<crate::animation::resource::AnimationResourceMember>> =
        Vec::with_capacity(groups.len());
    for index in 0..groups.len() {
        let Some(resource) = groups
            .element(index)
            .and_then(|group| group.field("tag_resource"))
            .and_then(|field| field.as_resource())
        else {
            carried.push(Vec::new());
            continue;
        };
        let Some(state) = resource.xsync_state() else {
            // Already in the destination's shape; the ordinary transfer has it.
            carried.push(Vec::new());
            continue;
        };
        let primary = resource.exploded_payload().unwrap_or(&[]);
        let Some(mut members) = crate::animation::resource::read_members(&state, primary) else {
            record_unsupported(
                context,
                format!("tag resource groups[{index}]/tag_resource"),
                "The Xbox 360 animation resource's control data could not be walked".to_owned(),
            );
            return;
        };
        for (member_index, member) in members.iter_mut().enumerate() {
            let frames = member.frame_count.max(1) as u16;
            if let Err(why) = crate::animation::byte_order::swap_animation_blob(
                &mut member.animation_data,
                &member.data_sizes,
                frames,
            ) {
                record_unsupported(
                    context,
                    format!("tag resource groups[{index}]/tag_resource"),
                    format!(
                        "Animation {member_index} could not be put into this side's byte \
                         order: {why}"
                    ),
                );
                return;
            }
        }
        carried.push(members);
    }

    let mut written = 0usize;
    {
        let mut target_root = target.root_mut();
        let Some(mut groups_field) = target_root.field_mut("tag resource groups") else {
            return;
        };
        let Some(mut target_groups) = groups_field.as_block_mut() else {
            return;
        };
        let mismatch = (target_groups.len() != carried.len())
            .then(|| (target_groups.len(), carried.len()));
        if let Some((theirs, ours)) = mismatch {
            drop(target_groups);
            drop(groups_field);
            record_unsupported(
                context,
                "tag resource groups".to_owned(),
                format!("The target carries {theirs} resource group(s) but the source has {ours}"),
            );
            return;
        }
        for (index, members) in carried.iter().enumerate() {
            if members.is_empty() {
                continue;
            }
            let Some(mut group) = target_groups.element_mut(index) else { continue };
            let Some(mut field) = group.field_mut("tag_resource") else { continue };
            if field.init_resource().is_err() {
                continue;
            }
            let Some(mut payload) = field.as_resource_struct_mut() else { continue };
            let Some(mut members_field) = payload.field_mut("group_members") else { continue };
            let Some(mut list) = members_field.as_block_mut() else { continue };
            for member in members {
                let at = list.add_element();
                let Some(mut element) = list.element_mut(at) else { continue };
                set_int_field(&mut element, "animation_index", member.animation_index as i64);
                set_int_field(&mut element, "animation_checksum", member.animation_checksum as i64);
                set_int_field(&mut element, "frame count", member.frame_count as i64);
                set_int_field(&mut element, "node count", member.node_count as i64);
                set_int_field(
                    &mut element,
                    "movement_data_type",
                    member.movement_data_type as i64,
                );
                if let Some(mut sizes_field) = element.field_mut("data sizes")
                    && let Some(mut sizes) = sizes_field.as_struct_mut()
                {
                    let names: Vec<String> = sizes.as_ref().field_names().map(str::to_owned).collect();
                    for (name, value) in names.iter().zip(member.data_sizes.iter()) {
                        set_int_field(&mut sizes, name, *value as i64);
                    }
                }
                if let Some(mut data) = element.field_mut("animation_data") {
                    let _ = data.set(TagFieldData::Data(member.animation_data.clone()));
                }
                written += 1;
            }
        }
    }

    forgive_resources(
        context,
        |path| path.starts_with("tag resource groups"),
        move |_| {
            format!(
                "The Xbox 360 animation resource was read out of the build's control data and \
                 written back as the inline members a loose tag carries ({written} animation(s), \
                 codec streams turned round)"
            )
        },
    );
}

/// Move an Xbox 360 bitmap's pixels into the shape a PC tag keeps them in.
///
/// The two builds disagree about where a bitmap's pixels live. A PC tag puts
/// every image in one top-level `processed pixel data` blob and has each
/// `bitmaps[i]` index into it. A 360 build leaves that blob empty and gives each
/// image its own pageable `hardware textures[i]/texture resource`, tiled into
/// Xenos 32x32 blocks with the bytes swapped inside each block. So the field
/// walk carries the metadata across correctly and the target lands with no
/// pixels at all.
///
/// [`crate::bitmap::Bitmap`] already reads either shape and hands back linear PC
/// bytes, so this is a re-lay-out rather than a decode: concatenate the images,
/// write the blob, and point each `bitmaps[i]` at its slice.
///
/// **The whole chain.** Every level and every layer comes across: the smaller
/// levels out of their packed layout (each 4KB-aligned, sub-16-pixel levels
/// sharing a tile), a cube map's six faces put back in D3D order from the one
/// Xenos stores them in, and an array's layers in the level-major order a kit
/// tag holds them. Each image is written with the mip count it actually
/// carries.
///
/// All or nothing: an image that will not decode leaves the whole tag untouched
/// and its resources on the lost list, so the tag is held back rather than
/// written with some images blank.
pub(super) fn convert_x360_bitmap_pixels(
    source: &TagFile,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if source.header.group_tag != u32::from_be_bytes(*b"bitm") {
        return;
    }
    // The 360 shape, asked of the data rather than of the byte order: a PC tag
    // that happened to be big-endian would have nothing to move.
    let root = source.root();
    let has_pc_pixels = root
        .field_path("processed pixel data")
        .and_then(|f| f.as_data())
        .is_some_and(|data| !data.is_empty());
    let has_hardware_textures = root
        .field_path("hardware textures")
        .and_then(|f| f.as_block())
        .is_some_and(|block| !block.is_empty());
    if has_pc_pixels || !has_hardware_textures {
        return;
    }

    // A `tag_cache` keeps its resource payloads in an LRU of resident blocks,
    // not a complete store, so a build can hold a bitmap whose pixels were never
    // paged in. The resource is then still an unhydrated `tgxc` and there is
    // genuinely nothing in this build to carry. Worth saying in those words,
    // because it is the one failure here that no amount of work on this end
    // fixes.
    if x360_payloads_are_unhydrated(&root) {
        record_unsupported(
            context,
            "hardware textures".to_owned(),
            "The Xbox 360 build has no resident pixel data for this bitmap: its texture              resources were never paged into the cache, so there is nothing to carry"
                .to_owned(),
        );
        return;
    }
    let bitmap = match crate::bitmap::Bitmap::new(source) {
        Ok(bitmap) => bitmap,
        Err(error) => {
            record_unsupported(
                context,
                "processed pixel data".to_owned(),
                format!("The Xbox 360 pixel data could not be read: {error}"),
            );
            return;
        }
    };

    // The whole blob is built before anything is written, so a failure part way
    // through cannot leave the tag describing pixels it does not have.
    // What the destination says each image is, read before anything is built:
    // a 360 tag carries both mirrors, and the PC one is the statement of what
    // a PC pixel blob should hold.
    let target_formats: Vec<Option<crate::bitmap::BitmapFormat>> = target
        .root()
        .field_path("bitmaps")
        .and_then(|field| field.as_block())
        .map(|block| {
            (0..block.len())
                .map(|index| {
                    block
                        .element(index)
                        .and_then(|elem| elem.read_enum_name("format"))
                        .and_then(|name| {
                            crate::bitmap::BitmapFormat::from_schema_name(&name)
                        })
                })
                .collect()
        })
        .unwrap_or_default();

    let mut blob: Vec<u8> = Vec::new();
    let mut slices: Vec<(i32, i32)> = Vec::with_capacity(bitmap.len());
    let mut mip_counts: Vec<i64> = Vec::with_capacity(bitmap.len());
    for index in 0..bitmap.len() {
        let Some(image) = bitmap.image(index) else {
            record_unsupported(
                context,
                format!("bitmaps[{index}]"),
                "The Xbox 360 image is missing from the tag".to_owned(),
            );
            return;
        };
        let pixels = match image.pixel_bytes() {
            Ok(pixels) => pixels,
            Err(error) => {
                // Say how many surfaces were being assembled: a multi-layer
                // image has six or more of them and which one ran short is the
                // first thing anybody reading this will want.
                let detail = if image.layer_count() > 1 {
                    format!(
                        "The {}-layer Xbox 360 {} could not be detiled: {error}",
                        image.layer_count(),
                        image.type_name().unwrap_or_else(|| "image".to_owned())
                    )
                } else {
                    format!("The Xbox 360 image could not be detiled: {error}")
                };
                record_unsupported(context, format!("bitmaps[{index}]"), detail);
                return;
            }
        };
        // What the 360 actually stored, which is what has to be decoded.
        let source_format = image.format().ok();
        // The two mirrors can disagree about the format. Halo's `ctx1`,
        // `dxn_mono_alpha` and the `dxt3a`/`dxt5a` family are Xbox 360 block
        // formats that the PC build ships decoded, so the tag's own PC image
        // block asks for `v8u8`, `a8y8` or `y8` where the 360 stored blocks.
        // Believe it: it is the destination's own statement of what it holds,
        // and its `pixels size` is computed from it.
        let target_format = target_formats
            .get(index)
            .copied()
            .flatten()
            .filter(|target| Some(*target) != source_format);
        let pixels = match target_format {
            None => pixels.to_vec(),
            Some(target) => {
                let Some(source) = source_format else {
                    record_unsupported(
                        context,
                        format!("bitmaps[{index}]"),
                        "The Xbox 360 image's format could not be resolved".to_owned(),
                    );
                    return;
                };
                match crate::bitmap::encode::transcode_levels(
                    source,
                    target,
                    image.width(),
                    image.height(),
                    image.mipmap_levels(),
                    image.layer_count(),
                    pixels,
                    crate::bitmap::P8Palette::Halo2,
                ) {
                    Ok(packed) => packed,
                    Err(error) => {
                        record_unsupported(
                            context,
                            format!("bitmaps[{index}]"),
                            format!(
                                "The Xbox 360 image is {source:?} and the target asks for \
                                 {target:?}, which could not be produced: {error}"
                            ),
                        );
                        return;
                    }
                }
            }
        };

        // Levels beyond the base, which is what the field counts. Taken from
        // the image the detiler actually produced rather than from the source,
        // so the number and the bytes can never disagree.
        mip_counts.push(image.mipmap_levels().saturating_sub(1) as i64);
        slices.push((blob.len() as i32, pixels.len() as i32));
        blob.extend_from_slice(&pixels);
    }
    if blob.is_empty() {
        return;
    }

    let images = slices.len();
    let mut target_root = target.root_mut();
    // A tag that described its images only in the 360 mirror has nowhere to
    // record what was just detiled, so give it the block a kit tag would have.
    describe_images_from_mirror(source, &mut target_root);
    let Some(mut bitmaps_field) = target_root.field_mut("bitmaps") else {
        record_unsupported(
            context,
            "bitmaps".to_owned(),
            "The target has no bitmaps block to put the pixels behind".to_owned(),
        );
        return;
    };
    let Some(mut bitmaps) = bitmaps_field.as_block_mut() else {
        return;
    };
    if bitmaps.len() != images {
        let carried = bitmaps.len();
        drop(bitmaps_field);
        record_unsupported(
            context,
            "bitmaps".to_owned(),
            format!(
                "The target carries {carried} image(s) but the source has {images}; the pixels \
                 were not written"
            ),
        );
        return;
    }
    for (index, (offset, size)) in slices.iter().enumerate() {
        let Some(mut elem) = bitmaps.element_mut(index) else {
            continue;
        };
        set_int_field(&mut elem, "pixels offset", i64::from(*offset));
        set_int_field(&mut elem, "pixels size", i64::from(*size));
        set_int_field(&mut elem, "mipmap count", mip_counts[index]);
        set_int_field(&mut elem, "high res pixels offset offset", 0);
        set_int_field(&mut elem, "high res pixels size", 0);
        // Two handles the 360 build filled in at run time and a kit tag ships
        // zero: the D3D format the texture was created with, and where the tag
        // happened to be loaded. Carried across they describe a machine that is
        // not this one.
        set_int_field(&mut elem, "hardware format", 0);
        set_int_field(&mut elem, "runtime tag base address", 0);
    }
    drop(bitmaps_field);

    if let Some(mut field) = target_root.field_mut("processed pixel data") {
        let _ = field.set(TagFieldData::Data(blob));
    }
    // The 360 mirrors describe storage that no longer exists. Left in place they
    // send a reader back to a resource this has just emptied.
    for name in [
        "hardware textures",
        "xenon bitmaps",
        "interleaved hardware textures",
    ] {
        if let Some(mut field) = target_root.field_mut(name)
            && let Some(mut block) = field.as_block_mut()
        {
            block.clear();
        }
    }
    if let Some(mut field) = target_root.field_mut("xenon processed pixel data") {
        let _ = field.set(TagFieldData::Data(Vec::new()));
    }
    // And the tag no longer reaches its pixels through an interop handle, which
    // is what this bit claims. A kit tag for the same texture has it clear.
    clear_flags_by_name(&mut target_root, "Flags", &["using tag_interop and tag_resource"]);

    forgive_resources(
        context,
        |path| path.starts_with("hardware textures"),
        |path| {
            format!(
                "The Xbox 360 texture resource was dropped; its pixels were detiled into \
                 processed pixel data instead ({path})"
            )
        },
    );
}

/// Whether every one of this bitmap's texture resources is still an unhydrated
/// xsync stub.
///
/// A monolithic build's `cache_N` partitions are an LRU of resident resource
/// blocks, not a complete archive, so a tag can name a resource the build never
/// paged in. `MonolithicCache::read_tag` leaves those as `tgxc` rather than
/// inventing bytes for them, and this is how that shows up downstream.
fn x360_payloads_are_unhydrated(root: &TagStruct<'_>) -> bool {
    let Some(block) = root.field_path("hardware textures").and_then(|f| f.as_block()) else {
        return false;
    };
    if block.is_empty() {
        return false;
    }
    (0..block.len())
        .filter_map(|index| block.element(index))
        .filter_map(|elem| elem.field("texture resource"))
        .filter_map(|field| field.as_resource())
        .all(|resource| matches!(resource.kind(), TagResourceKind::Xsync))
}

/// Groups whose substance the PC build keeps outside the tag.
///
/// A `sound` is the case: MCC Reach's own sound tags carry
/// `sound data resource` null and name an FMOD bank instead, so the samples
/// were never in the tag to begin with. An Xbox 360 build put them there as an
/// XMA stream, and that stream is the one thing about the tag that genuinely
/// cannot move — but it also is not wanted, because the destination does not
/// read it.
/// The second field says whether the group also carries geometry that has to
/// have arrived first. A BSP's meshes come across as author-format blocks like a
/// render model's, and forgiving its resources before checking that would write
/// a level with no geometry and no complaint.
const PAYLOAD_LIVES_OUTSIDE_THE_TAG: &[(&str, bool)] =
    &[("sound", false), ("scenario_structure_bsp", true)];

/// Stop counting a resource as lost when the destination would not carry it.
///
/// Only for a byte-order upgrade, and only for a group on the list above. Both
/// halves matter. Across engines the audio really is stranded, which is what the
/// conversion catalog's `sound` rule is about; within one engine the tag is
/// simply moving to the side of that engine that keeps its samples in banks,
/// and arriving with a null resource is arriving in the right shape.
///
/// What this cannot do is bring the audio with it. The tag lands complete in
/// every other respect and finds its samples by name in the destination kit's
/// banks, exactly as a stock tag there does; a sound the kit has no bank for is
/// silent, and the report says so.
pub(super) fn forgive_externally_stored_payload(
    byte_order_upgrade: bool,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if !byte_order_upgrade || context.resources_left_behind.is_empty() {
        return;
    }
    let Some((_, needs_geometry)) = PAYLOAD_LIVES_OUTSIDE_THE_TAG
        .iter()
        .find(|(group, _)| context.group_name.eq_ignore_ascii_case(group))
    else {
        return;
    };
    if *needs_geometry && crate::render_geometry::author_geometry_populated(target) != Some(true) {
        return;
    }
    // Cleared rather than left as the template found it: a resource copied from
    // a kit tag underneath a source that had its own would be somebody else's
    // audio.
    clear_all_resources(&mut target.root_mut());
    let group = context.group_name.to_owned();
    forgive_resources(
        context,
        |_| true,
        move |_| {
            format!(
                "The Xbox 360 payload was dropped; a {group} on this side of the engine carries \
                 this resource null and finds what it needs outside the tag"
            )
        },
    );
}

/// Null every pageable resource in a tag.
fn clear_all_resources(value: &mut TagStructMut<'_>) {
    for index in 0..value.as_ref().fields().count() {
        let Some(mut field) = value.field_at_mut(index) else {
            continue;
        };
        if field.as_ref().field_type() == TagFieldType::PageableResource {
            let _ = field.clear_resource();
            continue;
        }
        if let Some(mut nested) = field.as_struct_mut() {
            clear_all_resources(&mut nested);
            continue;
        }
        if let Some(mut block) = field.as_block_mut() {
            for element in 0..block.len() {
                if let Some(mut element) = block.element_mut(element) {
                    clear_all_resources(&mut element);
                }
            }
        }
    }
}

/// Put every curve in a converted tag into the destination's byte order.
///
/// A `mapping_function` is a byte blob, so the field walk carries it across
/// verbatim and every value inside it stays in the source's order: the clamp
/// range, the control points, and the compact size the engine uses to find the
/// end of it. That last one is why this is not a cosmetic problem. The engine
/// reads a nonsense length and walks off the end of the tag, and what the mod
/// tools report is an access violation inside `tag_load` with nothing to say
/// about which field caused it.
///
/// Curves are everywhere -- animated shader parameters, particle properties,
/// light fades, weapon rates -- so this walks the whole tag rather than a list
/// of known fields. What makes that safe is that
/// [`swap_function_definition`] identifies a curve by walking its structure and
/// refuses anything that does not add up exactly.
///
/// Only for a byte-order upgrade. Between two little-endian profiles a curve is
/// already in the right order and swapping it would be the bug.
pub(super) fn swap_function_curves(
    byte_order_upgrade: bool,
    target: &mut TagFile,
    context: &mut ConversionContext<'_>,
) {
    if !byte_order_upgrade {
        return;
    }
    let mut swapped = 0usize;
    swap_curves_in(&mut target.root_mut(), &mut swapped);
    if swapped > 0 {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: String::new(),
            message: format!("{swapped} function curve(s) were byte-swapped for the target"),
        });
    }
}

fn swap_curves_in(value: &mut TagStructMut<'_>, swapped: &mut usize) {
    for index in 0..value.as_ref().fields().count() {
        let Some(mut field) = value.field_at_mut(index) else {
            continue;
        };
        if let Some(bytes) = field.as_ref().as_data()
            && let Some(fixed) = crate::tag_function::swap_function_definition(bytes)
        {
            let _ = field.set(TagFieldData::Data(fixed));
            *swapped += 1;
            continue;
        }
        if let Some(mut nested) = field.as_struct_mut() {
            swap_curves_in(&mut nested, swapped);
            continue;
        }
        if let Some(mut block) = field.as_block_mut() {
            for element in 0..block.len() {
                if let Some(mut element) = block.element_mut(element) {
                    swap_curves_in(&mut element, swapped);
                }
            }
            continue;
        }
        if let Some(mut array) = field.as_array_mut() {
            for element in 0..array.len() {
                if let Some(mut element) = array.element_mut(element) {
                    swap_curves_in(&mut element, swapped);
                }
            }
        }
    }
}

/// Give a mirror-only bitmap the PC image block it never had.
///
/// A Reach tag describes each image twice: `bitmaps` for the PC build and
/// `xenon bitmaps` for the 360 one. Most of the 2011 build's bitmaps carry
/// both, and the pixel pass only has to fill in offsets. Around one in eight --
/// every lightmap among them -- carries the 360 mirror alone, and there is
/// nowhere to record the pixels it just detiled.
///
/// The two blocks share one struct definition, and a kit tag for the same
/// texture holds the mirror's own values almost verbatim: the same size, type,
/// format, curve and flags. What it does not keep is the three fields that
/// describe how the 360 stored the picture rather than what the picture is --
/// the tiled and pitch bits, the tile-size shorthand, and the hardware format
/// handle. Those are cleared, because after detiling they would be a lie.
///
/// Returns how many elements were written, so the caller can tell "the mirror
/// was missing too" apart from "nothing needed doing".
fn describe_images_from_mirror(source: &TagFile, target: &mut TagStructMut<'_>) -> usize {
    let root = source.root();
    let Some(mirror) = root.field_path("xenon bitmaps").and_then(|f| f.as_block()) else {
        return 0;
    };
    if mirror.is_empty() {
        return 0;
    }
    let Some(mut field) = target.field_mut("bitmaps") else {
        return 0;
    };
    let Some(mut images) = field.as_block_mut() else {
        return 0;
    };
    if !images.is_empty() {
        return 0;
    }
    let mut written = 0usize;
    for index in 0..mirror.len() {
        let Some(source_image) = mirror.element(index) else {
            continue;
        };
        let at = images.add_element();
        let Some(mut target_image) = images.element_mut(at) else {
            continue;
        };
        copy_fields_by_name(&source_image, &mut target_image);
        clear_flags_by_name(
            &mut target_image,
            "more flags",
            &["xbox360 tiled texture", "xbox360 pitch (memory spacing)"],
        );
        set_int_field(&mut target_image, "four times log2 size", 0);
        set_int_field(&mut target_image, "hardware format", 0);
        written += 1;
    }
    written
}

/// Copy every field the two structs share, by name.
///
/// Not [`crate::api::TagBlockMut::paste_element`]: the two blocks share a
/// definition in the kit's schema but not necessarily in the tag's own, and a
/// 2011 tag widens `hardware format` differently. Name and value, coerced to
/// whatever width the destination declares, is the part that always holds.
fn copy_fields_by_name(source: &TagStruct<'_>, target: &mut TagStructMut<'_>) {
    for name in source.field_names() {
        let Some(value) = source.field(&name).and_then(|f| f.value()) else {
            continue;
        };
        if let Some(number) = source.read_int_any(&name) {
            set_int_field(target, &name, number as i64);
            continue;
        }
        if let Some(mut field) = target.field_mut(&name)
            && std::mem::discriminant(&value)
                == field.as_ref().value().map(|v| std::mem::discriminant(&v)).unwrap_or_else(
                    || std::mem::discriminant(&value),
                )
        {
            let _ = field.set(value);
        }
    }
}

/// Clear named bits of a flags field, leaving every other bit as it was.
fn clear_flags_by_name(elem: &mut TagStructMut<'_>, field_name: &str, bits: &[&str]) {
    let Some(mut field) = elem.field_mut(field_name) else {
        return;
    };
    let cleared = match field.as_ref().value() {
        Some(TagFieldData::ByteFlags { mut value, names }) => {
            for (bit, name) in &names {
                if bits.contains(&name.as_str()) {
                    value &= !(1u8 << bit);
                }
            }
            TagFieldData::ByteFlags { value, names }
        }
        Some(TagFieldData::WordFlags { mut value, names }) => {
            for (bit, name) in &names {
                if bits.contains(&name.as_str()) {
                    value &= !(1u16 << bit);
                }
            }
            TagFieldData::WordFlags { value, names }
        }
        Some(TagFieldData::LongFlags { mut value, names }) => {
            for (bit, name) in &names {
                if bits.contains(&name.as_str()) {
                    value &= !(1i32 << bit);
                }
            }
            TagFieldData::LongFlags { value, names }
        }
        _ => return,
    };
    let _ = field.set(cleared);
}

/// Set an integer-shaped field at whatever width and shape the schema declares.
///
/// The same field is a `char_integer` in one profile and a `short_integer` in
/// the next, and `TagFieldMut::set` takes the variant rather than a number, so
/// guessing the wrong one writes nothing at all. Enums, flags and block indices
/// are integer-shaped too -- [`TagStruct::read_int_any`] reads all of them --
/// and a setter that handled only the plain widths would silently drop half of
/// what a caller copying a struct by name hands it.
fn set_int_field(elem: &mut TagStructMut<'_>, name: &str, value: i64) {
    let Some(mut field) = elem.field_mut(name) else {
        return;
    };
    let replacement = match field.as_ref().value() {
        Some(TagFieldData::CharInteger(_)) => TagFieldData::CharInteger(value as i8),
        Some(TagFieldData::ByteInteger(_)) => TagFieldData::ByteInteger(value as u8),
        Some(TagFieldData::ShortInteger(_)) => TagFieldData::ShortInteger(value as i16),
        Some(TagFieldData::WordInteger(_)) => TagFieldData::WordInteger(value as u16),
        Some(TagFieldData::LongInteger(_)) => TagFieldData::LongInteger(value as i32),
        Some(TagFieldData::DwordInteger(_)) => TagFieldData::DwordInteger(value as u32),
        Some(TagFieldData::Int64Integer(_)) => TagFieldData::Int64Integer(value),
        Some(TagFieldData::QwordInteger(_)) => TagFieldData::QwordInteger(value as u64),
        Some(TagFieldData::CharBlockIndex(_)) => TagFieldData::CharBlockIndex(value as i8),
        Some(TagFieldData::ShortBlockIndex(_)) => TagFieldData::ShortBlockIndex(value as i16),
        Some(TagFieldData::LongBlockIndex(_)) => TagFieldData::LongBlockIndex(value as i32),
        Some(TagFieldData::CustomCharBlockIndex(_)) => {
            TagFieldData::CustomCharBlockIndex(value as i8)
        }
        Some(TagFieldData::CustomShortBlockIndex(_)) => {
            TagFieldData::CustomShortBlockIndex(value as i16)
        }
        Some(TagFieldData::CustomLongBlockIndex(_)) => {
            TagFieldData::CustomLongBlockIndex(value as i32)
        }
        // Names are resolved from the layout on read, so `None` here is not a
        // loss -- the next read of this field resolves the new value's name.
        Some(TagFieldData::CharEnum { .. }) => {
            TagFieldData::CharEnum { value: value as i8, name: None }
        }
        Some(TagFieldData::ShortEnum { .. }) => {
            TagFieldData::ShortEnum { value: value as i16, name: None }
        }
        Some(TagFieldData::LongEnum { .. }) => {
            TagFieldData::LongEnum { value: value as i32, name: None }
        }
        Some(TagFieldData::ByteFlags { names, .. }) => {
            TagFieldData::ByteFlags { value: value as u8, names }
        }
        Some(TagFieldData::WordFlags { names, .. }) => {
            TagFieldData::WordFlags { value: value as u16, names }
        }
        Some(TagFieldData::LongFlags { names, .. }) => {
            TagFieldData::LongFlags { value: value as i32, names }
        }
        Some(TagFieldData::ByteBlockFlags(_)) => TagFieldData::ByteBlockFlags(value as u8),
        Some(TagFieldData::WordBlockFlags(_)) => TagFieldData::WordBlockFlags(value as u16),
        Some(TagFieldData::LongBlockFlags(_)) => TagFieldData::LongBlockFlags(value as i32),
        _ => return,
    };
    let _ = field.set(replacement);
}

/// Take every left-behind resource `matches` accepts off the lost list, leaving a
/// warning on the report in place of the refusal.
///
/// Shared by the two passes that make a resource unnecessary rather than
/// carrying it. Neither deletes the issue: somebody comparing this tag against a
/// kit-authored one should still be told the resource is gone, and why.
fn forgive_resources(
    context: &mut ConversionContext<'_>,
    matches: impl Fn(&str) -> bool,
    message: impl Fn(&str) -> String,
) {
    let forgiven: Vec<String> = context
        .resources_left_behind
        .iter()
        .filter(|path| matches(path))
        .cloned()
        .collect();
    if forgiven.is_empty() {
        return;
    }
    context
        .resources_left_behind
        .retain(|path| !forgiven.contains(path));
    for path in forgiven {
        for issue in &mut context.report.issues {
            if issue.path == path && issue.kind == ConversionIssueKind::Unsupported {
                issue.kind = ConversionIssueKind::Warning;
                issue.message = message(&path);
            }
        }
    }
}
