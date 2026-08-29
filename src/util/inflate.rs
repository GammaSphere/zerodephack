//! DEFLATE decompression (RFC 1951) and the zlib container format (RFC 1950).
//!
//! Replaces the `flate2` crate. Git stores every loose object and every packed
//! object as a zlib stream, so nothing else in this program works without it.
//!
//! The decoder is a straightforward canonical-Huffman implementation: symbols
//! are decoded one bit at a time rather than through a lookup table. That is
//! slower than zlib, and `STDLIB.md` records by how much. Correctness and
//! legibility were the priority.
//!
//! Packed objects are concatenated zlib streams with no separator between them,
//! so every entry point reports how many input bytes it consumed.

use std::error::Error;
use std::fmt;

/// Longest Huffman code permitted by DEFLATE, in bits.
const MAX_BITS: usize = 15;
/// Literal/length alphabet: 256 literals, an end-of-block symbol, 29 lengths.
const LITERAL_CODES: usize = 286;
/// Distance alphabet size.
const DISTANCE_CODES: usize = 30;
/// Code-length alphabet size, used to encode the two dynamic tables.
const CODE_LENGTH_CODES: usize = 19;

/// Code lengths for the code-length alphabet arrive in this order so that
/// trailing zeroes can be omitted (RFC 1951 section 3.2.7).
const CODE_LENGTH_ORDER: [usize; CODE_LENGTH_CODES] =
    [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Base match length for length symbols 257..=285.
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits to read for each length symbol.
const LENGTH_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base match distance for distance symbols 0..=29.
const DISTANCE_BASE: [u16; DISTANCE_CODES] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits to read for each distance symbol.
const DISTANCE_EXTRA: [u32; DISTANCE_CODES] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// What went wrong, and where in the compressed input it was noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InflateError {
    pub kind: ErrorKind,
    /// Byte offset into the compressed input at which the problem surfaced.
    pub offset: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// The stream ended while more bits were required.
    UnexpectedEof,
    /// Block type 3 is reserved and never valid.
    ReservedBlockType,
    /// A stored block's length and its one's complement disagree.
    StoredLengthMismatch { len: u16, nlen: u16 },
    /// More codes of some length than the tree can hold.
    OversubscribedCode,
    /// The literal/length tree does not use every available code.
    IncompleteCode,
    /// A decoded symbol falls outside its alphabet.
    InvalidSymbol(u16),
    /// A match reaches back further than the bytes produced so far.
    DistanceTooFar { distance: usize, available: usize },
    /// The zlib header is not a multiple of 31, so it is corrupt.
    InvalidZlibHeader { cmf: u8, flg: u8 },
    /// Only compression method 8 (deflate) exists.
    UnsupportedCompressionMethod(u8),
    /// Preset dictionaries are legal zlib but git never emits them.
    PresetDictionary,
    /// The trailing Adler-32 does not match the decompressed bytes.
    ChecksumMismatch { expected: u32, actual: u32 },
    /// The caller knew the object size and the stream disagreed.
    LengthMismatch { expected: usize, actual: usize },
}

impl fmt::Display for InflateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at byte {}: ", self.offset)?;
        match &self.kind {
            ErrorKind::UnexpectedEof => write!(f, "compressed stream ended early"),
            ErrorKind::ReservedBlockType => write!(f, "reserved deflate block type 3"),
            ErrorKind::StoredLengthMismatch { len, nlen } => write!(
                f,
                "stored block length {len} does not match its complement {nlen}"
            ),
            ErrorKind::OversubscribedCode => write!(f, "oversubscribed huffman code"),
            ErrorKind::IncompleteCode => write!(f, "incomplete huffman code"),
            ErrorKind::InvalidSymbol(s) => write!(f, "symbol {s} is outside its alphabet"),
            ErrorKind::DistanceTooFar {
                distance,
                available,
            } => write!(
                f,
                "match distance {distance} exceeds the {available} bytes produced so far"
            ),
            ErrorKind::InvalidZlibHeader { cmf, flg } => {
                write!(f, "corrupt zlib header {cmf:#04x} {flg:#04x}")
            }
            ErrorKind::UnsupportedCompressionMethod(m) => {
                write!(f, "unsupported compression method {m}, expected 8")
            }
            ErrorKind::PresetDictionary => write!(f, "zlib preset dictionaries are not supported"),
            ErrorKind::ChecksumMismatch { expected, actual } => write!(
                f,
                "adler-32 mismatch: trailer says {expected:#010x}, data gives {actual:#010x}"
            ),
            ErrorKind::LengthMismatch { expected, actual } => {
                write!(f, "expected {expected} bytes, stream produced {actual}")
            }
        }
    }
}

