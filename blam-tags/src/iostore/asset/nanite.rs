//! Decoder for cooked UE5 **Nanite** geometry (the high-detail mesh a Nanite
//! `UStaticMesh` stores instead of a full-resolution LOD — the classic
//! `FStaticMeshLODResources` only keeps a coarse fallback). Campaign Evolved is
//! UE 5.5.4, so this targets the 5.5 layout.
//!
//! Ported from CUE4Parse's Nanite reader. This file holds the low-level
//! bit-stream primitives every stage builds on; the page/cluster structures and
//! the decode pipeline layer on top.
//!
//! Nanite geometry is a DAG of *pages* → *clusters*; each cluster holds up to
//! 256 triangles as bit-packed, delta-encoded, quantized vertex streams (an
//! SOA low/mid/high byte layout in 5.4+) plus a strip-compressed index stream.

#![allow(dead_code)]

/// `BitFieldExtractU32`: `numBits` bits of `value` starting at `offset`.
#[inline]
pub fn get_bits(value: u32, num_bits: u32, offset: u32) -> u32 {
    // Match HLSL/C# semantics: the shift amount is taken mod 32, so a wrapped
    // `offset` (e.g. `foundBitIndex - 1` underflowing to u32::MAX) behaves like
    // `>> 31` rather than panicking.
    let offset = offset & 31;
    if num_bits >= 32 {
        return value >> offset;
    }
    let mask = (1u32 << num_bits) - 1;
    (value >> offset) & mask
}

/// Sign-extend the low `bit_length` bits of `value` to a signed `i32`.
#[inline]
pub fn uint_to_int(value: u32, bit_length: u32) -> i32 {
    ((value << (32 - bit_length)) as i32) >> (32 - bit_length)
}

/// `BitFieldExtractS32`.
#[inline]
pub fn get_bits_as_signed(value: u32, num_bits: u32, offset: u32) -> i32 {
    uint_to_int(get_bits(value, num_bits, offset), num_bits)
}

/// Funnel-shift: take `shift` (mod 32) bits from the top of `low`, backfilling
/// from `high`. Matches HLSL `BitAlignU32`.
#[inline]
pub fn bit_align_u32(high: u32, low: u32, shift: u64) -> u32 {
    let shift = (shift & 31) as u32;
    let mut result = low >> shift;
    if shift > 0 {
        result |= high << (32 - shift);
    }
    result
}

#[inline]
pub fn bit_field_mask_u32(mask_width: u32, mask_location: u32) -> u32 {
    let w = mask_width & 31;
    let l = mask_location & 31;
    ((1u32 << w) - 1) << l
}

#[inline]
pub fn decode_zigzag(data: u32) -> i32 {
    ((data >> 1) as i32) ^ -((data & 1) as i32)
}

/// Index of the highest set bit, or `u32::MAX` if `x == 0`.
#[inline]
pub fn first_bit_high(x: u32) -> u32 {
    if x == 0 {
        u32::MAX
    } else {
        31 - x.leading_zeros()
    }
}

#[inline]
pub fn unpack_byte0(v: u32) -> u32 {
    v & 0xff
}
#[inline]
pub fn unpack_byte1(v: u32) -> u32 {
    (v >> 8) & 0xff
}
#[inline]
pub fn unpack_byte2(v: u32) -> u32 {
    (v >> 16) & 0xff
}
#[inline]
pub fn unpack_byte3(v: u32) -> u32 {
    v >> 24
}

/// `2^-i` via direct float exponent manipulation — the quantization scale table
/// (`PrecisionScales`). Valid for `i` in `[-32, 32]`.
#[inline]
pub fn precision_scale(i: i32) -> f32 {
    // 1.0f bits (0x3F80_0000) minus (i << 23), reinterpreted as f32.
    f32::from_bits((0x3F80_0000i32 - (i << 23)) as u32)
}

/// `low/mid/high` byte-count increment for a stream of `num` values at
/// `bytes_per_value`.
#[inline]
pub fn low_mid_high_increment(bytes_per_value: u32, num: u32) -> [u32; 3] {
    [
        if bytes_per_value >= 1 { num } else { 0 },
        if bytes_per_value >= 2 { num } else { 0 },
        if bytes_per_value >= 3 { num } else { 0 },
    ]
}

/// Read a (possibly bit-unaligned) `u32` from `data`, anchored at
/// `base_addr` bytes with `bit_offset` bits beyond it. Mirrors CUE4Parse's
/// `ReadUnalignedDword`.
pub fn read_unaligned_dword(data: &[u8], base_addr: usize, bit_offset: u64) -> u32 {
    let byte_addr = base_addr as u64 + (bit_offset >> 3);
    let aligned = byte_addr & !3;
    let bit_offset = ((byte_addr - aligned) << 3) | (bit_offset & 7);
    let low = read_u32(data, aligned as usize);
    let high = read_u32(data, aligned as usize + 4);
    bit_align_u32(high, low, bit_offset)
}

/// Little-endian `u32` at `offset`, zero-filled past the end (Nanite readers
/// deliberately over-read at page boundaries).
#[inline]
pub fn read_u32(data: &[u8], offset: usize) -> u32 {
    let mut b = [0u8; 4];
    for i in 0..4 {
        if let Some(&x) = data.get(offset + i) {
            b[i] = x;
        }
    }
    u32::from_le_bytes(b)
}

/// The SOA low/mid/high byte-stream reader for 5.4+ cluster vertex data. Each
/// value's bytes are split across three byte banks; a running `prev` value is
/// added back (delta coding) after zig-zag decode.
pub struct LmhStreamReader<'a> {
    data: &'a [u8],
}

impl<'a> LmhStreamReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    /// Read `count` components (`components` = how many byte banks are active,
    /// 1..=3), delta-accumulating into `prev`. `low_mid_high` are absolute byte
    /// offsets of the three banks; `index` is the value index in the stream.
    pub fn read(
        &self,
        low_mid_high: [u32; 3],
        components: u32,
        count: usize,
        index: u32,
        prev: &mut [i32; 4],
    ) -> [i32; 4] {
        let pos = [
            low_mid_high[0] + index * count as u32,
            low_mid_high[1] + index * count as u32,
            low_mid_high[2] + index * count as u32,
        ];
        let mut packed = [0u32; 4];

        if components >= 3 {
            for i in 0..count {
                packed[i] |= (*self.data.get(pos[2] as usize + i).unwrap_or(&0) as u32) << 16;
            }
        }
        if components >= 2 {
            for i in 0..count {
                packed[i] |= (*self.data.get(pos[1] as usize + i).unwrap_or(&0) as u32) << 8;
            }
        }
        if components >= 1 {
            for i in 0..count {
                packed[i] |= *self.data.get(pos[0] as usize + i).unwrap_or(&0) as u32;
            }
        }

        let mut value = [0i32; 4];
        for i in 0..count {
            value[i] = decode_zigzag(packed[i]) + prev[i];
        }
        // Components beyond `count` carry the previous value forward unchanged.
        for i in count..4 {
            value[i] = prev[i];
        }
        *prev = value;
        value
    }
}

