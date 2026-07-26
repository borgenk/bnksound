//! PNG encoding for screenshots.
//!
//! Frames are ARGB8888 words with an opaque alpha, so they go out as 8-bit RGB.
//! The zlib stream uses deflate's fixed Huffman codes over an LZ77 pass, which
//! is a large win on a flat UI (long runs of one colour) for a small amount of
//! code, and needs no code-length table in the output.

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Encode `pixels` (row-major, `width` words per row) as an 8-bit RGB PNG.
pub fn encode_rgb(pixels: &[u32], width: u32, height: u32) -> Vec<u8> {
    let raw = scanlines(pixels, width, height);
    let mut out = Vec::from(SIGNATURE);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 8 bits per sample, color type 2 (truecolor), no compression/filter/
    // interlace variation beyond the one PNG defines.
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

/// The raw PNG image data: every row prefixed with filter type 0 (none).
fn scanlines(pixels: &[u32], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut raw = Vec::with_capacity(h * (1 + w * 3));
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let px = pixels.get(y * w + x).copied().unwrap_or(0);
            raw.push((px >> 16) as u8);
            raw.push((px >> 8) as u8);
            raw.push(px as u8);
        }
    }
    raw
}

/// Wrap `data` in a zlib stream: one fixed-Huffman deflate block over an LZ77
/// pass, then the Adler checksum.
fn zlib(data: &[u8]) -> Vec<u8> {
    // CMF 0x78 (deflate, 32K window) and FLG 0x01, whose check bits make the
    // pair a multiple of 31.
    let mut w = BitWriter::new(vec![0x78, 0x01]);
    w.bits(1, 1); // final block
    w.bits(1, 2); // fixed Huffman codes
    deflate_fixed(data, &mut w);
    let mut out = w.finish();
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Bits go out least significant first, except Huffman codes, which deflate
/// packs most significant first.
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn new(out: Vec<u8>) -> Self {
        BitWriter { out, acc: 0, n: 0 }
    }

    fn bits(&mut self, value: u32, count: u32) {
        self.acc |= (value & ((1 << count) - 1)) << self.n;
        self.n += count;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, whose bits are defined most significant first.
    fn code(&mut self, code: u32, count: u32) {
        for i in (0..count).rev() {
            self.bits((code >> i) & 1, 1);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// Emit one literal or length symbol in deflate's fixed code.
fn fixed_symbol(w: &mut BitWriter, sym: u16) {
    match sym {
        0..=143 => w.code(0x30 + u32::from(sym), 8),
        144..=255 => w.code(0x190 + u32::from(sym) - 144, 9),
        256..=279 => w.code(u32::from(sym) - 256, 7),
        _ => w.code(0xc0 + u32::from(sym) - 280, 8),
    }
}

/// Greedy LZ77 over a hash of the next three bytes, emitted in the fixed code.
fn deflate_fixed(data: &[u8], w: &mut BitWriter) {
    const WINDOW: usize = 32768;
    const MIN_MATCH: usize = 3;
    const MAX_MATCH: usize = 258;
    /// How far back along a hash chain to look. Bounds the worst case on
    /// flat images, where one hash can cover a very long run.
    const MAX_CHAIN: usize = 32;

    let mut heads = vec![usize::MAX; 1 << 15];
    let mut prev = vec![usize::MAX; data.len().max(1)];
    let hash = |d: &[u8], i: usize| -> usize {
        ((usize::from(d[i]) << 10) ^ (usize::from(d[i + 1]) << 5) ^ usize::from(d[i + 2]))
            & ((1 << 15) - 1)
    };

    let mut i = 0;
    while i < data.len() {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + MIN_MATCH <= data.len() {
            let h = hash(data, i);
            let mut candidate = heads[h];
            let mut walked = 0;
            while candidate != usize::MAX && walked < MAX_CHAIN {
                let dist = i - candidate;
                if dist > WINDOW {
                    break;
                }
                let max = MAX_MATCH.min(data.len() - i);
                let mut len = 0;
                while len < max && data[candidate + len] == data[i + len] {
                    len += 1;
                }
                if len > best_len {
                    best_len = len;
                    best_dist = dist;
                    if len == MAX_MATCH {
                        break;
                    }
                }
                candidate = prev[candidate];
                walked += 1;
            }
            prev[i] = heads[h];
            heads[h] = i;
        }

        if best_len >= MIN_MATCH {
            let li = LENGTH_BASE
                .iter()
                .rposition(|&b| usize::from(b) <= best_len)
                .unwrap_or(0);
            fixed_symbol(w, 257 + li as u16);
            w.bits(
                (best_len - usize::from(LENGTH_BASE[li])) as u32,
                u32::from(LENGTH_EXTRA[li]),
            );
            let di = DIST_BASE
                .iter()
                .rposition(|&b| usize::from(b) <= best_dist)
                .unwrap_or(0);
            w.code(di as u32, 5);
            w.bits(
                (best_dist - usize::from(DIST_BASE[di])) as u32,
                u32::from(DIST_EXTRA[di]),
            );
            // Index the bytes the match covered so later matches can find them.
            let mut k = i + 1;
            while k < i + best_len && k + MIN_MATCH <= data.len() {
                let h = hash(data, k);
                prev[k] = heads[h];
                heads[h] = k;
                k += 1;
            }
            i += best_len;
        } else {
            fixed_symbol(w, u16::from(data[i]));
            i += 1;
        }
    }
    fixed_symbol(w, 256);
}

/// Append a length-tagged, CRC-checked PNG chunk.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// Running CRC-32, computed bitwise so there is no table to carry.
struct Crc(u32);

impl Crc {
    fn new() -> Self {
        Crc(0xffff_ffff)
    }

    fn update(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u32::from(b);
            for _ in 0..8 {
                // The reflected CRC-32 polynomial.
                let mask = (self.0 & 1).wrapping_neg();
                self.0 = (self.0 >> 1) ^ (0xedb8_8320 & mask);
            }
        }
    }

    fn finish(self) -> u32 {
        !self.0
    }
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    // Chunked so the accumulators cannot overflow before the reduction.
    for block in data.chunks(5552) {
        for &byte in block {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the low 24 bits reach the file; the alpha byte is dropped.
    #[test]
    fn pixels_are_written_as_rgb_in_order() {
        let png = encode_rgb(&[0xff11_2233, 0xff44_5566], 2, 1);
        let back = decode(&png).expect("decode");
        assert_eq!(back.pixels, vec![0xff11_2233, 0xff44_5566]);
    }

    /// A flat image is mostly repeats, which is exactly what LZ77 is for. If
    /// this stops holding, the encoder has quietly stopped compressing.
    #[test]
    fn a_flat_image_compresses_far_below_its_raw_size() {
        let (w, h) = (256u32, 256u32);
        let png = encode_rgb(&vec![0xff20_3040; (w * h) as usize], w, h);
        let raw = (w * h * 3) as usize;
        assert!(
            png.len() < raw / 20,
            "expected heavy compression, got {} bytes for {raw} raw",
            png.len()
        );
        let back = decode(&png).expect("decode");
        assert!(back.pixels.iter().all(|&p| p == 0xff20_3040));
    }

    /// Photographic-ish noise cannot compress much, but must still round-trip:
    /// that is where the match finder and the literal path both get used.
    #[test]
    fn noisy_data_round_trips_even_when_it_cannot_compress() {
        let pixels: Vec<u32> = (0..97u32 * 61)
            .map(|i| 0xff00_0000 | i.wrapping_mul(2_654_435_761) & 0x00ff_ffff)
            .collect();
        let png = encode_rgb(&pixels, 97, 61);
        assert_eq!(decode(&png).expect("decode").pixels, pixels);
    }

    /// The two check values PNG relies on, against vectors from their specs.
    #[test]
    fn crc_and_adler_match_known_vectors() {
        let mut crc = Crc::new();
        crc.update(b"IEND");
        assert_eq!(crc.finish(), 0xae42_6082, "IEND's CRC is fixed by the spec");

        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn the_header_describes_the_image() {
        let png = encode_rgb(&[0xffff_0000; 6], 3, 2);
        assert_eq!(&png[..8], &SIGNATURE);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[16..20], &3u32.to_be_bytes());
        assert_eq!(&png[20..24], &2u32.to_be_bytes());
        // 8-bit truecolor.
        assert_eq!(&png[24..26], &[8, 2]);
        assert!(png.ends_with(&[0xae, 0x42, 0x60, 0x82]), "IEND closes it");
    }

    /// Every chunk's declared length and CRC must agree with its bytes, or a
    /// decoder rejects the file. Walk the whole stream and check both.
    #[test]
    fn every_chunk_is_well_formed_and_they_appear_in_order() {
        let png = encode_rgb(&[0xff01_0203, 0xff04_0506], 2, 1);
        let mut at = SIGNATURE.len();
        let mut kinds = Vec::new();
        while at < png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().expect("len")) as usize;
            let kind = &png[at + 4..at + 8];
            let data = &png[at + 8..at + 8 + len];
            let want =
                u32::from_be_bytes(png[at + 8 + len..at + 12 + len].try_into().expect("crc"));
            let mut crc = Crc::new();
            crc.update(kind);
            crc.update(data);
            assert_eq!(crc.finish(), want, "CRC for {:?}", str::from_utf8(kind));
            kinds.push(String::from_utf8_lossy(kind).into_owned());
            at += 12 + len;
        }
        assert_eq!(at, png.len(), "chunks tile the file exactly");
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
    }

    /// Our own encoder uses stored blocks; the decoder must read them back
    /// exactly, or the golden-image comparison is measuring nothing.
    #[test]
    fn a_frame_survives_an_encode_decode_round_trip() {
        let pixels: Vec<u32> = (0..64 * 40)
            .map(|i: u32| 0xff00_0000 | (i.wrapping_mul(2_654_435_761) & 0x00ff_ffff))
            .collect();
        let png = encode_rgb(&pixels, 64, 40);
        let back = decode(&png).expect("decode our own output");
        assert_eq!((back.width, back.height), (64, 40));
        assert_eq!(back.pixels, pixels, "every pixel round-trips");
    }

    /// The reference screenshot is a real deflate-compressed PNG with dynamic
    /// Huffman blocks and per-row filters, which is what exercises inflate.
    #[test]
    fn the_reference_screenshot_decodes() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/screenshot.png");
        let bytes = std::fs::read(path).expect("reference screenshot");
        let img = decode(&bytes).expect("decode reference");
        assert_eq!(img.pixels.len(), (img.width * img.height) as usize);
        assert!(img.width > 100 && img.height > 100, "a real image");
        // A screenshot of a dark UI: opaque, and mostly dark.
        assert!(img.pixels.iter().all(|p| p >> 24 == 0xff), "opaque");
        let dark = img
            .pixels
            .iter()
            .filter(|p| ((*p >> 16) & 0xff) < 0x60)
            .count();
        assert!(dark > img.pixels.len() / 2, "a dark UI decodes as dark");
    }

    #[test]
    fn junk_and_unsupported_forms_are_rejected_not_guessed_at() {
        assert_eq!(decode(b"not a png at all").err(), Some(DecodeError::NotPng));
        let mut png = encode_rgb(&[0xff11_2233], 1, 1);
        // Corrupt the IHDR CRC.
        let n = png.len();
        png[24] ^= 0xff;
        assert_eq!(decode(&png).err(), Some(DecodeError::BadChecksum));
        assert_eq!(png.len(), n);
        // Truncated mid-chunk.
        let png = encode_rgb(&[0xff11_2233], 1, 1);
        assert_eq!(decode(&png[..20]).err(), Some(DecodeError::Truncated));
    }

    /// A PNG assembled chunk by chunk. The encoder only writes 8-bit
    /// truecolour, so the forms icon themes ship need building by hand. Rows
    /// are already packed scanlines and go out with filter type 0.
    /// A header claiming an impossible size must be refused before anything is
    /// sized from it. The file is a few bytes; believing it would ask for an
    /// allocation in the terabytes, and the arithmetic that gets there would
    /// have wrapped on the way.
    #[test]
    fn an_absurd_header_is_refused_rather_than_allocated_for() {
        for (width, height) in [(u32::MAX, u32::MAX), (100_000, 1), (1, 100_000)] {
            let png = build_png(width, height, 8, 2, &[], &[], &[vec![0, 0, 0]]);
            assert_eq!(
                decode(&png).err(),
                Some(DecodeError::TooLarge { width, height }),
                "{width}x{height} should be refused",
            );
        }
        // The largest size that is still read stays readable.
        let png = build_png(1, 1, 8, 2, &[], &[], &[vec![1, 2, 3]]);
        assert!(decode(&png).is_ok());
    }

    /// A stream that keeps producing after the header's worth of pixels is
    /// malformed. Following it lets a tiny file expand without bound, so it is
    /// stopped at what the header described.
    #[test]
    fn a_stream_that_outruns_its_header_is_refused() {
        // A 1x1 truecolour image is four bytes of scanline; hand it far more.
        let mut png = Vec::from(SIGNATURE);
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&1u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(&mut png, b"IDAT", &zlib(&vec![0u8; 64 * 1024]));
        chunk(&mut png, b"IEND", &[]);

        assert_eq!(decode(&png).err(), Some(DecodeError::Deflate));
    }

    fn build_png(
        width: u32,
        height: u32,
        depth: u8,
        color: u8,
        plte: &[u8],
        trns: &[u8],
        rows: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut out = Vec::from(SIGNATURE);
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[depth, color, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &ihdr);
        if !plte.is_empty() {
            chunk(&mut out, b"PLTE", plte);
        }
        if !trns.is_empty() {
            chunk(&mut out, b"tRNS", trns);
        }
        let mut raw = Vec::new();
        for row in rows {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        chunk(&mut out, b"IDAT", &zlib(&raw));
        chunk(&mut out, b"IEND", &[]);
        out
    }

    #[test]
    fn a_palette_image_expands_through_plte_and_trns() {
        // Two entries, the first marked fully transparent.
        let plte = [0xff, 0x00, 0x00, 0x00, 0x80, 0x40];
        let png = build_png(2, 1, 8, 3, &plte, &[0x00], &[vec![0, 1]]);
        assert_eq!(
            decode(&png).expect("decode").pixels,
            vec![0x00ff_0000, 0xff00_8040]
        );
    }

    #[test]
    fn a_four_bit_palette_unpacks_two_pixels_per_byte() {
        let plte = [0, 0, 0, 0x11, 0, 0, 0x22, 0, 0, 0x33, 0, 0];
        // Indices 0,1,2,3, high nibble first.
        let png = build_png(4, 1, 4, 3, &plte, &[], &[vec![0x01, 0x23]]);
        assert_eq!(
            decode(&png).expect("decode").pixels,
            vec![0xff00_0000, 0xff11_0000, 0xff22_0000, 0xff33_0000]
        );
    }

    #[test]
    fn a_one_bit_palette_unpacks_eight_pixels_per_byte() {
        let plte = [0, 0, 0, 0xff, 0xff, 0xff];
        let png = build_png(8, 1, 1, 3, &plte, &[], &[vec![0b1010_0101]]);
        let (w, b) = (0xffff_ffffu32, 0xff00_0000u32);
        assert_eq!(
            decode(&png).expect("decode").pixels,
            vec![w, b, w, b, b, w, b, w]
        );
    }

    #[test]
    fn a_palette_index_past_the_table_draws_as_nothing() {
        let png = build_png(2, 1, 8, 3, &[0xff, 0, 0], &[], &[vec![0, 7]]);
        assert_eq!(
            decode(&png).expect("decode").pixels,
            vec![0xffff_0000, 0x0000_0000]
        );
    }

    #[test]
    fn a_palette_image_without_plte_is_rejected() {
        let png = build_png(1, 1, 8, 3, &[], &[], &[vec![0]]);
        assert_eq!(decode(&png).err(), Some(DecodeError::MissingPalette));
    }

    #[test]
    fn low_bit_depth_greys_scale_onto_the_full_range() {
        // Depth 4: 0x0 is black, 0xf is white, 0x8 lands near the middle.
        let png = build_png(3, 1, 4, 0, &[], &[], &[vec![0x0f, 0x80]]);
        let img = decode(&png).expect("decode");
        assert_eq!(img.pixels[0] & 0xff, 0);
        assert_eq!(img.pixels[1] & 0xff, 255);
        assert_eq!(img.pixels[2] & 0xff, 136);
    }

    #[test]
    fn sixteen_bit_samples_narrow_to_their_high_byte() {
        let png = build_png(
            1,
            1,
            16,
            2,
            &[],
            &[],
            &[vec![0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]],
        );
        assert_eq!(decode(&png).expect("decode").pixels, vec![0xff12_569a]);
    }

    #[test]
    fn grey_plus_alpha_carries_its_own_alpha() {
        let png = build_png(2, 1, 8, 4, &[], &[], &[vec![0x40, 0xff, 0x80, 0x00]]);
        assert_eq!(
            decode(&png).expect("decode").pixels,
            vec![0xff40_4040, 0x0080_8080]
        );
    }

    #[test]
    fn a_truecolour_transparency_key_clears_matching_pixels() {
        // tRNS names pure red, stored as three 16-bit samples.
        let trns = [0x00, 0xff, 0x00, 0x00, 0x00, 0x00];
        let png = build_png(2, 1, 8, 2, &[], &trns, &[vec![0xff, 0, 0, 0, 0, 0xff]]);
        assert_eq!(
            decode(&png).expect("decode").pixels,
            vec![0x00ff_0000, 0xff00_00ff]
        );
    }

    #[test]
    fn depths_a_colour_type_does_not_allow_are_rejected() {
        // Truecolour has no four-bit form.
        let png = build_png(1, 1, 4, 2, &[], &[], &[vec![0]]);
        assert_eq!(
            decode(&png).err(),
            Some(DecodeError::Unsupported { depth: 4, color: 2 })
        );
    }
}

// --- Decoding ------------------------------------------------------------

/// A decoded image: ARGB8888 words, row-major.
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u32>,
}

/// Why a PNG could not be read.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    NotPng,
    Truncated,
    BadChecksum,
    /// A bit depth the format does not pair with this colour type.
    Unsupported {
        depth: u8,
        color: u8,
    },
    /// A palette image whose PLTE chunk never arrived.
    MissingPalette,
    /// Interlaced images; icon themes and the mixer both avoid them.
    Interlaced,
    /// A header claiming more pixels than any icon has. Believing it would ask
    /// for an allocation on a stranger's say-so.
    TooLarge {
        width: u32,
        height: u32,
    },
    Deflate,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPng => f.write_str("not a PNG"),
            Self::Truncated => f.write_str("file ends mid-chunk"),
            Self::BadChecksum => f.write_str("chunk CRC mismatch"),
            Self::Unsupported { depth, color } => {
                write!(f, "unsupported PNG: {depth}-bit colour type {color}")
            }
            Self::MissingPalette => f.write_str("palette image with no PLTE chunk"),
            Self::Interlaced => f.write_str("interlaced PNGs are not read"),
            Self::TooLarge { width, height } => {
                write!(f, "image too large to decode: {width}x{height}")
            }
            Self::Deflate => f.write_str("malformed deflate stream"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// The pixel layout an IHDR describes.
#[derive(Clone, Copy)]
struct Format {
    depth: u8,
    color: u8,
    /// Samples per pixel: one for grey or a palette index, two for grey plus
    /// alpha, three for RGB, four for RGBA.
    samples: usize,
}

impl Format {
    fn new(depth: u8, color: u8) -> Result<Self, DecodeError> {
        // Each colour type allows only some depths.
        let paired = match color {
            0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
            3 => matches!(depth, 1 | 2 | 4 | 8),
            2 | 4 | 6 => matches!(depth, 8 | 16),
            _ => false,
        };
        if !paired {
            return Err(DecodeError::Unsupported { depth, color });
        }
        let samples = match color {
            0 | 3 => 1,
            4 => 2,
            2 => 3,
            _ => 4,
        };
        Ok(Format {
            depth,
            color,
            samples,
        })
    }

    /// Bytes per scanline, rounded up: depths below eight pack several samples
    /// into a byte and a row ends on a byte boundary.
    fn stride(&self, width: u32) -> usize {
        (width as usize * self.samples * self.depth as usize).div_ceil(8)
    }

    /// How far back a filter reaches, in whole bytes, never less than one.
    fn filter_bpp(&self) -> usize {
        (self.samples * self.depth as usize / 8).max(1)
    }
}

/// PLTE entries as ARGB, with tRNS supplying alpha for the leading entries it
/// covers. Entries past the end of tRNS are opaque.
fn build_palette(plte: &[u8], trns: &[u8]) -> Result<Vec<u32>, DecodeError> {
    if plte.is_empty() {
        return Err(DecodeError::MissingPalette);
    }
    Ok(plte
        .chunks_exact(3)
        .enumerate()
        .map(|(i, c)| pack(trns.get(i).copied().unwrap_or(0xff), c[0], c[1], c[2]))
        .collect())
}

/// The one sample value tRNS marks fully transparent in grey and truecolour
/// images. The chunk stores each as sixteen bits whatever the depth, so the
/// comparison happens before samples are scaled down.
fn color_key(color: u8, trns: &[u8]) -> Option<[u16; 3]> {
    let be = |i: usize| u16::from(trns[i * 2]) << 8 | u16::from(trns[i * 2 + 1]);
    match color {
        0 if trns.len() >= 2 => Some([be(0); 3]),
        2 if trns.len() >= 6 => Some([be(0), be(1), be(2)]),
        _ => None,
    }
}

/// One raw sample from a scanline. `index` counts samples, not pixels or bytes;
/// depths below eight pack several into a byte, most significant first.
fn raw_sample(line: &[u8], depth: u8, index: usize) -> u16 {
    let byte = |i: usize| u16::from(line.get(i).copied().unwrap_or(0));
    match depth {
        16 => byte(index * 2) << 8 | byte(index * 2 + 1),
        8 => byte(index),
        _ => {
            let per_byte = 8 / depth as usize;
            let shift = 8 - depth as usize * (index % per_byte + 1);
            (byte(index / per_byte) >> shift) & ((1u16 << depth) - 1)
        }
    }
}

/// A raw sample scaled to eight bits, so every depth lands on one range.
fn scale8(v: u16, depth: u8) -> u8 {
    match depth {
        16 => (v >> 8) as u8,
        8 => v as u8,
        _ => {
            let max = (1u16 << depth) - 1;
            ((v * 255 + max / 2) / max) as u8
        }
    }
}

fn pack(a: u8, r: u8, g: u8, b: u8) -> u32 {
    u32::from(a) << 24 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
}

/// Widest or tallest image the decoder will build. Icon themes top out in the
/// hundreds of pixels; past this the header is corrupt or hostile, and the
/// arithmetic that sizes the buffers stops being trustworthy.
const MAX_DIMENSION: u32 = 16_384;

/// Decode a non-interlaced PNG into ARGB words. Every colour type is read at
/// each depth the format allows it, with PLTE and tRNS applied.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    if bytes.len() < 8 || bytes[..8] != SIGNATURE {
        return Err(DecodeError::NotPng);
    }
    let (mut width, mut height) = (0u32, 0u32);
    let (mut depth, mut color) = (0u8, 0u8);
    let (mut plte, mut trns) = (Vec::new(), Vec::new());
    let mut header = false;
    let mut idat = Vec::new();
    let mut at = 8;

    while at + 8 <= bytes.len() {
        let len = u32::from_be_bytes(
            bytes[at..at + 4]
                .try_into()
                .map_err(|_| DecodeError::Truncated)?,
        ) as usize;
        let kind = &bytes[at + 4..at + 8];
        let end = at + 8 + len;
        if end + 4 > bytes.len() {
            return Err(DecodeError::Truncated);
        }
        let data = &bytes[at + 8..end];

        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        let want = u32::from_be_bytes(
            bytes[end..end + 4]
                .try_into()
                .map_err(|_| DecodeError::Truncated)?,
        );
        if crc.finish() != want {
            return Err(DecodeError::BadChecksum);
        }

        match kind {
            b"IHDR" => {
                if data.len() < 13 {
                    return Err(DecodeError::Truncated);
                }
                width = u32::from_be_bytes(data[0..4].try_into().unwrap_or_default());
                height = u32::from_be_bytes(data[4..8].try_into().unwrap_or_default());
                depth = data[8];
                color = data[9];
                if data[12] != 0 {
                    return Err(DecodeError::Interlaced);
                }
                header = true;
            }
            b"PLTE" => plte = data.to_vec(),
            b"tRNS" => trns = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        at = end + 4;
    }

    if !header || width == 0 || height == 0 {
        return Err(DecodeError::Truncated);
    }
    if width > MAX_DIMENSION || height > MAX_DIMENSION {
        return Err(DecodeError::TooLarge { width, height });
    }
    // Format first, so an impossible depth reports as such rather than as a
    // missing palette.
    let fmt = Format::new(depth, color)?;
    let palette = if color == 3 {
        Some(build_palette(&plte, &trns)?)
    } else {
        None
    };

    let expected = (height as usize) * (fmt.stride(width) + 1);
    let raw = inflate(&idat, expected)?;
    unfilter(
        &raw,
        width,
        height,
        fmt,
        palette.as_deref(),
        color_key(color, &trns),
    )
}

/// Reverse the per-scanline filters and pack the result into ARGB words.
fn unfilter(
    raw: &[u8],
    width: u32,
    height: u32,
    fmt: Format,
    palette: Option<&[u32]>,
    key: Option<[u16; 3]>,
) -> Result<Decoded, DecodeError> {
    let (w, h) = (width as usize, height as usize);
    let stride = fmt.stride(width);
    let channels = fmt.filter_bpp();
    if raw.len() < h * (stride + 1) {
        return Err(DecodeError::Truncated);
    }

    let mut out = vec![0u32; w * h];
    let mut prev = vec![0u8; stride];
    let mut line = vec![0u8; stride];

    for y in 0..h {
        let start = y * (stride + 1);
        let filter = raw[start];
        line.copy_from_slice(&raw[start + 1..start + 1 + stride]);

        for i in 0..stride {
            // The byte `channels` back on this row, and the one above it.
            let a = if i >= channels {
                u32::from(line[i - channels])
            } else {
                0
            };
            let b = u32::from(prev[i]);
            let c = if i >= channels {
                u32::from(prev[i - channels])
            } else {
                0
            };
            let x = u32::from(line[i]);
            line[i] = match filter {
                0 => x,
                1 => x + a,
                2 => x + b,
                3 => x + (a + b) / 2,
                4 => x + paeth(a, b, c),
                _ => return Err(DecodeError::Deflate),
            } as u8;
        }

        let depth = fmt.depth;
        for x in 0..w {
            let base = x * fmt.samples;
            let s = |i: usize| raw_sample(&line, depth, base + i);
            let to8 = |v: u16| scale8(v, depth);
            out[y * w + x] = match fmt.color {
                // An index outside the palette has no colour to stand for, so
                // it draws as nothing rather than as an arbitrary entry.
                3 => palette
                    .and_then(|p| p.get(s(0) as usize).copied())
                    .unwrap_or(0),
                0 | 2 => {
                    let (r, g, b) = if fmt.color == 0 {
                        (s(0), s(0), s(0))
                    } else {
                        (s(0), s(1), s(2))
                    };
                    let a = if key == Some([r, g, b]) { 0 } else { 0xff };
                    pack(a, to8(r), to8(g), to8(b))
                }
                4 => {
                    let v = to8(s(0));
                    pack(to8(s(1)), v, v, v)
                }
                _ => pack(to8(s(3)), to8(s(0)), to8(s(1)), to8(s(2))),
            };
        }
        prev.copy_from_slice(&line);
    }

    Ok(Decoded {
        width,
        height,
        pixels: out,
    })
}

/// The Paeth predictor: whichever of the three neighbours the initial estimate
/// lands nearest.
fn paeth(a: u32, b: u32, c: u32) -> u32 {
    let p = a as i32 + b as i32 - c as i32;
    let (pa, pb, pc) = (
        (p - a as i32).abs(),
        (p - b as i32).abs(),
        (p - c as i32).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// --- Inflate -------------------------------------------------------------

/// A little-endian bit reader over the deflate stream.
struct Bits<'a> {
    data: &'a [u8],
    byte: usize,
    bit: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            byte: 0,
            bit: 0,
        }
    }

    /// Read `n` bits, least significant first.
    fn take(&mut self, n: u32) -> Result<u32, DecodeError> {
        let mut out = 0;
        for i in 0..n {
            let byte = *self.data.get(self.byte).ok_or(DecodeError::Deflate)?;
            out |= u32::from((byte >> self.bit) & 1) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.byte += 1;
            }
        }
        Ok(out)
    }

    /// Discard the rest of the current byte, for stored blocks.
    fn align(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.byte += 1;
        }
    }
}

