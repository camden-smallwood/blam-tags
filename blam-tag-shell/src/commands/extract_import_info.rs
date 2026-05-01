//! `extract-import-info` — unzip and write out the source files
//! Bungie's authoring tool baked into a tag's `info` (import-info)
//! stream when the tag was built. For a `model_animation_graph` /
//! `render_model` / `collision_model` / etc., the `files[]` block
//! holds the original `.JMS` / `.JMA` / texture / etc. that were
//! consumed at import time, zlib-compressed inside the
//! `tag_import_file_block.zipped data` field.
//!
//! Layout: each `files[i]` row carries the on-disk path the tool saw
//! (`c:\mcc\release\h3\source\…\foo.JMS`) plus that file's bytes
//! compressed with zlib. We strip the drive letter, normalize
//! separators, and write the decompressed bytes to
//! `<output>/<sanitized_path>`. Default `<output>` is
//! `./<tag_stem>/import_info/`.
//!
//! `--list` prints the manifest (path, original size, compressed size,
//! crc32) without writing anything — useful for verifying which source
//! files Bungie's importer recorded before deciding what to extract.
//!
//! Tag must have an `info` stream — `add-import-info` creates an empty
//! one but you'd be unzipping nothing in that case. MCC-shipped tags
//! routinely carry these.
//!
//! `MAXIMUM_TAG_IMPORT_FILE_ZIPPED_DATA_SIZE_IN_BYTES` from the schema
//! is 160 MiB, so a single source file can be very large; we stream
//! the decompressor's output to disk via `BufWriter` rather than
//! buffering in RAM.
//!
//! Per the schema: `tag_import_file_zipped_data_definition` is the
//! data-block definition for the compressed payload. Field name is
//! literally `"zipped data"` with a space.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use flate2::read::ZlibDecoder;

use crate::context::CliContext;
use crate::paths::tag_stem;

pub fn run(
    ctx: &mut CliContext,
    output: Option<&str>,
    list_only: bool,
) -> Result<()> {
    let loaded = ctx.loaded("extract-import-info")?;

    let import_info = loaded
        .tag
        .import_info()
        .ok_or_else(|| anyhow!(
            "tag has no `info` stream — nothing to extract. \
             Use `add-import-info` to attach an empty one if you \
             intended to author a tag from scratch."
        ))?;

    let files = import_info
        .field("files")
        .and_then(|f| f.as_block())
        .ok_or_else(|| anyhow!("`info` stream is missing the `files` block"))?;

    if files.is_empty() {
        println!("import-info `files` block is empty");
        return Ok(());
    }

    // Print header metadata so the user can correlate with the source-
    // tree layout (build / culprit / import date).
    let build = import_info
        .field("build")
        .and_then(|f| f.value())
        .and_then(|v| match v {
            blam_tags::TagFieldData::LongInteger(n) => Some(n),
            _ => None,
        });
    let version = read_string(&import_info, "version");
    let import_date = read_string(&import_info, "import date");
    let culprit = read_string(&import_info, "culprit");
    println!(
        "tag built {} {} by {} (build={:?})",
        version.as_deref().unwrap_or("?"),
        import_date.as_deref().unwrap_or(""),
        culprit.as_deref().unwrap_or("?"),
        build,
    );

    let dest_root: Option<PathBuf> = if list_only {
        None
    } else {
        let root = match output {
            Some(p) => PathBuf::from(p),
            None => {
                let stem = tag_stem(&loaded.path, "tag");
                PathBuf::from(stem).join("import_info")
            }
        };
        fs::create_dir_all(&root)
            .with_context(|| format!("create {}", root.display()))?;
        Some(root)
    };

    let mut total_compressed = 0u64;
    let mut total_decompressed = 0u64;
    let mut written = 0usize;
    let mut failed = 0usize;

    for (idx, file) in files.iter().enumerate() {
        let path = read_string(&file, "path").unwrap_or_else(|| format!("file_{idx}"));
        let original_size = file
            .field("size")
            .and_then(|f| f.value())
            .and_then(|v| match v {
                blam_tags::TagFieldData::LongInteger(n) => Some(n as u64),
                _ => None,
            })
            .unwrap_or(0);
        let crc32 = file
            .field("checksum")
            .and_then(|f| f.value())
            .and_then(|v| match v {
                blam_tags::TagFieldData::LongInteger(n) => Some(n as u32),
                _ => None,
            })
            .unwrap_or(0);
        let zipped = file
            .field("zipped data")
            .and_then(|f| f.as_data())
            .unwrap_or(&[]);

        total_compressed += zipped.len() as u64;

        if list_only {
            println!(
                "  [{idx:>3}] {path} ({}/{} bytes, crc32={:08x})",
                zipped.len(),
                original_size,
                crc32,
            );
            continue;
        }

        // Sanitize Windows-style absolute path: strip drive letter
        // (`c:\`), convert `\` to `/`, drop leading separators so
        // joining onto `dest_root` doesn't escape it.
        let rel = sanitize_path(&path);
        let target = dest_root.as_ref().unwrap().join(&rel);

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }

        match decompress_to(zipped, &target) {
            Ok(decompressed_size) => {
                total_decompressed += decompressed_size;
                written += 1;
                println!(
                    "  {} ({} → {} bytes)",
                    target.display(),
                    zipped.len(),
                    decompressed_size,
                );
            }
            Err(e) => {
                failed += 1;
                eprintln!("  {}: decompress failed — {e}", target.display());
            }
        }
    }

    println!(
        "{} files: {} written, {} failed; {} bytes compressed → {} bytes decompressed",
        files.len(),
        written,
        failed,
        total_compressed,
        total_decompressed,
    );

    if failed > 0 {
        anyhow::bail!("{failed} of {} files failed to decompress", files.len());
    }
    Ok(())
}

fn read_string(s: &blam_tags::TagStruct<'_>, name: &str) -> Option<String> {
    s.field(name).and_then(|f| f.value()).and_then(|v| match v {
        blam_tags::TagFieldData::String(s) | blam_tags::TagFieldData::LongString(s) => Some(s),
        _ => None,
    })
}

/// Strip drive letter + leading separators, normalize backslashes to
/// forward slashes. Output is always relative.
fn sanitize_path(input: &str) -> PathBuf {
    let mut s = input.replace('\\', "/");
    // Strip windows drive letter ("c:/foo" → "foo")
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        if bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            s = s[2..].to_string();
        }
    }
    while s.starts_with('/') {
        s = s[1..].to_string();
    }
    PathBuf::from(s)
}

fn decompress_to(zipped: &[u8], target: &Path) -> Result<u64> {
    let mut decoder = ZlibDecoder::new(zipped);
    let file = File::create(target)
        .with_context(|| format!("create {}", target.display()))?;
    let mut writer = BufWriter::new(file);
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = decoder.read(&mut buf)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n])?;
        total += n as u64;
    }
    writer.flush()?;
    Ok(total)
}