/// The aligned bit-stream reader used for 5.3-and-earlier attribute data and
/// (5.6+) bone influences. Reads little-endian dwords lazily, shifting a
/// 4-dword window. `max_remaining_bits` bounds the window refill logic.
pub struct BitStreamReader<'a> {
    data: &'a [u8],
    aligned_byte_address: i64,
    bit_offset_from_address: i64,
    compile_time_max_remaining_bits: i64,
    buffer_bits: [u32; 4],
    buffer_offset: i64,
    compile_time_min_buffer_bits: i64,
    compile_time_min_dword_bits: i64,
}

impl<'a> BitStreamReader<'a> {
    /// `byte_address` need not be 4-aligned; the offset is folded in.
    pub fn new(data: &'a [u8], byte_address: i64, bit_offset: i64, max_remaining_bits: i64) -> Self {
        let aligned = byte_address & !3;
        let bit_offset = bit_offset + ((byte_address & 3) << 3);
        Self {
            data,
            aligned_byte_address: aligned,
            bit_offset_from_address: bit_offset,
            compile_time_max_remaining_bits: max_remaining_bits,
            buffer_bits: [0; 4],
            buffer_offset: 0,
            compile_time_min_buffer_bits: 0,
            compile_time_min_dword_bits: 0,
        }
    }

    /// Already-4-aligned variant (`CreateBitStreamReader_Aligned`).
    pub fn new_aligned(
        data: &'a [u8],
        byte_address: i64,
        bit_offset: i64,
        max_remaining_bits: i64,
    ) -> Self {
        Self {
            data,
            aligned_byte_address: byte_address,
            bit_offset_from_address: bit_offset,
            compile_time_max_remaining_bits: max_remaining_bits,
            buffer_bits: [0; 4],
            buffer_offset: 0,
            compile_time_min_buffer_bits: 0,
            compile_time_min_dword_bits: 0,
        }
    }

    pub fn read(&mut self, num_bits: u32, compile_time_max_bits: i64) -> u32 {
        if compile_time_max_bits > self.compile_time_min_buffer_bits {
            self.bit_offset_from_address += self.buffer_offset;
            let address = self.aligned_byte_address + ((self.bit_offset_from_address >> 5) << 2);
            let data: [u32; 4] = [
                read_u32(self.data, address as usize),
                read_u32(self.data, address as usize + 4),
                read_u32(self.data, address as usize + 8),
                read_u32(self.data, address as usize + 12),
            ];
            let off = self.bit_offset_from_address as u64;
            self.buffer_bits[0] = bit_align_u32(data[1], data[0], off);
            if self.compile_time_max_remaining_bits > 32 {
                self.buffer_bits[1] = bit_align_u32(data[2], data[1], off);
            }
            if self.compile_time_max_remaining_bits > 64 {
                self.buffer_bits[2] = bit_align_u32(data[3], data[2], off);
            }
            if self.compile_time_max_remaining_bits > 96 {
                self.buffer_bits[3] = bit_align_u32(0, data[3], off);
            }
            self.buffer_offset = 0;
            self.compile_time_min_dword_bits = 32.min(self.compile_time_max_remaining_bits);
            self.compile_time_min_buffer_bits = 97.min(self.compile_time_max_remaining_bits);
        } else if compile_time_max_bits > self.compile_time_min_dword_bits {
            self.bit_offset_from_address += self.buffer_offset;
            let offset32 = self.compile_time_min_dword_bits == 0 && self.buffer_offset == 32;
            let bo = self.buffer_offset as u64;
            self.buffer_bits[0] = if offset32 {
                self.buffer_bits[1]
            } else {
                bit_align_u32(self.buffer_bits[1], self.buffer_bits[0], bo)
            };
            if self.compile_time_min_buffer_bits > 32 {
                self.buffer_bits[1] = if offset32 {
                    self.buffer_bits[2]
                } else {
                    bit_align_u32(self.buffer_bits[2], self.buffer_bits[1], bo)
                };
            }
            if self.compile_time_min_buffer_bits > 64 {
                self.buffer_bits[2] = if offset32 {
                    self.buffer_bits[3]
                } else {
                    bit_align_u32(self.buffer_bits[3], self.buffer_bits[2], bo)
                };
            }
            if self.compile_time_min_buffer_bits > 96 {
                self.buffer_bits[3] = if offset32 {
                    0
                } else {
                    bit_align_u32(0, self.buffer_bits[3], bo)
                };
            }
            self.buffer_offset = 0;
            self.compile_time_min_dword_bits = 32.min(self.compile_time_max_remaining_bits);
        }

        let result = get_bits(self.buffer_bits[0], num_bits, self.buffer_offset as u32);
        self.buffer_offset += num_bits as i64;
        self.compile_time_min_buffer_bits -= compile_time_max_bits;
        self.compile_time_min_dword_bits -= compile_time_max_bits;
        self.compile_time_max_remaining_bits -= compile_time_max_bits;
        result
    }
}

// ---------------------------------------------------------------------------
// Page + cluster structures (UE 5.5 layout)
// ---------------------------------------------------------------------------

/// `NANITE_MIN_POSITION_PRECISION` for 5.4+.
pub const MIN_POSITION_PRECISION: i32 = -20;
/// uint4 rows per packed cluster header.
pub const NUM_PACKED_CLUSTER_FLOAT4S: usize = 8;
pub const GPU_PAGE_HEADER_SIZE: usize = 16;
/// `NANITE_MAX_CLUSTERS_PER_PAGE_BITS` (5.4+).
pub const MAX_CLUSTERS_PER_PAGE_BITS: u32 = 8;
pub const VERTEX_COLOR_MODE_VARIABLE: u32 = 1;
pub const FIXUP_MAGIC: u16 = 0x464E;

/// A streaming page's disk header (5.5): offsets are relative to the page-disk
/// header origin (right after the fixup chunk).
#[derive(Debug, Clone, Copy, Default)]
pub struct PageDiskHeader {
    pub num_clusters: u32,
    pub num_raw_float4s: u32,
    pub num_vertex_refs: u32,
    pub decode_info_offset: u32,
    pub strip_bitmask_offset: u32,
    pub vertex_ref_bitmask_offset: u32,
}

/// Per-cluster disk header (5.5): all offsets relative to the page-disk header.
#[derive(Debug, Clone, Copy, Default)]
pub struct ClusterDiskHeader {
    pub index_data_offset: u32,
    pub page_cluster_map_offset: u32,
    pub vertex_ref_data_offset: u32,
    pub low_bytes_offset: u32,
    pub mid_bytes_offset: u32,
    pub high_bytes_offset: u32,
    pub num_vertex_refs: u32,
    pub num_prev_ref_vertices_before_dwords: u32,
    pub num_prev_new_vertices_before_dwords: u32,
}

/// Decoded (unpacked) cluster header — the SOA `FCluster` fields we need to
/// decode geometry. UE 5.5 only.
#[derive(Debug, Clone, Default)]
pub struct Cluster {
    pub num_verts: u32,
    pub position_offset: u32,
    pub num_tris: u32,
    pub index_offset: u32,
    pub color_min: [i32; 4],
    pub color_component_bits: [i32; 4],
    pub pos_start: [i32; 3],
    pub bits_per_index: u32,
    pub pos_precision: i32,
    pub pos_scale: f32,
    pub pos_bits: [u32; 3],
    pub normal_precision: u32,
    pub tangent_precision: u32,
    pub flags: u32,
    pub attribute_offset: u32,
    pub bits_per_attribute: u32,
    pub decode_info_offset: u32,
    pub has_tangents: bool,
    pub skinning: bool,
    pub num_uvs: u32,
    pub color_mode: u32,
    pub uv_bit_offsets: u32,
    // Material assignment (fast path when material_table_length == 0).
    pub material_table_offset: u32,
    pub material_table_length: u32,
    pub material0_index: u32,
    pub material1_index: u32,
    pub material2_index: u32,
    pub material0_length: u32,
    pub material1_length: u32,
}

