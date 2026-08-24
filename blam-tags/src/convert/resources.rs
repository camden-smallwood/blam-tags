//! Carrying a pageable `tag_resource` across a profile pair.
//! It owns deciding whether a resource can cross and reporting when it cannot;
//! field matching and the conversion walk belong to the parent module.

use super::*;

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
/// **One mip.** The detiler reproduces the base level only — the smaller chain
/// has its own packed layout (each level 4KB-aligned, sub-16-pixel mips sharing
/// a tile) that nothing here reconstructs. Every image is written with a mip
/// count of 0 and the report says so, because a bitmap claiming mips it does not
/// carry is worse than one honestly missing them.
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
                // Named apart because it is a known gap rather than a surprise:
                // a cube map's six faces and an array's layers each sit in their
                // own 360 surface, and only the first is reproduced here.
                let detail = if image.layer_count() > 1 {
                    format!(
                        "A {}-layer Xbox 360 {} is not reproduced yet; only single-layer images                          come across",
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

/// Set an integer field at whatever width the schema declares it.
///
/// The same field is a `char_integer` in one profile and a `short_integer` in
/// the next, and `TagFieldMut::set` takes the variant rather than a number, so
/// guessing the wrong one writes nothing at all.
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
