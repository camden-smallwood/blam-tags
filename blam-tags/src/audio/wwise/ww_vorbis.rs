//! Wwise-RIFF-Vorbis → standard Ogg Vorbis reconstruction.
//!
//! A faithful port of hcs's `ww2ogg` (the parts Halo 4 needs). Wwise stores
//! Vorbis with the three Ogg headers stripped/rebuilt and the codebooks
//! replaced by 10-bit indices into an external library; audio packets have
//! their Vorbis framing bits removed ("mod packets"). This module rebuilds a
//! spec-compliant Ogg bitstream in memory from a `.wem`'s parsed fields, which
//! is then handed to `lewton` for actual PCM decode.
//!
//! The H4 code path specifically (verified against `ww2ogg`/`vgmstream`):
//! external codebooks, stripped setup header, 2-byte packet headers with no
//! granule, and modified Vorbis packets. Other Wwise variants (inline
//! codebooks, full setup, header triads, 6/8-byte headers) are intentionally
//! not ported — they don't occur in these files.

/// External codebook library bundled into the binary (aoTuV 6.03 table, the
/// standard for modern Wwise). Selected by 10-bit id from each `.wem`'s setup.
static PACKED_CODEBOOKS: &[u8] = include_bytes!("packed_codebooks_aoTuV_603.bin");

// --- LSB-first bit reader over a byte slice -------------------------------
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u32, // bits already consumed from data[byte_pos]
    total_bits: u64,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
            total_bits: 0,
        }
    }

    fn get_bit(&mut self) -> Result<u32, String> {
        let byte = *self
            .data
            .get(self.byte_pos)
            .ok_or("Wwise Vorbis: out of bits")?;
        let bit = (byte >> self.bit_pos) & 1;
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        self.total_bits += 1;
        Ok(bit as u32)
    }

    /// Read `n` bits LSB-first into a u32 (n <= 32).
    fn read(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.get_bit()? << i;
        }
        Ok(v)
    }

    fn total_bits_read(&self) -> u64 {
        self.total_bits
    }
}

// --- LSB-first bit writer with Ogg page framing ---------------------------
const OGG_HEADER_BYTES: usize = 27;
const OGG_MAX_SEGMENTS: usize = 255;
const OGG_SEGMENT_SIZE: usize = 255;

struct OggWriter {
    out: Vec<u8>,
    bit_buffer: u8,
    bits_stored: u32,
    payload: Vec<u8>,
    first: bool,
    continued: bool,
    granule: u32,
    seqno: u32,
}