/// Read a `u16` LE (zero-filled past end).
#[inline]
fn read_u16(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([
        data.get(offset).copied().unwrap_or(0),
        data.get(offset + 1).copied().unwrap_or(0),
    ])
}

/// A decoded root/streaming page: its cluster metadata + the raw page bytes.
/// `disk_header_offset` is the origin all cluster offsets are relative to; the
/// GPU-page header origin drives the material/decode-info offsets.
pub struct Page {
    pub disk_header_offset: usize,
    pub gpu_header_offset: usize,
    pub disk_header: PageDiskHeader,
    pub cluster_disk_headers: Vec<ClusterDiskHeader>,
    pub clusters: Vec<Cluster>,
}

impl Page {
    /// Parse a page from `data` starting at the fixup chunk at `start` (5.5).
    /// Returns the parsed page (cluster *headers* only; geometry decode is a
    /// later stage).
    pub fn parse(data: &[u8], start: usize) -> Option<Page> {
        // FFixupChunk header (5.5): magic, NumClusters, NumHierarchyFixups,
        // NumClusterFixups.
        let magic = read_u16(data, start);
        if magic != FIXUP_MAGIC {
            return None;
        }
        let fixup_num_clusters = read_u16(data, start + 2);
        let num_hierarchy_fixups = read_u16(data, start + 4) as usize;
        let num_cluster_fixups = read_u16(data, start + 6) as usize;
        // Skip the fixup arrays: FHierarchyFixup=16B, FClusterFixup=8B.
        let disk_header_offset = start + 8 + num_hierarchy_fixups * 16 + num_cluster_fixups * 8;

        let d = disk_header_offset;
        let disk_header = PageDiskHeader {
            num_clusters: read_u32(data, d),
            num_raw_float4s: read_u32(data, d + 4),
            num_vertex_refs: read_u32(data, d + 8),
            decode_info_offset: read_u32(data, d + 12),
            strip_bitmask_offset: read_u32(data, d + 16),
            vertex_ref_bitmask_offset: read_u32(data, d + 20),
        };
        let num_clusters = disk_header.num_clusters;
        if num_clusters == 0
            || num_clusters > 1024
            || num_clusters as u16 != fixup_num_clusters
        {
            return None;
        }

        // FClusterDiskHeader[NumClusters] (36B each).
        let cdh_base = disk_header_offset + 24;
        let mut cluster_disk_headers = Vec::with_capacity(num_clusters as usize);
        for i in 0..num_clusters as usize {
            let o = cdh_base + i * 36;
            cluster_disk_headers.push(ClusterDiskHeader {
                index_data_offset: read_u32(data, o),
                page_cluster_map_offset: read_u32(data, o + 4),
                vertex_ref_data_offset: read_u32(data, o + 8),
                low_bytes_offset: read_u32(data, o + 12),
                mid_bytes_offset: read_u32(data, o + 16),
                high_bytes_offset: read_u32(data, o + 20),
                num_vertex_refs: read_u32(data, o + 24),
                num_prev_ref_vertices_before_dwords: read_u32(data, o + 28),
                num_prev_new_vertices_before_dwords: read_u32(data, o + 32),
            });
        }

        // FPageGPUHeader (16B) then SOA-packed cluster headers.
        let gpu_header_offset = cdh_base + num_clusters as usize * 36;
        let cluster_origin = gpu_header_offset + GPU_PAGE_HEADER_SIZE;
        let mut clusters = Vec::with_capacity(num_clusters as usize);
        for i in 0..num_clusters {
            clusters.push(parse_cluster(data, cluster_origin, i, num_clusters));
        }

        Some(Page {
            disk_header_offset,
            gpu_header_offset,
            disk_header,
            cluster_disk_headers,
            clusters,
        })
    }
}

/// Row `r` of cluster `i` in the page's SOA cluster-header block.
#[inline]
fn soa_row(cluster_origin: usize, cluster_index: u32, num_clusters: u32, row: u32) -> usize {
    cluster_origin + 16 * (cluster_index + row * num_clusters) as usize
}

/// Unpack one SOA cluster header (UE 5.5).
fn parse_cluster(data: &[u8], origin: usize, index: u32, num_clusters: u32) -> Cluster {
    let row = |r: u32| soa_row(origin, index, num_clusters, r);
    let mut c = Cluster::default();

    // Row 0.
    let r0 = row(0);
    let num_verts_position_offset = read_u32(data, r0);
    c.num_verts = get_bits(num_verts_position_offset, 9, 0);
    c.position_offset = get_bits(num_verts_position_offset, 23, 9);
    let num_tris_index_offset = read_u32(data, r0 + 4);
    c.num_tris = get_bits(num_tris_index_offset, 8, 0);
    c.index_offset = get_bits(num_tris_index_offset, 24, 8);
    let color_min = read_u32(data, r0 + 8);
    c.color_min = [
        unpack_byte0(color_min) as i32,
        unpack_byte1(color_min) as i32,
        unpack_byte2(color_min) as i32,
        unpack_byte3(color_min) as i32,
    ];
    let color_bits_group = read_u32(data, r0 + 12);
    let color_bits = get_bits(color_bits_group, 16, 0);
    c.color_component_bits = [
        get_bits(color_bits, 4, 0) as i32,
        get_bits(color_bits, 4, 4) as i32,
        get_bits(color_bits, 4, 8) as i32,
        get_bits(color_bits, 4, 12) as i32,
    ];

    // Row 1.
    let r1 = row(1);
    c.pos_start = [
        read_u32(data, r1) as i32,
        read_u32(data, r1 + 4) as i32,
        read_u32(data, r1 + 8) as i32,
    ];
    let bpi_pp_pb = read_u32(data, r1 + 12);
    c.bits_per_index = get_bits(bpi_pp_pb, 3, 0) + 1;
    c.pos_precision = get_bits(bpi_pp_pb, 6, 3) as i32 + MIN_POSITION_PRECISION;
    c.pos_bits = [
        get_bits(bpi_pp_pb, 5, 9),
        get_bits(bpi_pp_pb, 5, 14),
        get_bits(bpi_pp_pb, 5, 19),
    ];
    c.normal_precision = get_bits(bpi_pp_pb, 4, 24);
    c.tangent_precision = get_bits(bpi_pp_pb, 4, 28);
    c.pos_scale = precision_scale(c.pos_precision);

    // Row 4: BoxBoundsExtent(12) + Flags(u32).
    let r4 = row(4);
    c.flags = read_u32(data, r4 + 12);

    // Row 5.
    let r5 = row(5);
    let attr_off_bpa = read_u32(data, r5);
    c.attribute_offset = get_bits(attr_off_bpa, 22, 0);
    c.bits_per_attribute = get_bits(attr_off_bpa, 10, 22);
    let dio = read_u32(data, r5 + 4);
    c.decode_info_offset = get_bits(dio, 22, 0);
    c.has_tangents = get_bits(dio, 1, 22) == 1;
    c.skinning = get_bits(dio, 1, 23) == 1;
    c.num_uvs = get_bits(dio, 3, 24);
    c.color_mode = get_bits(dio, 1, 27);
    c.uv_bit_offsets = read_u32(data, r5 + 8);
    let material_encoding = read_u32(data, r5 + 12);

    // Row 6 (5.5): ExtendedData/BrickData (unused here).

    // Material assignment.
    if material_encoding < 0xFE00_0000 {
        c.material0_index = get_bits(material_encoding, 6, 0);
        c.material1_index = get_bits(material_encoding, 6, 6);
        c.material2_index = get_bits(material_encoding, 6, 12);
        c.material0_length = get_bits(material_encoding, 7, 18) + 1;
        c.material1_length = get_bits(material_encoding, 7, 25);
    } else {
        c.material_table_offset = get_bits(material_encoding, 19, 0);
        c.material_table_length = get_bits(material_encoding, 6, 19) + 1;
    }

    c
}