impl Error for InflateError {}

/// A decompressed stream and the number of input bytes it occupied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inflated {
    pub data: Vec<u8>,
    /// Input bytes consumed, including any zlib header and trailer. Packfiles
    /// concatenate streams without separators, so the caller needs this to find
    /// where the next object begins.
    pub consumed: usize,
}

/// Reads bits least-significant-first, the order DEFLATE uses for every field
/// except the Huffman codes themselves.
struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte to pull into the accumulator.
    pos: usize,
    /// Accumulator holding bits not yet consumed.
    buf: u64,
    /// How many bits in `buf` are valid.
    count: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            buf: 0,
            count: 0,
        }
    }

    /// Offset of the byte the reader is currently working in, for error reporting.
    fn offset(&self) -> usize {
        self.pos.saturating_sub((self.count / 8) as usize)
    }

    fn err(&self, kind: ErrorKind) -> InflateError {
        InflateError {
            kind,
            offset: self.offset(),
        }
    }

    /// Consume `n` bits, where `n` is at most 32.
    fn bits(&mut self, n: u32) -> Result<u32, InflateError> {
        if n == 0 {
            return Ok(0);
        }
        while self.count < n {
            let byte = *self
                .data
                .get(self.pos)
                .ok_or_else(|| self.err(ErrorKind::UnexpectedEof))?;
            self.buf |= (byte as u64) << self.count;
            self.pos += 1;
            self.count += 8;
        }
        let value = (self.buf & ((1u64 << n) - 1)) as u32;
        self.buf >>= n;
        self.count -= n;
        Ok(value)
    }

    /// Discard bits up to the next byte boundary, as stored blocks require.
    fn align(&mut self) {
        let drop = self.count % 8;
        self.buf >>= drop;
        self.count -= drop;
    }

    /// Take whole bytes directly. Only valid immediately after `align`.
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], InflateError> {
        // Whole bytes still sitting in the accumulator belong to the stream,
        // so rewind past them before reading directly.
        let start = self.consumed();
        self.buf = 0;
        self.count = 0;
        let end = start
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or(InflateError {
                kind: ErrorKind::UnexpectedEof,
                offset: start,
            })?;
        self.pos = end;
        Ok(&self.data[start..end])
    }

    /// Bytes consumed so far, ignoring any partial byte in the accumulator.
    fn consumed(&self) -> usize {
        self.pos - (self.count / 8) as usize
    }
}

/// A canonical Huffman decoding table, stored as the count of codes at each bit
/// length plus the symbols in canonical order. Decoding walks one bit at a time,
/// which keeps table construction trivial and free of allocation per symbol.
struct Huffman {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
    /// False when the code leaves unused space. Legal only for distance trees.
    complete: bool,
}

impl Huffman {
    /// Build a table from per-symbol code lengths, where zero means unused.
    fn new(lengths: &[u8]) -> Result<Self, ErrorKind> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &len in lengths {
            counts[len as usize] += 1;
        }
        // Length zero means "symbol absent" and occupies no code space.
        counts[0] = 0;

        // Check that the code neither overflows the tree nor leaves it ragged.
        // `left` tracks unused codes available at the current length.
        let mut left = 1i32;
        for len in 1..=MAX_BITS {
            left <<= 1;
            left -= counts[len] as i32;
            if left < 0 {
                return Err(ErrorKind::OversubscribedCode);
            }
        }

        // Offset of the first symbol of each length within `symbols`.
        let mut offsets = [0u16; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            offsets[len + 1] = offsets[len] + counts[len];
        }
        let total = offsets[MAX_BITS + 1] as usize;

        let mut symbols = vec![0u16; total];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbols[offsets[len as usize] as usize] = symbol as u16;
                offsets[len as usize] += 1;
            }
        }

        Ok(Huffman {
            counts,
            symbols,
            complete: left == 0,
        })
    }

    /// Decode one symbol. Huffman codes are stored most-significant-bit first,
    /// the opposite order from every other DEFLATE field, so bits accumulate
    /// into `code` from the top.
    fn decode(&self, br: &mut BitReader) -> Result<u16, InflateError> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_BITS {
            code |= br.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(br.err(ErrorKind::IncompleteCode))
    }
}