impl OggWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            bit_buffer: 0,
            bits_stored: 0,
            payload: Vec::new(),
            first: true,
            continued: false,
            granule: 0,
            seqno: 0,
        }
    }

    fn put_bit(&mut self, bit: u32) {
        if bit != 0 {
            self.bit_buffer |= 1 << self.bits_stored;
        }
        self.bits_stored += 1;
        if self.bits_stored == 8 {
            self.flush_bits();
        }
    }

    /// Write `n` low bits of `v`, LSB-first.
    fn write(&mut self, v: u32, n: u32) {
        for i in 0..n {
            self.put_bit((v >> i) & 1);
        }
    }

    fn flush_bits(&mut self) {
        if self.bits_stored != 0 {
            self.payload.push(self.bit_buffer);
            self.bits_stored = 0;
            self.bit_buffer = 0;
        }
    }

    fn set_granule(&mut self, g: u32) {
        self.granule = g;
    }

    fn flush_page(&mut self, next_continued: bool, last: bool) {
        self.flush_bits();
        if self.payload.is_empty() {
            return;
        }
        let payload_bytes = self.payload.len();
        // Round up, always leaving a terminating lacing value (may be 0);
        // clamp to the max, dropping the final 0 at exactly 255 segments.
        let mut segments = (payload_bytes + OGG_SEGMENT_SIZE) / OGG_SEGMENT_SIZE;
        if segments == OGG_MAX_SEGMENTS + 1 {
            segments = OGG_MAX_SEGMENTS;
        }

        let mut page = Vec::with_capacity(OGG_HEADER_BYTES + segments + payload_bytes);
        page.extend_from_slice(b"OggS");
        page.push(0); // stream_structure_version
        let mut header_type = 0u8;
        if self.continued {
            header_type |= 1;
        }
        if self.first {
            header_type |= 2;
        }
        if last {
            header_type |= 4;
        }
        page.push(header_type);
        page.extend_from_slice(&self.granule.to_le_bytes()); // granule low
        let granule_hi: u32 = if self.granule == 0xFFFF_FFFF {
            0xFFFF_FFFF
        } else {
            0
        };
        page.extend_from_slice(&granule_hi.to_le_bytes()); // granule high
        page.extend_from_slice(&1u32.to_le_bytes()); // serial number
        page.extend_from_slice(&self.seqno.to_le_bytes()); // page sequence
        page.extend_from_slice(&0u32.to_le_bytes()); // checksum placeholder
        page.push(segments as u8);

        // lacing values
        let mut bytes_left = payload_bytes;
        for _ in 0..segments {
            if bytes_left >= OGG_SEGMENT_SIZE {
                bytes_left -= OGG_SEGMENT_SIZE;
                page.push(OGG_SEGMENT_SIZE as u8);
            } else {
                page.push(bytes_left as u8);
                bytes_left = 0;
            }
        }
        page.extend_from_slice(&self.payload);

        let crc = ogg_checksum(&page);
        page[22..26].copy_from_slice(&crc.to_le_bytes());

        self.out.extend_from_slice(&page);
        self.seqno += 1;
        self.first = false;
        self.continued = next_continued;
        self.payload.clear();
    }
}

/// Ogg page CRC (poly 0x04c11db7, MSB-first, no reflection, init 0).
fn ogg_checksum(data: &[u8]) -> u32 {
    let mut reg: u32 = 0;
    for &b in data {
        let idx = ((reg >> 24) as u8) ^ b;
        reg = (reg << 8) ^ OGG_CRC_TABLE[idx as usize];
    }
    reg
}

static OGG_CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut r = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            r = if r & 0x8000_0000 != 0 {
                (r << 1) ^ 0x04c1_1db7
            } else {
                r << 1
            };
            j += 1;
        }
        table[i] = r;
        i += 1;
    }
    table
}

// --- vorbis helpers (from Tremor) -----------------------------------------
fn ilog(mut v: u32) -> u32 {
    let mut ret = 0;
    while v != 0 {
        ret += 1;
        v >>= 1;
    }
    ret
}

fn book_maptype1_quantvals(entries: u32, dimensions: u32) -> u32 {
    let bits = ilog(entries);
    let mut vals = entries >> ((bits - 1) * (dimensions - 1) / dimensions);
    loop {
        let mut acc: u64 = 1;
        let mut acc1: u64 = 1;
        for _ in 0..dimensions {
            acc *= vals as u64;
            acc1 *= (vals + 1) as u64;
        }
        if acc <= entries as u64 && acc1 > entries as u64 {
            return vals;
        } else if acc > entries as u64 {
            vals -= 1;
        } else {
            vals += 1;
        }
    }
}

// --- external codebook library --------------------------------------------
struct CodebookLibrary {
    data: &'static [u8],
    offsets: Vec<u32>,
}