// ---------------------------------------------------------------------------
// Vertex attribute unpacking (UE 5.5)
// ---------------------------------------------------------------------------

pub const MAX_UVS: usize = 4;
pub const UV_FLOAT_NUM_EXPONENT_BITS: u32 = 5;
pub const UV_FLOAT_NUM_MANTISSA_BITS: u32 = 14;

/// A cluster's per-UV-channel decode range (5.4+ float layout).
#[derive(Debug, Clone, Copy, Default)]
pub struct UvRange {
    pub min: [u32; 2],
    pub num_bits: [u32; 2],
    pub num_mantissa_bits: u32,
    pub bytes_per_value: u32,
}

fn parse_uv_range(data: &[u8], off: usize) -> UvRange {
    let p0 = read_u32(data, off);
    let p1 = read_u32(data, off + 4);
    let nb = [p0 & 0x1f, p1 & 0x1f];
    UvRange {
        min: [p0 >> 5, p1 >> 5],
        num_bits: nb,
        num_mantissa_bits: UV_FLOAT_NUM_MANTISSA_BITS,
        bytes_per_value: (nb[0].max(nb[1]) + 7) / 8,
    }
}

/// Octahedral normal unpack (`FNaniteVertex.UnpackNormals`).
pub fn unpack_normals(packed: u32, bits: u32) -> [f32; 3] {
    let mask = bit_field_mask_u32(bits, 0);
    let f0 = get_bits(packed, bits, 0) as f32 * (2.0 / mask as f32) - 1.0;
    let f1 = get_bits(packed, bits, bits) as f32 * (2.0 / mask as f32) - 1.0;
    let mut n = [f0, f1, 1.0 - f0.abs() - f1.abs()];
    let t = (-n[2]).clamp(0.0, 1.0);
    n[0] += if n[0] >= 0.0 { -t } else { t };
    n[1] += if n[1] >= 0.0 { -t } else { t };
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len > 0.0 {
        n[0] /= len;
        n[1] /= len;
        n[2] /= len;
    }
    n
}

fn decode_uv_float(encoded: u32, num_mantissa_bits: u32) -> f32 {
    let exp_mant_mask = bit_field_mask_u32(UV_FLOAT_NUM_EXPONENT_BITS + num_mantissa_bits, 0);
    let b_neg = encoded <= exp_mant_mask;
    let exp_mant = (if b_neg { !encoded } else { encoded }) & exp_mant_mask;
    let result = f32::from_bits(0x3F00_0000u32 + (exp_mant << (23 - num_mantissa_bits)));
    let result = (result * 2.0 - 1.0).min(result);
    if b_neg { -result } else { result }
}

fn unpack_tex_coord(packed: [u32; 2], uv: &UvRange) -> [f32; 2] {
    let global = [packed[0] + uv.min[0], packed[1] + uv.min[1]];
    [
        decode_uv_float(global[0], uv.num_mantissa_bits),
        decode_uv_float(global[1], uv.num_mantissa_bits),
    ]
}

/// A fully decoded vertex (position in UE cm, attributes).
#[derive(Debug, Clone, Default)]
pub struct DecodedVertex {
    pub raw_pos: [i32; 3],
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub color: [u8; 4],
    pub uvs: [[f32; 2]; MAX_UVS],
    pub is_ref: bool,
}

/// A decoded cluster: triangles + vertices (ref vertices are `None` until
/// `resolve_page_references` runs).
pub struct DecodedCluster {
    pub tri_indices: Vec<[u32; 3]>,
    pub vertices: Vec<Option<DecodedVertex>>,
    pub group_ref_to_vertex: Vec<u32>,
    pub group_non_ref_to_vertex: Vec<u32>,
    pub num_uvs: u32,
}