/// A canonical Huffman table: how many codes of each length, and the symbols in
/// canonical order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    /// Build from a code length per symbol; length 0 means the symbol is unused.
    fn new(lengths: &[u8]) -> Self {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;

        // Where each length's run of symbols begins in canonical order.
        let mut offsets = [0u16; 16];
        for l in 1..15 {
            offsets[l + 1] = offsets[l] + counts[l];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = sym as u16;
                offsets[l as usize] += 1;
            }
        }
        Huffman { counts, symbols }
    }

    /// Walk the code bit by bit, widening until it falls inside a length's run.
    fn decode(&self, bits: &mut Bits) -> Result<u16, DecodeError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - first < count {
                return self
                    .symbols
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or(DecodeError::Deflate);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(DecodeError::Deflate)
    }
}

/// Base lengths and extra bits for the length symbols 257..=285.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Decompress a zlib stream (the two-byte header, then deflate blocks) into at
/// most `limit` bytes, which is what the image header says it should produce.
/// A stream that keeps going past that is malformed, and following it would
/// let a small file ask for an unbounded allocation.
fn inflate(zlib: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    if zlib.len() < 2 {
        return Err(DecodeError::Deflate);
    }
    let mut bits = Bits::new(&zlib[2..]);
    let mut out = Vec::new();

    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.align();
                let lo = bits.take(8)? as usize;
                let hi = bits.take(8)? as usize;
                let len = lo | (hi << 8);
                // Skip NLEN; the CRC already covers the chunk's integrity.
                bits.take(16)?;
                if out.len() + len > limit {
                    return Err(DecodeError::Deflate);
                }
                for _ in 0..len {
                    out.push(bits.take(8)? as u8);
                }
            }
            1 => {
                let (lit, dist) = fixed_tables();
                inflate_block(&mut bits, &lit, &dist, &mut out, limit)?;
            }
            2 => {
                let (lit, dist) = dynamic_tables(&mut bits)?;
                inflate_block(&mut bits, &lit, &dist, &mut out, limit)?;
            }
            _ => return Err(DecodeError::Deflate),
        }
        if last {
            return Ok(out);
        }
    }
}