impl CodebookLibrary {
    fn load() -> Self {
        let bytes = PACKED_CODEBOOKS;
        let n = bytes.len();
        let offset_offset =
            u32::from_le_bytes([bytes[n - 4], bytes[n - 3], bytes[n - 2], bytes[n - 1]]) as usize;
        let count = (n - offset_offset) / 4;
        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            let p = offset_offset + i * 4;
            offsets.push(u32::from_le_bytes([
                bytes[p],
                bytes[p + 1],
                bytes[p + 2],
                bytes[p + 3],
            ]));
        }
        Self {
            data: &bytes[..offset_offset],
            offsets,
        }
    }

    /// Bytes of codebook `i` (valid for i in [0, count-1)).
    fn codebook(&self, i: usize) -> Option<&[u8]> {
        if i + 1 >= self.offsets.len() {
            return None;
        }
        let a = self.offsets[i] as usize;
        let b = self.offsets[i + 1] as usize;
        self.data.get(a..b)
    }

    /// Rebuild external codebook `id` into the output Ogg setup stream.
    fn rebuild(&self, id: usize, os: &mut OggWriter) -> Result<(), String> {
        let cb = self
            .codebook(id)
            .ok_or_else(|| format!("Wwise Vorbis: invalid codebook id {id}"))?;
        let cb_size = cb.len();
        let mut bis = BitReader::new(cb);
        rebuild_codebook(&mut bis, cb_size, os)
    }
}

/// Rebuild one codebook from the packed (4-bit dim / 14-bit entries) form into
/// the standard Vorbis codebook bitstream. Mirrors `codebook_library::rebuild`.
fn rebuild_codebook(bis: &mut BitReader, cb_size: usize, os: &mut OggWriter) -> Result<(), String> {
    let dimensions = bis.read(4)?;
    let entries = bis.read(14)?;

    os.write(0x564342, 24); // "BCV" identifier
    os.write(dimensions, 16);
    os.write(entries, 24);

    let ordered = bis.read(1)?;
    os.write(ordered, 1);
    if ordered != 0 {
        let initial_length = bis.read(5)?;
        os.write(initial_length, 5);
        let mut current_entry = 0u32;
        while current_entry < entries {
            let bits = ilog(entries - current_entry);
            let number = bis.read(bits)?;
            os.write(number, bits);
            current_entry += number;
        }
        if current_entry > entries {
            return Err("Wwise Vorbis: codebook current_entry out of range".into());
        }
    } else {
        let codeword_length_length = bis.read(3)?;
        let sparse = bis.read(1)?;
        if codeword_length_length == 0 || codeword_length_length > 5 {
            return Err("Wwise Vorbis: nonsense codeword length".into());
        }
        os.write(sparse, 1);
        for _ in 0..entries {
            let present = if sparse != 0 {
                let p = bis.read(1)?;
                os.write(p, 1);
                p != 0
            } else {
                true
            };
            if present {
                let codeword_length = bis.read(codeword_length_length)?;
                os.write(codeword_length, 5);
            }
        }
    }

    let lookup_type = bis.read(1)?;
    os.write(lookup_type, 4);
    if lookup_type == 1 {
        let min = bis.read(32)?;
        let max = bis.read(32)?;
        let value_length = bis.read(4)?;
        let sequence_flag = bis.read(1)?;
        os.write(min, 32);
        os.write(max, 32);
        os.write(value_length, 4);
        os.write(sequence_flag, 1);

        let quantvals = book_maptype1_quantvals(entries, dimensions);
        for _ in 0..quantvals {
            let val = bis.read(value_length + 1)?;
            os.write(val, value_length + 1);
        }
    } else if lookup_type != 0 {
        return Err(format!("Wwise Vorbis: unexpected lookup type {lookup_type}"));
    }

    // exactly all bytes used (one extra 0 byte if the last byte was full)
    if cb_size != 0 && (bis.total_bits_read() / 8 + 1) as usize != cb_size {
        return Err(format!(
            "Wwise Vorbis: codebook size mismatch (expected {cb_size}, used {})",
            bis.total_bits_read() / 8 + 1
        ));
    }
    Ok(())
}

/// The parsed Wwise-Vorbis fields needed to rebuild an Ogg (H4 path only).
pub struct WwiseVorbis<'a> {
    /// The `data` chunk body (setup + audio packets, offsets are relative here).
    pub data: &'a [u8],
    pub channels: u32,
    pub sample_rate: u32,
    pub avg_bytes_per_second: u32,
    pub setup_packet_offset: u32,
    pub first_audio_packet_offset: u32,
    pub blocksize_0_pow: u8,
    pub blocksize_1_pow: u8,
    pub mod_packets: bool,
}