impl Cluster {
    /// Strip-decode one triangle's three vertex indices
    /// (`FCluster.GetTriangleIndices`, UE 5.5).
    fn get_triangle_indices(
        &self,
        data: &[u8],
        page: &Page,
        cluster_index: u32,
        tri_index: u32,
    ) -> (u32, u32, u32) {
        let cdh = page.cluster_disk_headers[cluster_index as usize];
        let dword_index = tri_index >> 5;
        let bit_index = tri_index & 31;

        let sb = page.disk_header_offset
            + page.disk_header.strip_bitmask_offset as usize
            + ((cluster_index * 4 + dword_index) * 12) as usize;
        let s_mask = read_u32(data, sb);
        let l_mask = read_u32(data, sb + 4);
        let w_mask = read_u32(data, sb + 8);
        let sl_mask = s_mask & l_mask;
        let head_ref_vertex_mask = (sl_mask | !s_mask) & w_mask;

        let prev_bits_mask = (1u32 << bit_index).wrapping_sub(1);

        let num_prev_ref_before: i32 = if dword_index == 0 {
            0
        } else {
            get_bits(cdh.num_prev_ref_vertices_before_dwords, 10, dword_index * 10 - 10) as i32
        };
        let num_prev_new_before: i32 = if dword_index == 0 {
            0
        } else {
            get_bits(cdh.num_prev_new_vertices_before_dwords, 10, dword_index * 10 - 10) as i32
        };

        let cur_ref = (((sl_mask & prev_bits_mask).count_ones() << 1)
            + (w_mask & prev_bits_mask).count_ones()) as i32;
        let cur_new =
            ((s_mask & prev_bits_mask).count_ones() << 1) as i32 + bit_index as i32 - cur_ref;

        let num_prev_ref = num_prev_ref_before + cur_ref;
        let num_prev_new = num_prev_new_before + cur_new;

        let is_start = get_bits_as_signed(s_mask, 1, bit_index); // -1 true, 0 false
        let is_left = get_bits_as_signed(l_mask, 1, bit_index);
        let is_ref = get_bits_as_signed(w_mask, 1, bit_index);

        let base_vertex = (num_prev_new - 1) as u32;
        let read_base = page.disk_header_offset + cdh.index_data_offset as usize;
        // Every path through the branch below assigns each exactly once, so
        // there is no initial value to pick and none to leave stale.
        let x: u32;
        // `y`/`z` are taken by `&mut` further down, so they stay `mut`.
        let mut y: u32;
        let mut z: u32;
        let mut index_data =
            read_unaligned_dword(data, read_base, ((num_prev_ref + !is_start) * 5) as i64 as u64);

        if is_start != 0 {
            let minus_num_ref = (is_left << 1) + is_ref;
            let mut next_vertex = num_prev_new as u32;
            if minus_num_ref <= -1 {
                x = base_vertex.wrapping_sub(index_data & 31);
                index_data >>= 5;
            } else {
                x = next_vertex;
                next_vertex += 1;
            }
            if minus_num_ref <= -2 {
                y = base_vertex.wrapping_sub(index_data & 31);
                index_data >>= 5;
            } else {
                y = next_vertex;
                next_vertex += 1;
            }
            if minus_num_ref <= -3 {
                z = base_vertex.wrapping_sub(index_data & 31);
            } else {
                z = next_vertex;
            }
        } else {
            let prev_bit_index = bit_index - 1;
            let is_prev_start = get_bits_as_signed(s_mask, 1, prev_bit_index);
            let is_prev_head_ref = get_bits_as_signed(head_ref_vertex_mask, 1, prev_bit_index);
            let num_prev_new_in_tri = is_prev_start
                & (3i32
                    - (((get_bits(l_mask, 1, prev_bit_index) << 1)
                        | get_bits(w_mask, 1, prev_bit_index)) as i32));
            y = (base_vertex as i32
                + (is_prev_head_ref & (num_prev_new_in_tri - (index_data & 31) as i32)))
                as u32;
            z = (num_prev_new + (is_ref & (-1 - get_bits(index_data, 5, 5) as i32))) as u32;

            let search_mask = s_mask | (l_mask ^ is_left as u32);
            let found_bit_index = first_bit_high(search_mask & prev_bits_mask);
            let is_found_case_s = get_bits_as_signed(s_mask, 1, found_bit_index);

            let found_prev_bits_mask = (1u32 << found_bit_index).wrapping_sub(1);
            let found_cur_ref = (((sl_mask & found_prev_bits_mask).count_ones() << 1)
                + (w_mask & found_prev_bits_mask).count_ones())
                as i32;
            let found_cur_new = ((s_mask & found_prev_bits_mask).count_ones() << 1) as i32
                + found_bit_index as i32
                - found_cur_ref;

            let found_num_prev_new = num_prev_new_before + found_cur_new;
            let found_num_prev_ref = num_prev_ref_before + found_cur_ref;

            let found_num_ref =
                (get_bits(l_mask, 1, found_bit_index) << 1) + get_bits(w_mask, 1, found_bit_index);
            let is_before_found_ref =
                get_bits(head_ref_vertex_mask, 1, found_bit_index.wrapping_sub(1));

            let read_offset = if is_found_case_s != 0 { is_left } else { 1 };
            let found_index_data = read_unaligned_dword(
                data,
                read_base,
                ((found_num_prev_ref - read_offset) * 5) as i64 as u64,
            );
            let found_index =
                ((found_num_prev_new - 1) as u32).wrapping_sub(get_bits(found_index_data, 5, 0));

            let condition = if is_found_case_s != 0 {
                found_num_ref as i32 >= 1 - is_left
            } else {
                is_before_found_ref != 0
            };
            let found_new_vertex = found_num_prev_new
                + if is_found_case_s != 0 {
                    is_left & if found_num_ref == 0 { 1 } else { 0 }
                } else {
                    -1
                };
            x = if condition { found_index } else { found_new_vertex as u32 };

            if is_left != 0 {
                std::mem::swap(&mut y, &mut z);
            }
        }
        (x, y, z)
    }