/// The fixed literal/length and distance trees of RFC 1951 section 3.2.6.
fn fixed_tables() -> (Huffman, Huffman) {
    let mut literal_lengths = [0u8; 288];
    literal_lengths[0..144].fill(8);
    literal_lengths[144..256].fill(9);
    literal_lengths[256..280].fill(7);
    literal_lengths[280..288].fill(8);

    // All 32 distance codes are five bits, including the two that never appear.
    let distance_lengths = [5u8; 32];

    (
        Huffman::new(&literal_lengths).expect("fixed literal table is well formed"),
        Huffman::new(&distance_lengths).expect("fixed distance table is well formed"),
    )
}

/// Read the two Huffman trees that precede a dynamic block.
fn dynamic_tables(br: &mut BitReader) -> Result<(Huffman, Huffman), InflateError> {
    let hlit = br.bits(5)? as usize + 257;
    let hdist = br.bits(5)? as usize + 1;
    let hclen = br.bits(4)? as usize + 4;

    if hlit > LITERAL_CODES || hdist > DISTANCE_CODES {
        return Err(br.err(ErrorKind::OversubscribedCode));
    }

    // The tree that encodes the other two trees.
    let mut code_lengths = [0u8; CODE_LENGTH_CODES];
    for &slot in CODE_LENGTH_ORDER.iter().take(hclen) {
        code_lengths[slot] = br.bits(3)? as u8;
    }
    let code_length_tree = Huffman::new(&code_lengths).map_err(|kind| br.err(kind))?;

    // Both trees are encoded as a single run of lengths with three repeat codes.
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0;
    while i < lengths.len() {
        let symbol = code_length_tree.decode(br)?;
        match symbol {
            0..=15 => {
                lengths[i] = symbol as u8;
                i += 1;
            }
            16 => {
                // Repeat the previous length three to six times.
                if i == 0 {
                    return Err(br.err(ErrorKind::InvalidSymbol(16)));
                }
                let previous = lengths[i - 1];
                let repeat = 3 + br.bits(2)? as usize;
                fill_lengths(&mut lengths, &mut i, previous, repeat, br)?;
            }
            17 => {
                let repeat = 3 + br.bits(3)? as usize;
                fill_lengths(&mut lengths, &mut i, 0, repeat, br)?;
            }
            18 => {
                let repeat = 11 + br.bits(7)? as usize;
                fill_lengths(&mut lengths, &mut i, 0, repeat, br)?;
            }
            other => return Err(br.err(ErrorKind::InvalidSymbol(other))),
        }
    }

    let literal_tree = Huffman::new(&lengths[..hlit]).map_err(|kind| br.err(kind))?;
    if !literal_tree.complete {
        return Err(br.err(ErrorKind::IncompleteCode));
    }

    let distance_tree = Huffman::new(&lengths[hlit..]).map_err(|kind| br.err(kind))?;
    // An incomplete distance tree is tolerated only when it holds at most one
    // code, which is what encoders emit for blocks containing no matches.
    if !distance_tree.complete && distance_tree.symbols.len() > 1 {
        return Err(br.err(ErrorKind::IncompleteCode));
    }

    Ok((literal_tree, distance_tree))
}

fn fill_lengths(
    lengths: &mut [u8],
    i: &mut usize,
    value: u8,
    repeat: usize,
    br: &BitReader,
) -> Result<(), InflateError> {
    if *i + repeat > lengths.len() {
        return Err(br.err(ErrorKind::OversubscribedCode));
    }
    lengths[*i..*i + repeat].fill(value);
    *i += repeat;
    Ok(())
}