impl WwiseVorbis<'_> {
    /// Rebuild a complete standard Ogg Vorbis stream in memory.
    pub fn to_ogg(&self) -> Result<Vec<u8>, String> {
        let mut os = OggWriter::new();
        let mut mode_blockflag: Vec<bool> = Vec::new();
        let mut mode_bits = 0u32;

        self.write_headers(&mut os, &mut mode_blockflag, &mut mode_bits)?;
        self.write_audio(&mut os, &mode_blockflag, mode_bits)?;

        os.flush_page(false, false);
        Ok(std::mem::take(&mut os.out))
    }

    fn vorbis_head(os: &mut OggWriter, packet_type: u32) {
        os.write(packet_type, 8);
        for &c in b"vorbis" {
            os.write(c as u32, 8);
        }
    }

    fn write_headers(
        &self,
        os: &mut OggWriter,
        mode_blockflag: &mut Vec<bool>,
        mode_bits: &mut u32,
    ) -> Result<(), String> {
        // --- identification header ---
        Self::vorbis_head(os, 1);
        os.write(0, 32); // vorbis version
        os.write(self.channels, 8);
        os.write(self.sample_rate, 32);
        os.write(0, 32); // bitrate max
        os.write(self.avg_bytes_per_second * 8, 32); // bitrate nominal
        os.write(0, 32); // bitrate min
        os.write(self.blocksize_0_pow as u32, 4);
        os.write(self.blocksize_1_pow as u32, 4);
        os.write(1, 1); // framing
        os.flush_page(false, false);

        // --- comment header ---
        Self::vorbis_head(os, 3);
        let vendor = b"converted from Audiokinetic Wwise by blam-tags";
        os.write(vendor.len() as u32, 32);
        for &c in vendor {
            os.write(c as u32, 8);
        }
        os.write(0, 32); // user comment count (no loops handled)
        os.write(1, 1); // framing
        os.flush_page(false, false);

        // --- setup header (stripped, external codebooks) ---
        Self::vorbis_head(os, 5);
        let (setup, setup_size) = self.packet(self.setup_packet_offset)?;
        let mut ss = BitReader::new(setup);

        let codebook_count_less1 = ss.read(8)?;
        let codebook_count = codebook_count_less1 + 1;
        os.write(codebook_count_less1, 8);

        let cbl = CodebookLibrary::load();
        for _ in 0..codebook_count {
            let codebook_id = ss.read(10)?;
            cbl.rebuild(codebook_id as usize, os)?;
        }

        // time-domain transforms (placeholder)
        os.write(0, 6); // time count - 1
        os.write(0, 16); // dummy time value

        // floors
        let floor_count_less1 = ss.read(6)?;
        let floor_count = floor_count_less1 + 1;
        os.write(floor_count_less1, 6);
        for _ in 0..floor_count {
            os.write(1, 16); // always floor type 1

            let floor1_partitions = ss.read(5)?;
            os.write(floor1_partitions, 5);

            let mut partition_class_list = Vec::with_capacity(floor1_partitions as usize);
            let mut maximum_class = 0u32;
            for _ in 0..floor1_partitions {
                let pc = ss.read(4)?;
                os.write(pc, 4);
                if pc > maximum_class {
                    maximum_class = pc;
                }
                partition_class_list.push(pc);
            }

            let mut class_dimensions = vec![0u32; (maximum_class + 1) as usize];
            for j in 0..=maximum_class {
                let dims_less1 = ss.read(3)?;
                os.write(dims_less1, 3);
                class_dimensions[j as usize] = dims_less1 + 1;

                let subclasses = ss.read(2)?;
                os.write(subclasses, 2);
                if subclasses != 0 {
                    let masterbook = ss.read(8)?;
                    os.write(masterbook, 8);
                    if masterbook >= codebook_count {
                        return Err("Wwise Vorbis: invalid floor1 masterbook".into());
                    }
                }
                for _ in 0..(1u32 << subclasses) {
                    let subclass_book_plus1 = ss.read(8)?;
                    os.write(subclass_book_plus1, 8);
                    let subclass_book = subclass_book_plus1 as i32 - 1;
                    if subclass_book >= 0 && subclass_book as u32 >= codebook_count {
                        return Err("Wwise Vorbis: invalid floor1 subclass book".into());
                    }
                }
            }

            let multiplier_less1 = ss.read(2)?;
            os.write(multiplier_less1, 2);
            let rangebits = ss.read(4)?;
            os.write(rangebits, 4);

            for &pc in &partition_class_list {
                for _ in 0..class_dimensions[pc as usize] {
                    let x = ss.read(rangebits)?;
                    os.write(x, rangebits);
                }
            }
        }

        // residues
        let residue_count_less1 = ss.read(6)?;
        let residue_count = residue_count_less1 + 1;
        os.write(residue_count_less1, 6);
        for _ in 0..residue_count {
            let residue_type = ss.read(2)?;
            os.write(residue_type, 16);
            if residue_type > 2 {
                return Err("Wwise Vorbis: invalid residue type".into());
            }

            let residue_begin = ss.read(24)?;
            let residue_end = ss.read(24)?;
            let residue_partition_size_less1 = ss.read(24)?;
            let residue_classifications_less1 = ss.read(6)?;
            let residue_classbook = ss.read(8)?;
            let residue_classifications = residue_classifications_less1 + 1;
            os.write(residue_begin, 24);
            os.write(residue_end, 24);
            os.write(residue_partition_size_less1, 24);
            os.write(residue_classifications_less1, 6);
            os.write(residue_classbook, 8);
            if residue_classbook >= codebook_count {
                return Err("Wwise Vorbis: invalid residue classbook".into());
            }

            let mut cascade = vec![0u32; residue_classifications as usize];
            for c in cascade.iter_mut() {
                let low_bits = ss.read(3)?;
                os.write(low_bits, 3);
                let bitflag = ss.read(1)?;
                os.write(bitflag, 1);
                let high_bits = if bitflag != 0 {
                    let hb = ss.read(5)?;
                    os.write(hb, 5);
                    hb
                } else {
                    0
                };
                *c = high_bits * 8 + low_bits;
            }
            for &c in &cascade {
                for k in 0..8 {
                    if c & (1 << k) != 0 {
                        let residue_book = ss.read(8)?;
                        os.write(residue_book, 8);
                        if residue_book >= codebook_count {
                            return Err("Wwise Vorbis: invalid residue book".into());
                        }
                    }
                }
            }
        }

        // mappings
        let mapping_count_less1 = ss.read(6)?;
        let mapping_count = mapping_count_less1 + 1;
        os.write(mapping_count_less1, 6);
        for _ in 0..mapping_count {
            os.write(0, 16); // always mapping type 0

            let submaps_flag = ss.read(1)?;
            os.write(submaps_flag, 1);
            let submaps = if submaps_flag != 0 {
                let submaps_less1 = ss.read(4)?;
                os.write(submaps_less1, 4);
                submaps_less1 + 1
            } else {
                1
            };

            let square_polar_flag = ss.read(1)?;
            os.write(square_polar_flag, 1);
            if square_polar_flag != 0 {
                let coupling_steps_less1 = ss.read(8)?;
                let coupling_steps = coupling_steps_less1 + 1;
                os.write(coupling_steps_less1, 8);
                let bits = ilog(self.channels - 1);
                for _ in 0..coupling_steps {
                    let magnitude = ss.read(bits)?;
                    let angle = ss.read(bits)?;
                    os.write(magnitude, bits);
                    os.write(angle, bits);
                    if angle == magnitude || magnitude >= self.channels || angle >= self.channels {
                        return Err("Wwise Vorbis: invalid coupling".into());
                    }
                }
            }

            let mapping_reserved = ss.read(2)?;
            os.write(mapping_reserved, 2);
            if mapping_reserved != 0 {
                return Err("Wwise Vorbis: mapping reserved field nonzero".into());
            }

            if submaps > 1 {
                for _ in 0..self.channels {
                    let mux = ss.read(4)?;
                    os.write(mux, 4);
                    if mux >= submaps {
                        return Err("Wwise Vorbis: mapping mux >= submaps".into());
                    }
                }
            }
            for _ in 0..submaps {
                let time_config = ss.read(8)?;
                os.write(time_config, 8);
                let floor_number = ss.read(8)?;
                os.write(floor_number, 8);
                if floor_number >= floor_count {
                    return Err("Wwise Vorbis: invalid floor mapping".into());
                }
                let residue_number = ss.read(8)?;
                os.write(residue_number, 8);
                if residue_number >= residue_count {
                    return Err("Wwise Vorbis: invalid residue mapping".into());
                }
            }
        }

        // modes
        let mode_count_less1 = ss.read(6)?;
        let mode_count = mode_count_less1 + 1;
        os.write(mode_count_less1, 6);
        *mode_bits = ilog(mode_count - 1);
        mode_blockflag.clear();
        for _ in 0..mode_count {
            let block_flag = ss.read(1)?;
            os.write(block_flag, 1);
            mode_blockflag.push(block_flag != 0);
            os.write(0, 16); // window type
            os.write(0, 16); // transform type
            let mapping = ss.read(8)?;
            os.write(mapping, 8);
            if mapping >= mapping_count {
                return Err("Wwise Vorbis: invalid mode mapping".into());
            }
        }

        os.write(1, 1); // framing
        os.flush_page(false, false);

        // sanity: consumed exactly the setup packet
        if ss.total_bits_read().div_ceil(8) as usize != setup_size {
            return Err("Wwise Vorbis: setup packet not fully consumed".into());
        }
        Ok(())
    }

    fn write_audio(
        &self,
        os: &mut OggWriter,
        mode_blockflag: &[bool],
        mode_bits: u32,
    ) -> Result<(), String> {
        let mut offset = self.first_audio_packet_offset as usize;
        let mut prev_blockflag = false;
        let end = self.data.len();

        while offset < end {
            let (payload, size, next_offset) = self.packet_at(offset)?;
            os.set_granule(0); // no-granule packets

            if self.mod_packets {
                if mode_blockflag.is_empty() {
                    return Err("Wwise Vorbis: mode_blockflag not loaded".into());
                }
                let first_byte = *payload.first().ok_or("Wwise Vorbis: empty audio packet")?;
                // 1-bit packet type (0 = audio)
                os.write(0, 1);
                // N-bit mode number from the low bits of the first byte
                let mode_number = (first_byte as u32) & ((1 << mode_bits) - 1);
                os.write(mode_number, mode_bits);
                let remainder = (first_byte as u32) >> mode_bits;

                if mode_blockflag[mode_number as usize] {
                    // long window: emit prev/next window-type bits
                    let mut next_blockflag = false;
                    if next_offset < end
                        && let Ok((npayload, nsize, _)) = self.packet_at(next_offset)
                        && nsize > 0
                    {
                        let nb = *npayload.first().ok_or("Wwise Vorbis: empty next packet")?;
                        let next_mode = (nb as u32) & ((1 << mode_bits) - 1);
                        next_blockflag = mode_blockflag[next_mode as usize];
                    }
                    os.write(prev_blockflag as u32, 1);
                    os.write(next_blockflag as u32, 1);
                }
                prev_blockflag = mode_blockflag[mode_number as usize];
                // remaining bits of the first byte
                os.write(remainder, 8 - mode_bits);
                // rest of the packet bytes verbatim
                for &b in &payload[1..size] {
                    os.write(b as u32, 8);
                }
            } else {
                for &b in &payload[..size] {
                    os.write(b as u32, 8);
                }
            }

            offset = next_offset;
            os.flush_page(false, offset >= end);
        }
        Ok(())
    }

    /// Read a 2-byte-length (no-granule) packet whose header sits at
    /// `data[rel]`; return `(payload_slice, size, next_offset)`.
    fn packet_at(&self, rel: usize) -> Result<(&[u8], usize, usize), String> {
        if rel + 2 > self.data.len() {
            return Err("Wwise Vorbis: packet header truncated".into());
        }
        let size = u16::from_le_bytes([self.data[rel], self.data[rel + 1]]) as usize;
        let start = rel + 2;
        let stop = start
            .checked_add(size)
            .filter(|&s| s <= self.data.len())
            .ok_or("Wwise Vorbis: packet body truncated")?;
        Ok((&self.data[start..stop], size, stop))
    }

    /// Like `packet_at` but returns `(payload, size)` (for the setup packet).
    fn packet(&self, rel: u32) -> Result<(&[u8], usize), String> {
        let (payload, size, _) = self.packet_at(rel as usize)?;
        Ok((payload, size))
    }
}