    /// Decode this cluster's geometry (5.4+/5.5 path): triangle indices, the
    /// ref/non-ref vertex classification, and all non-ref vertices. Ref
    /// vertices are left `None` for `resolve_page_references`.
    pub fn decode(&self, data: &[u8], page: &Page, cluster_index: u32) -> DecodedCluster {
        let cdh = page.cluster_disk_headers[cluster_index as usize];

        // --- triangle indices (with the canonical winding rotation) ---
        let mut tri_indices = Vec::with_capacity(self.num_tris as usize);
        for tri in 0..self.num_tris {
            let (mut x, mut y, mut z) = self.get_triangle_indices(data, page, cluster_index, tri);
            if y < x.min(z) {
                (x, y, z) = (y, z, x);
            } else if z < x.min(y) {
                (x, y, z) = (z, x, y);
            }
            tri_indices.push([x, y, z]);
        }

        // --- ref / non-ref vertex classification ---
        let num_vertex_refs = cdh.num_vertex_refs;
        let num_non_ref = self.num_verts - num_vertex_refs;
        let mut group_ref_to_vertex = vec![0u32; num_vertex_refs as usize];
        let mut group_non_ref_to_vertex = vec![0u32; num_non_ref as usize];

        let aligned_bitmask_offset = page.disk_header_offset
            + page.disk_header.vertex_ref_bitmask_offset as usize
            + cluster_index as usize * 32; // NANITE_MAX_CLUSTER_VERTICES / 8
        let mut group_refs_prev = [0u32; 2];
        for group_index in 0..7u32 {
            let count =
                read_u32(data, aligned_bitmask_offset + group_index as usize * 4).count_ones();
            let count8888 = count.wrapping_mul(0x0101_0101);
            let index = group_index + 1;
            group_refs_prev[(index >> 2) as usize] =
                group_refs_prev[(index >> 2) as usize].wrapping_add(count8888 << ((index & 3) << 3));
            if self.num_verts > 128 && index < 4 {
                group_refs_prev[1] = group_refs_prev[1].wrapping_add(count8888);
            }
        }
        for vertex_index in 0..self.num_verts {
            let dword_index = vertex_index >> 5;
            let bit_index = vertex_index & 31;
            let shift = (dword_index & 3) << 3;
            let num_refs_in_prev_dwords =
                (group_refs_prev[(dword_index >> 2) as usize] >> shift) & 0xff;
            let dword_mask = read_u32(data, aligned_bitmask_offset + dword_index as usize * 4);
            let num_prev_ref = get_bits(dword_mask, bit_index, 0).count_ones() + num_refs_in_prev_dwords;
            if dword_mask & (1u32 << bit_index) != 0 {
                group_ref_to_vertex[num_prev_ref as usize] = vertex_index;
            } else {
                let num_prev_non_ref = vertex_index - num_prev_ref;
                group_non_ref_to_vertex[num_prev_non_ref as usize] = vertex_index;
            }
        }

        // --- UV ranges (from the GPU-page-relative DecodeInfoOffset) ---
        let decode_info = page.gpu_header_offset + self.decode_info_offset as usize;
        let mut uv_ranges = [UvRange::default(); MAX_UVS];
        for i in 0..self.num_uvs.min(MAX_UVS as u32) as usize {
            uv_ranges[i] = parse_uv_range(data, decode_info + i * 8);
        }

        // --- non-ref vertices via the low/mid/high SOA reader ---
        let mut vertices: Vec<Option<DecodedVertex>> = vec![None; self.num_verts as usize];
        let base = |o: u32| -> [u32; 3] {
            [
                page.disk_header_offset as u32 + cdh.low_bytes_offset + o,
                page.disk_header_offset as u32 + cdh.mid_bytes_offset + o,
                page.disk_header_offset as u32 + cdh.high_bytes_offset + o,
            ]
        };
        // Running low/mid/high increment as each stream is consumed in order.
        let mut acc = [0u32; 3];
        let add = |acc: &mut [u32; 3], inc: [u32; 3]| {
            for i in 0..3 {
                acc[i] += inc[i];
            }
        };

        let position_off = base(0);
        let position_bytes = (self.pos_bits[0].max(self.pos_bits[1]).max(self.pos_bits[2]) + 7) / 8;
        let position_mask = [
            (1i64 << self.pos_bits[0]) as i32 - 1,
            (1i64 << self.pos_bits[1]) as i32 - 1,
            (1i64 << self.pos_bits[2]) as i32 - 1,
            0,
        ];
        let mut prev_position = [
            1i32 << (self.pos_bits[0].saturating_sub(1)),
            1i32 << (self.pos_bits[1].saturating_sub(1)),
            1i32 << (self.pos_bits[2].saturating_sub(1)),
            0,
        ];
        add(&mut acc, low_mid_high_increment(position_bytes, 3 * num_non_ref));

        let normal_offsets = [
            position_off[0] + acc[0],
            position_off[1] + acc[1],
            position_off[2] + acc[2],
        ];
        let normal_bytes = (self.normal_precision + 7) / 8;
        let normal_mask = [(1i64 << self.normal_precision) as i32 - 1; 4];
        let mut prev_normal = [0i32; 4];
        add(&mut acc, low_mid_high_increment(normal_bytes, 2 * num_non_ref));

        // Tangents / color streams are skipped for extraction, but their offset
        // contribution must still advance `acc` so UV offsets land correctly.
        if self.has_tangents {
            let tangent_bytes = (self.tangent_precision + 1 + 7) / 8;
            add(&mut acc, low_mid_high_increment(tangent_bytes, num_non_ref));
        }
        let color_offsets = [
            position_off[0] + acc[0],
            position_off[1] + acc[1],
            position_off[2] + acc[2],
        ];
        let color_mask = [
            (1i64 << self.color_component_bits[0]) as i32 - 1,
            (1i64 << self.color_component_bits[1]) as i32 - 1,
            (1i64 << self.color_component_bits[2]) as i32 - 1,
            (1i64 << self.color_component_bits[3]) as i32 - 1,
        ];
        let mut prev_color = [0i32; 4];
        let color_variable = self.color_mode == VERTEX_COLOR_MODE_VARIABLE;
        if color_variable {
            add(&mut acc, low_mid_high_increment(1, 4 * num_non_ref));
        }

        let mut uv_offsets = [[0u32; 3]; MAX_UVS];
        let mut uv_prev = [[0i32; 4]; MAX_UVS];
        let mut uv_masks = [[0i32; 4]; MAX_UVS];
        for i in 0..self.num_uvs.min(MAX_UVS as u32) as usize {
            uv_offsets[i] = [
                position_off[0] + acc[0],
                position_off[1] + acc[1],
                position_off[2] + acc[2],
            ];
            uv_masks[i] = [
                (1i64 << uv_ranges[i].num_bits[0]) as i32 - 1,
                (1i64 << uv_ranges[i].num_bits[1]) as i32 - 1,
                0,
                0,
            ];
            add(
                &mut acc,
                low_mid_high_increment(uv_ranges[i].bytes_per_value, 2 * num_non_ref),
            );
        }

        let reader = LmhStreamReader::new(data);
        for nr in 0..num_non_ref {
            let mut v = DecodedVertex::default();

            let val = reader.read(position_off, position_bytes, 3, nr, &mut prev_position);
            v.raw_pos = [
                (val[0] & position_mask[0]) + self.pos_start[0],
                (val[1] & position_mask[1]) + self.pos_start[1],
                (val[2] & position_mask[2]) + self.pos_start[2],
            ];
            v.pos = [
                v.raw_pos[0] as f32 * self.pos_scale,
                v.raw_pos[1] as f32 * self.pos_scale,
                v.raw_pos[2] as f32 * self.pos_scale,
            ];

            let nval = reader.read(normal_offsets, normal_bytes, 2, nr, &mut prev_normal);
            let n0 = (nval[0] & normal_mask[0]) as u32;
            let n1 = (nval[1] & normal_mask[1]) as u32;
            let packed_normal = (n1 << self.normal_precision) | n0;
            v.normal = unpack_normals(packed_normal, self.normal_precision);

            if color_variable {
                let cval = reader.read(color_offsets, 1, 4, nr, &mut prev_color);
                v.color = [
                    ((cval[0] & color_mask[0]) + self.color_min[0]) as u8,
                    ((cval[1] & color_mask[1]) + self.color_min[1]) as u8,
                    ((cval[2] & color_mask[2]) + self.color_min[2]) as u8,
                    ((cval[3] & color_mask[3]) + self.color_min[3]) as u8,
                ];
            } else {
                v.color = [
                    self.color_min[0] as u8,
                    self.color_min[1] as u8,
                    self.color_min[2] as u8,
                    self.color_min[3] as u8,
                ];
            }

            for i in 0..self.num_uvs.min(MAX_UVS as u32) as usize {
                let uval = reader.read(
                    uv_offsets[i],
                    uv_ranges[i].bytes_per_value,
                    2,
                    nr,
                    &mut uv_prev[i],
                );
                let packed = [
                    (uval[0] & uv_masks[i][0]) as u32,
                    (uval[1] & uv_masks[i][1]) as u32,
                ];
                v.uvs[i] = unpack_tex_coord(packed, &uv_ranges[i]);
            }

            let vertex_index = group_non_ref_to_vertex[nr as usize] as usize;
            vertices[vertex_index] = Some(v);
        }

        DecodedCluster {
            tri_indices,
            vertices,
            group_ref_to_vertex,
            group_non_ref_to_vertex,
            num_uvs: self.num_uvs,
        }
    }
}

// ---------------------------------------------------------------------------
// Vertex reference resolution (Phase 5)
// ---------------------------------------------------------------------------

/// A single ref-vertex: `dest` (a vertex index in the owning cluster) copies
/// its attributes from vertex `src_vertex` of cluster `src_cluster`.
/// `parent_page_index == 0` means the source is in the *same* page; otherwise
/// it lives in a dependency page (streamed from `.ubulk`), indexed via
/// `PageDependencies[DependenciesStart + parent_page_index - 1]`.
#[derive(Debug, Clone, Copy, Default)]
pub struct VertexRef {
    pub dest: u32,
    pub parent_page_index: u32,
    pub src_cluster: u32,
    pub src_vertex: u32,
}