/// The literal/length and distance tables deflate defines for fixed blocks.
fn fixed_tables() -> (Huffman, Huffman) {
    let mut lengths = [0u8; 288];
    for (i, l) in lengths.iter_mut().enumerate() {
        *l = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    (Huffman::new(&lengths), Huffman::new(&[5u8; 30]))
}

/// Read the code-length alphabet a dynamic block carries, then the two tables
/// it encodes.
fn dynamic_tables(bits: &mut Bits) -> Result<(Huffman, Huffman), DecodeError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = bits.take(5)? as usize + 257;
    let hdist = bits.take(5)? as usize + 1;
    let hclen = bits.take(4)? as usize + 4;

    let mut code_lengths = [0u8; 19];
    for &slot in ORDER.iter().take(hclen) {
        code_lengths[slot] = bits.take(3)? as u8;
    }
    let code_table = Huffman::new(&code_lengths);

    // One run of lengths covering both tables, with three repeat encodings.
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let sym = code_table.decode(bits)?;
        match sym {
            0..=15 => {
                lengths[i] = sym as u8;
                i += 1;
            }
            16 => {
                let prev = *lengths.get(i.wrapping_sub(1)).ok_or(DecodeError::Deflate)?;
                let n = 3 + bits.take(2)? as usize;
                for _ in 0..n {
                    *lengths.get_mut(i).ok_or(DecodeError::Deflate)? = prev;
                    i += 1;
                }
            }
            17 => i += 3 + bits.take(3)? as usize,
            18 => i += 11 + bits.take(7)? as usize,
            _ => return Err(DecodeError::Deflate),
        }
    }
    if i > lengths.len() {
        return Err(DecodeError::Deflate);
    }
    Ok((
        Huffman::new(&lengths[..hlit]),
        Huffman::new(&lengths[hlit..]),
    ))
}

/// Emit one Huffman-coded block: literals straight through, matches copied from
/// what has already been produced.
fn inflate_block(
    bits: &mut Bits,
    lit: &Huffman,
    dist: &Huffman,
    out: &mut Vec<u8>,
    limit: usize,
) -> Result<(), DecodeError> {
    loop {
        if out.len() > limit {
            return Err(DecodeError::Deflate);
        }
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let i = sym as usize - 257;
                let len = LENGTH_BASE[i] as usize + bits.take(u32::from(LENGTH_EXTRA[i]))? as usize;
                let d = dist.decode(bits)? as usize;
                if d >= DIST_BASE.len() {
                    return Err(DecodeError::Deflate);
                }
                let back = DIST_BASE[d] as usize + bits.take(u32::from(DIST_EXTRA[d]))? as usize;
                if back > out.len() {
                    return Err(DecodeError::Deflate);
                }
                // Overlapping copies are legal and common: copy byte by byte so
                // a run repeats as it grows.
                let start = out.len() - back;
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
            _ => return Err(DecodeError::Deflate),
        }
    }
}