#[cfg(test)]
mod encode_feasibility {
    //! Experiment: are the codebooks our Vorbis *encoder* (vorbis_rs / aoTuV
    //! Lancer) emits the same codebooks Wwise references from the external aoTuV
    //! 6.03 library? If yes, re-encoding to Wwise-Vorbis is feasible (map each
    //! emitted codebook back to its library id). If no, it's a dead end.
    use super::*;

    /// A normalized codebook for identity comparison.
    #[derive(PartialEq, Eq, Hash, Debug)]
    struct Cb {
        dims: u32,
        entries: u32,
        lengths: Vec<u8>,
        lookup: u8,
        lookup_values: Vec<u32>,
    }

    /// Parse a *standard* Vorbis codebook (as emitted in a real setup header).
    fn parse_standard(bis: &mut BitReader) -> Result<Cb, String> {
        let sync = bis.read(24)?;
        if sync != 0x564342 {
            return Err(format!("bad codebook sync {sync:#x}"));
        }
        let dims = bis.read(16)?;
        let entries = bis.read(24)?;
        let ordered = bis.read(1)?;
        let mut lengths = Vec::with_capacity(entries as usize);
        if ordered != 0 {
            let mut cur = 0u32;
            let mut len = bis.read(5)? + 1;
            while cur < entries {
                let bits = ilog(entries - cur);
                let num = bis.read(bits)?;
                for _ in 0..num {
                    lengths.push(len as u8);
                }
                cur += num;
                len += 1;
            }
        } else {
            let sparse = bis.read(1)?;
            for _ in 0..entries {
                let present = if sparse != 0 { bis.read(1)? != 0 } else { true };
                lengths.push(if present { (bis.read(5)? + 1) as u8 } else { 0 });
            }
        }
        let lookup = bis.read(4)? as u8;
        let mut lookup_values = Vec::new();
        if lookup == 1 || lookup == 2 {
            let _min = bis.read(32)?;
            let _max = bis.read(32)?;
            let value_bits = bis.read(4)? + 1;
            let _seq = bis.read(1)?;
            let count = if lookup == 1 {
                book_maptype1_quantvals(entries, dims)
            } else {
                entries * dims
            };
            for _ in 0..count {
                lookup_values.push(bis.read(value_bits)?);
            }
        } else if lookup != 0 {
            return Err(format!("unexpected lookup {lookup}"));
        }
        Ok(Cb { dims, entries, lengths, lookup, lookup_values })
    }