impl Page {
    /// Decode a cluster's vertex-ref pointers (`ResolveVertexReferences`, the
    /// pointer half — 5.4+ zig-zag-delta coded source indices).
    pub fn vertex_refs(
        &self,
        data: &[u8],
        cluster_index: usize,
        group_ref_to_vertex: &[u32],
    ) -> Vec<VertexRef> {
        let cdh = self.cluster_disk_headers[cluster_index];
        let base = self.disk_header_offset + cdh.vertex_ref_data_offset as usize;
        let coded_base = base + self.disk_header.num_vertex_refs as usize;
        let mut out = Vec::with_capacity(cdh.num_vertex_refs as usize);
        let mut prev_ref = 0i32;
        for r in 0..cdh.num_vertex_refs as usize {
            let page_cluster_index = *data.get(base + r).unwrap_or(&0) as usize;
            let page_cluster_data = read_u32(
                data,
                self.disk_header_offset
                    + cdh.page_cluster_map_offset as usize
                    + page_cluster_index * 4,
            );
            let parent_page_index = page_cluster_data >> MAX_CLUSTERS_PER_PAGE_BITS;
            let src_cluster = get_bits(page_cluster_data, MAX_CLUSTERS_PER_PAGE_BITS, 0);
            let coded = *data.get(coded_base + r).unwrap_or(&0);
            // 5.4+ zig-zag delta accumulation (wraps to a u8 vertex index).
            let temp = decode_zigzag(coded as u32) + prev_ref;
            prev_ref = temp;
            out.push(VertexRef {
                dest: group_ref_to_vertex[r],
                parent_page_index,
                src_cluster,
                src_vertex: (temp as u8) as u32,
            });
        }
        out
    }
}

/// A page decoded to geometry, plus the per-cluster ref pointers (same-page
/// refs already resolved; cross-page refs pending `resolve_cross_page`).
pub struct DecodedPage {
    pub clusters: Vec<DecodedCluster>,
    pub refs: Vec<Vec<VertexRef>>,
    pub pos_scales: Vec<f32>,
}

/// Decode every cluster in a page and resolve same-page vertex references.
pub fn decode_page(data: &[u8], page: &Page) -> DecodedPage {
    let clusters: Vec<DecodedCluster> = (0..page.clusters.len())
        .map(|i| page.clusters[i].decode(data, page, i as u32))
        .collect();
    let refs: Vec<Vec<VertexRef>> = (0..page.clusters.len())
        .map(|i| page.vertex_refs(data, i, &clusters[i].group_ref_to_vertex))
        .collect();
    let pos_scales: Vec<f32> = page.clusters.iter().map(|c| c.pos_scale).collect();
    let mut dp = DecodedPage {
        clusters,
        refs,
        pos_scales,
    };
    dp.resolve_same_page();
    dp
}