/// Decode one block's symbols into `out`.
fn inflate_block(
    br: &mut BitReader,
    literals: &Huffman,
    distances: &Huffman,
    out: &mut Vec<u8>,
) -> Result<(), InflateError> {
    loop {
        let symbol = literals.decode(br)?;
        match symbol {
            0..=255 => out.push(symbol as u8),
            256 => return Ok(()),
            257..=285 => {
                let index = symbol as usize - 257;
                let length = LENGTH_BASE[index] as usize + br.bits(LENGTH_EXTRA[index])? as usize;

                let distance_symbol = distances.decode(br)? as usize;
                if distance_symbol >= DISTANCE_CODES {
                    return Err(br.err(ErrorKind::InvalidSymbol(distance_symbol as u16)));
                }
                let distance = DISTANCE_BASE[distance_symbol] as usize
                    + br.bits(DISTANCE_EXTRA[distance_symbol])? as usize;

                if distance > out.len() {
                    return Err(br.err(ErrorKind::DistanceTooFar {
                        distance,
                        available: out.len(),
                    }));
                }

                // Matches may overlap the bytes they produce, so copy one byte
                // at a time rather than cloning a fixed range up front.
                let start = out.len() - distance;
                out.reserve(length);
                for offset in 0..length {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
            other => return Err(br.err(ErrorKind::InvalidSymbol(other))),
        }
    }
}

/// Decompress a raw DEFLATE stream carrying no zlib framing.
///
/// `size_hint` preallocates the output. Git records every object's length in
/// its header, so the exact size is usually known before decoding starts.
pub fn inflate(input: &[u8], size_hint: usize) -> Result<Inflated, InflateError> {
    let mut br = BitReader::new(input);
    let mut out = Vec::with_capacity(size_hint);

    loop {
        let final_block = br.bits(1)? == 1;
        match br.bits(2)? {
            0 => {
                br.align();
                let header = br.bytes(4)?;
                let len = u16::from_le_bytes([header[0], header[1]]);
                let nlen = u16::from_le_bytes([header[2], header[3]]);
                if len != !nlen {
                    return Err(InflateError {
                        kind: ErrorKind::StoredLengthMismatch { len, nlen },
                        offset: br.consumed(),
                    });
                }
                out.extend_from_slice(br.bytes(len as usize)?);
            }
            1 => {
                let (literals, distances) = fixed_tables();
                inflate_block(&mut br, &literals, &distances, &mut out)?;
            }
            2 => {
                let (literals, distances) = dynamic_tables(&mut br)?;
                inflate_block(&mut br, &literals, &distances, &mut out)?;
            }
            _ => return Err(br.err(ErrorKind::ReservedBlockType)),
        }
        if final_block {
            break;
        }
    }

    Ok(Inflated {
        data: out,
        consumed: br.consumed(),
    })
}

/// Adler-32 as specified in RFC 1950, the zlib trailer checksum.
///
/// The inner loop runs 5552 bytes at a time, the largest count that cannot
/// overflow a `u32` before the modulo is applied.
pub fn adler32(data: &[u8]) -> u32 {
    const MODULUS: u32 = 65521;
    let (mut low, mut high) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            low += byte as u32;
            high += low;
        }
        low %= MODULUS;
        high %= MODULUS;
    }
    (high << 16) | low
}

/// Decompress a zlib stream: two header bytes, deflate data, an Adler-32 trailer.
///
/// This is the form git uses for loose objects and for every object inside a
/// packfile. `size_hint` may be zero when the size is not known in advance.
pub fn zlib_decompress(input: &[u8], size_hint: usize) -> Result<Inflated, InflateError> {
    if input.len() < 2 {
        return Err(InflateError {
            kind: ErrorKind::UnexpectedEof,
            offset: 0,
        });
    }

    let (cmf, flg) = (input[0], input[1]);
    let method = cmf & 0x0f;
    if method != 8 {
        return Err(InflateError {
            kind: ErrorKind::UnsupportedCompressionMethod(method),
            offset: 0,
        });
    }
    // The two header bytes read as a big-endian u16 must be a multiple of 31.
    if (cmf as u16 * 256 + flg as u16) % 31 != 0 {
        return Err(InflateError {
            kind: ErrorKind::InvalidZlibHeader { cmf, flg },
            offset: 0,
        });
    }
    if flg & 0x20 != 0 {
        return Err(InflateError {
            kind: ErrorKind::PresetDictionary,
            offset: 1,
        });
    }

    let mut inflated = inflate(&input[2..], size_hint).map_err(|mut e| {
        e.offset += 2;
        e
    })?;

    // The four-byte big-endian trailer follows the deflate data.
    let checksum_start = 2 + inflated.consumed;
    let trailer = input
        .get(checksum_start..checksum_start + 4)
        .ok_or(InflateError {
            kind: ErrorKind::UnexpectedEof,
            offset: checksum_start,
        })?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let actual = adler32(&inflated.data);
    if expected != actual {
        return Err(InflateError {
            kind: ErrorKind::ChecksumMismatch { expected, actual },
            offset: checksum_start,
        });
    }

    inflated.consumed = checksum_start + 4;
    Ok(inflated)
}

/// Decompress a zlib stream whose output length is already known, as it always
/// is for a git object. Rejects streams that disagree with the recorded length.
pub fn zlib_decompress_exact(input: &[u8], expected: usize) -> Result<Inflated, InflateError> {
    let inflated = zlib_decompress(input, expected)?;
    if inflated.data.len() != expected {
        return Err(InflateError {
            kind: ErrorKind::LengthMismatch {
                expected,
                actual: inflated.data.len(),
            },
            offset: 0,
        });
    }
    Ok(inflated)
}