    /// Parse a *packed* aoTuV library codebook (4-bit dim / 14-bit entries form).
    fn parse_packed(bis: &mut BitReader) -> Result<Cb, String> {
        let dims = bis.read(4)?;
        let entries = bis.read(14)?;
        let ordered = bis.read(1)?;
        let mut lengths = Vec::with_capacity(entries as usize);
        if ordered != 0 {
            let mut cur = 0u32;
            let mut len = bis.read(5)? + 1;
            while cur < entries {
                let bits = ilog(entries - cur);
                let num = bis.read(bits)?;
                for _ in 0..num {
                    lengths.push(len as u8);
                }
                cur += num;
                len += 1;
            }
        } else {
            let cll = bis.read(3)?;
            let sparse = bis.read(1)?;
            for _ in 0..entries {
                let present = if sparse != 0 { bis.read(1)? != 0 } else { true };
                lengths.push(if present { (bis.read(cll)? + 1) as u8 } else { 0 });
            }
        }
        let lookup = bis.read(1)? as u8;
        let mut lookup_values = Vec::new();
        if lookup == 1 {
            let _min = bis.read(32)?;
            let _max = bis.read(32)?;
            let value_bits = bis.read(4)? + 1;
            let _seq = bis.read(1)?;
            let count = book_maptype1_quantvals(entries, dims);
            for _ in 0..count {
                lookup_values.push(bis.read(value_bits)?);
            }
        }
        Ok(Cb { dims, entries, lengths, lookup, lookup_values })
    }