impl DecodedPage {
    /// Resolve refs whose source is in this same page. Iterated to a fixpoint
    /// so chained refs (ref → ref) settle regardless of cluster order.
    fn resolve_same_page(&mut self) {
        for _ in 0..16 {
            let mut changed = false;
            for ci in 0..self.clusters.len() {
                let pos_scale = self.pos_scales[ci];
                for r in 0..self.refs[ci].len() {
                    let vr = self.refs[ci][r];
                    if vr.parent_page_index != 0 {
                        continue; // cross-page, resolved once the source page is loaded
                    }
                    if self.clusters[ci].vertices[vr.dest as usize].is_some() {
                        continue;
                    }
                    let src = self
                        .clusters
                        .get(vr.src_cluster as usize)
                        .and_then(|c| c.vertices.get(vr.src_vertex as usize))
                        .and_then(|v| v.clone());
                    if let Some(sv) = src {
                        let mut nv = sv;
                        nv.pos = [
                            nv.raw_pos[0] as f32 * pos_scale,
                            nv.raw_pos[1] as f32 * pos_scale,
                            nv.raw_pos[2] as f32 * pos_scale,
                        ];
                        nv.is_ref = true;
                        self.clusters[ci].vertices[vr.dest as usize] = Some(nv);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Count vertices still unresolved (cross-page refs before streaming).
    pub fn unresolved(&self) -> usize {
        self.clusters
            .iter()
            .map(|c| c.vertices.iter().filter(|v| v.is_none()).count())
            .sum()
    }
}

// ---------------------------------------------------------------------------
// FNaniteResources parse + full multi-page decode (Phase 5b/6)
// ---------------------------------------------------------------------------

pub const CLUSTER_FLAG_FULL_LEAF: u32 = 0x4;

/// Per-page streaming state (5.5, 20 bytes).
#[derive(Debug, Clone, Copy, Default)]
pub struct StreamingState {
    pub bulk_offset: u32,
    pub bulk_size: u32,
    pub page_size: u32,
    pub deps_start: u32,
    pub deps_num: u16,
}

/// The parsed `FNaniteResources` block: enough to locate + decode every page.
pub struct NaniteResources {
    /// RootData byte span within the `.uasset` (inline root pages).
    pub root_data_offset: usize,
    pub root_data_len: usize,
    pub num_root_pages: u32,
    pub position_precision: i32,
    pub num_input_triangles: u32,
    pub num_clusters: u32,
    pub streaming_states: Vec<StreamingState>,
    pub page_dependencies: Vec<u32>,
}

impl NaniteResources {
    /// Parse `FNaniteResources` from a cooked `UStaticMesh` `.uasset` (5.5).
    /// Anchors on the RootData page (its first byte is the `0x464E` fixup
    /// magic, with the byte count in the preceding i32), then reads forward.
    pub fn parse(uasset: &[u8], header_size: usize) -> Option<NaniteResources> {
        for c in header_size..uasset.len().saturating_sub(6) {
            if uasset.get(c + 4) != Some(&0x4E) || uasset.get(c + 5) != Some(&0x46) {
                continue;
            }
            let root_len = i32at(uasset, c)?;
            if root_len <= 16 || root_len as usize > uasset.len() - c {
                continue;
            }
            if let Some(r) = Self::parse_from_root(uasset, c, root_len as usize) {
                return Some(r);
            }
        }
        None
    }

    fn parse_from_root(b: &[u8], c: usize, root_len: usize) -> Option<NaniteResources> {
        let root_data_offset = c + 4;
        let mut o = root_data_offset + root_len;

        // PageStreamingStates: i32 count + 20 bytes each.
        let n = i32at(b, o)?;
        if !(0..=100_000).contains(&n) {
            return None;
        }
        o += 4;
        let mut streaming_states = Vec::with_capacity(n as usize);
        for _ in 0..n {
            streaming_states.push(StreamingState {
                bulk_offset: read_u32(b, o),
                bulk_size: read_u32(b, o + 4),
                page_size: read_u32(b, o + 8),
                deps_start: read_u32(b, o + 12),
                deps_num: u16::from_le_bytes([*b.get(o + 16)?, *b.get(o + 17)?]),
            });
            o += 20;
        }
        // HierarchyNodes: i32 count + 208 bytes each.
        let n = i32at(b, o)?;
        if !(0..=1_000_000).contains(&n) {
            return None;
        }
        o += 4 + n as usize * 208;
        // HierarchyRootOffsets: i32 count + 4 bytes each.
        let n = i32at(b, o)?;
        if !(0..=1_000_000).contains(&n) {
            return None;
        }
        o += 4 + n as usize * 4;
        // PageDependencies: i32 count + u32 each.
        let n = i32at(b, o)?;
        if !(0..=10_000_000).contains(&n) {
            return None;
        }
        o += 4;
        let mut page_dependencies = Vec::with_capacity(n as usize);
        for _ in 0..n {
            page_dependencies.push(read_u32(b, o));
            o += 4;
        }
        // ImposterAtlas: i32 count + u16 each.
        let n = i32at(b, o)?;
        if !(0..=10_000_000).contains(&n) {
            return None;
        }
        o += 4 + n as usize * 2;

        let num_root_pages = i32at(b, o)?;
        let position_precision = i32at(b, o + 4)?;
        let _normal_precision = i32at(b, o + 8)?;
        let num_input_triangles = read_u32(b, o + 12);
        let num_clusters = read_u32(b, o + 24);
        if !(0..=64).contains(&num_root_pages)
            || !(-20..=43).contains(&position_precision)
            || num_input_triangles == 0
            || num_input_triangles > 50_000_000
        {
            return None;
        }
        Some(NaniteResources {
            root_data_offset,
            root_data_len: root_len,
            num_root_pages: num_root_pages as u32,
            position_precision,
            num_input_triangles,
            num_clusters,
            streaming_states,
            page_dependencies,
        })
    }
}

#[inline]
fn i32at(b: &[u8], o: usize) -> Option<i32> {
    b.get(o..o + 4).map(|s| i32::from_le_bytes(s.try_into().unwrap()))
}

/// One page fully parsed + decoded (same-page refs resolved).
struct PageBundle {
    page: Page,
    decoded: DecodedPage,
}

/// A fully-assembled high-resolution Nanite mesh (finest LOD, FULL_LEAF cut).
#[derive(Default)]
pub struct NaniteMesh {
    /// Positions in UE centimeters.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    /// Triangles as position-index triples.
    pub triangles: Vec<[u32; 3]>,
    pub unresolved_vertices: usize,
}

/// Decode a whole Nanite mesh: every page (root inline + streaming from the
/// `.ubulk`), resolve all vertex references, and assemble the FULL_LEAF
/// clusters into a single high-resolution mesh.
pub fn decode_nanite(uasset: &[u8], ubulk: &[u8], res: &NaniteResources) -> NaniteMesh {
    let root = &uasset[res.root_data_offset..res.root_data_offset + res.root_data_len];

    // 1. Parse + decode every page (same-page refs resolved inside decode_page).
    let mut pages: Vec<Option<PageBundle>> = Vec::with_capacity(res.streaming_states.len());
    for (i, st) in res.streaming_states.iter().enumerate() {
        let (buf, start) = if (i as u32) < res.num_root_pages {
            (root, st.bulk_offset as usize)
        } else {
            (ubulk, st.bulk_offset as usize)
        };
        let bundle = Page::parse(buf, start).map(|page| {
            let decoded = decode_page(buf, &page);
            PageBundle { page, decoded }
        });
        pages.push(bundle);
    }

    // 2. Resolve cross-page references to a fixpoint (chained refs settle).
    for _ in 0..32 {
        let mut changed = false;
        for pi in 0..pages.len() {
            let (deps_start, n_clusters) = match &pages[pi] {
                Some(b) => (
                    res.streaming_states[pi].deps_start as usize,
                    b.decoded.clusters.len(),
                ),
                None => continue,
            };
            for ci in 0..n_clusters {
                let pos_scale = pages[pi].as_ref().unwrap().page.clusters[ci].pos_scale;
                let refs = pages[pi].as_ref().unwrap().decoded.refs[ci].clone();
                for vr in refs {
                    if vr.parent_page_index == 0 {
                        continue;
                    }
                    // Already resolved?
                    if pages[pi].as_ref().unwrap().decoded.clusters[ci].vertices
                        [vr.dest as usize]
                        .is_some()
                    {
                        continue;
                    }
                    let dep_idx = deps_start + (vr.parent_page_index - 1) as usize;
                    let Some(&global_page) = res.page_dependencies.get(dep_idx) else {
                        continue;
                    };
                    let src = pages
                        .get(global_page as usize)
                        .and_then(|p| p.as_ref())
                        .and_then(|p| p.decoded.clusters.get(vr.src_cluster as usize))
                        .and_then(|c| c.vertices.get(vr.src_vertex as usize))
                        .and_then(|v| v.clone());
                    if let Some(sv) = src {
                        let mut nv = sv;
                        nv.pos = [
                            nv.raw_pos[0] as f32 * pos_scale,
                            nv.raw_pos[1] as f32 * pos_scale,
                            nv.raw_pos[2] as f32 * pos_scale,
                        ];
                        nv.is_ref = true;
                        pages[pi].as_mut().unwrap().decoded.clusters[ci].vertices
                            [vr.dest as usize] = Some(nv);
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    // 3. Assemble the FULL_LEAF clusters (finest complete cut of the DAG).
    let mut mesh = NaniteMesh::default();
    for bundle in pages.iter().flatten() {
        for (ci, cl) in bundle.page.clusters.iter().enumerate() {
            if cl.flags & CLUSTER_FLAG_FULL_LEAF == 0 {
                continue;
            }
            let dc = &bundle.decoded.clusters[ci];
            let base = mesh.positions.len() as u32;
            for v in &dc.vertices {
                match v {
                    Some(v) => {
                        mesh.positions.push(v.pos);
                        mesh.normals.push(v.normal);
                        mesh.uvs.push(v.uvs[0]);
                    }
                    None => {
                        mesh.unresolved_vertices += 1;
                        mesh.positions.push([0.0; 3]);
                        mesh.normals.push([0.0; 3]);
                        mesh.uvs.push([0.0; 2]);
                    }
                }
            }
            for t in &dc.tri_indices {
                mesh.triangles.push([base + t[0], base + t[1], base + t[2]]);
            }
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_primitives() {
        assert_eq!(get_bits(0b1011_0100, 4, 0), 0b0100);
        assert_eq!(get_bits(0b1011_0100, 4, 4), 0b1011);
        assert_eq!(get_bits_as_signed(0b1111, 4, 0), -1);
        assert_eq!(get_bits_as_signed(0b0111, 4, 0), 7);
        assert_eq!(decode_zigzag(0), 0);
        assert_eq!(decode_zigzag(1), -1);
        assert_eq!(decode_zigzag(2), 1);
        assert_eq!(decode_zigzag(3), -2);
        assert_eq!(first_bit_high(0), u32::MAX);
        assert_eq!(first_bit_high(1), 0);
        assert_eq!(first_bit_high(0b10000), 4);
    }

    #[test]
    fn precision_scales() {
        assert_eq!(precision_scale(0), 1.0);
        assert_eq!(precision_scale(1), 0.5);
        assert_eq!(precision_scale(-1), 2.0);
        assert_eq!(precision_scale(3), 0.125);
        assert_eq!(precision_scale(-20), (1u32 << 20) as f32);
    }

    #[test]
    fn bit_align() {
        // shift 0 → low unchanged.
        assert_eq!(bit_align_u32(0xFFFF_FFFF, 0x1234_5678, 0), 0x1234_5678);
        // shift 4 → low >> 4 with high's low nibble on top.
        assert_eq!(bit_align_u32(0x0000_000F, 0x1234_5678, 4), 0xF123_4567);
    }

    #[test]
    fn unaligned_dword() {
        // 8 bytes; read a dword starting 4 bits in.
        let data = [0x78, 0x56, 0x34, 0x12, 0xF0, 0xDE, 0xBC, 0x9A];
        // bytes as one 64-bit LE value: 0x9ABCDEF012345678; >>4 = 0x9ABCDEF01234567
        assert_eq!(read_unaligned_dword(&data, 0, 4), 0x0123_4567);
    }
}