    /// Split an Ogg bitstream into its packets (segment-lacing).
    fn ogg_packets(data: &[u8]) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut cur = Vec::new();
        let mut pos = 0usize;
        while pos + 27 <= data.len() && &data[pos..pos + 4] == b"OggS" {
            let nsegs = data[pos + 26] as usize;
            if pos + 27 + nsegs > data.len() {
                break;
            }
            let table = &data[pos + 27..pos + 27 + nsegs];
            let mut body = pos + 27 + nsegs;
            for &lace in table {
                let l = lace as usize;
                if body + l > data.len() {
                    return packets;
                }
                cur.extend_from_slice(&data[body..body + l]);
                body += l;
                if lace < 255 {
                    packets.push(std::mem::take(&mut cur));
                }
            }
            pos = body;
        }
        packets
    }

    #[test]
    fn encoder_codebooks_vs_aotuv_library() {
        // 1. Encode a real-ish signal with our Vorbis encoder.
        let pcm: Vec<i16> = (0..44_100 * 2)
            .map(|i| {
                let t = i as f64 / 44_100.0;
                let s = (2.0 * std::f64::consts::PI * 440.0 * t).sin()
                    + 0.5 * (2.0 * std::f64::consts::PI * 3000.0 * t).sin();
                (s * 9000.0) as i16
            })
            .collect();
        let ogg = crate::audio::encode_ogg_vorbis(&pcm, 1, 44_100).expect("encode");

        // 2. Pull the setup header (3rd Vorbis header packet) and parse its books.
        let packets = ogg_packets(&ogg);
        assert!(packets.len() >= 3, "expected >=3 header packets, got {}", packets.len());
        let setup = &packets[2];
        assert_eq!(setup[0], 5, "3rd packet should be the setup header");
        assert_eq!(&setup[1..7], b"vorbis");
        let mut bis = BitReader::new(&setup[7..]);
        let count = bis.read(8).expect("cb count") + 1;
        let mut encoder_books = Vec::new();
        for i in 0..count {
            encoder_books.push(parse_standard(&mut bis).unwrap_or_else(|e| panic!("enc cb {i}: {e}")));
        }

        // 3. Rebuild the aoTuV library into normalized codebooks.
        let lib = CodebookLibrary::load();
        let mut aotuv: std::collections::HashSet<Cb> = std::collections::HashSet::new();
        let mut id = 0usize;
        while let Some(cb) = lib.codebook(id) {
            let mut b = BitReader::new(cb);
            if let Ok(c) = parse_packed(&mut b) {
                aotuv.insert(c);
            }
            id += 1;
        }

        // 4. How many of the encoder's codebooks exist in the aoTuV library?
        let matched = encoder_books.iter().filter(|c| aotuv.contains(*c)).count();
        eprintln!(
            "ENCODER CODEBOOKS: {} | aoTuV LIBRARY: {} | MATCHED: {}/{}",
            encoder_books.len(),
            aotuv.len(),
            matched,
            encoder_books.len()
        );
        for (i, c) in encoder_books.iter().enumerate() {
            eprintln!(
                "  enc cb {i}: dims={} entries={} lookup={} inLib={}",
                c.dims,
                c.entries,
                c.lookup,
                aotuv.contains(c)
            );
        }
        // Not an assertion on the outcome — this test PRINTS the feasibility signal.
        assert!(!encoder_books.is_empty());
    }
}
