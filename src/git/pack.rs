//! Reading packfiles: the index, the object records, and delta chains.
//!
//! Replaces `libgit2` for the one job strata needs from it. After a `git gc` a
//! repository has no loose objects at all, so without this the tool reads
//! nothing.
//!
//! Objects are found through the `.idx` companion file, which stores ids sorted
//! with a 256-entry fanout table so a lookup is a bounded binary search rather
//! than a scan.
//!
//! Packed objects come in two flavours: whole ones, and deltas against another
//! object in the same pack (by offset) or in any pack (by id). Deltas nest, so
//! reconstructing one object can mean walking a chain and applying each patch
//! in turn. [`PackReader`] caches resolved bases, since a chain's tail is
//! usually the base for many of its neighbours.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::git::error::{Error, Result};
use crate::git::object::Kind;
use crate::git::oid::Oid;
use crate::util::inflate;

/// `\377tOc`, the magic that distinguishes a version 2 index from version 1.
const IDX_V2_MAGIC: [u8; 4] = [0xff, 0x74, 0x4f, 0x63];
const FANOUT_ENTRIES: usize = 256;
/// Offsets with the top bit set are indices into the 64-bit offset table.
const LARGE_OFFSET_FLAG: u32 = 0x8000_0000;

/// How many reconstructed objects a reader keeps. Delta chains are walked from
/// the tip down, so the objects most worth keeping are the ones just resolved.
const CACHE_ENTRIES: usize = 256;

/// Bytes read from the pack on the first attempt at an object. Grows on demand
/// when a compressed record turns out to be larger than its payload.
const INITIAL_READ: usize = 8 * 1024;

/// The `.idx` file: object ids sorted, with their offsets into the `.pack`.
pub struct PackIndex {
    /// Cumulative counts by leading byte. `fanout[b]` is the number of objects
    /// whose first byte is less than or equal to `b`.
    fanout: [u32; FANOUT_ENTRIES],
    oids: Vec<Oid>,
    offsets: Vec<u64>,
}

impl PackIndex {
    pub fn parse(bytes: &[u8]) -> Result<PackIndex> {
        if bytes.len() < 8 {
            return Err(Error::malformed("pack index", 0, "file is too short"));
        }
        if bytes[..4] != IDX_V2_MAGIC {
            // Version 1 indexes have no magic and a different layout. Git has
            // written version 2 since 2006; say so rather than misparsing.
            return Err(Error::malformed(
                "pack index",
                0,
                "not a version 2 index; version 1 packs are not supported",
            ));
        }
        let version = read_u32(bytes, 4)?;
        if version != 2 {
            return Err(Error::malformed(
                "pack index",
                4,
                format!("unsupported index version {version}"),
            ));
        }

        let mut fanout = [0u32; FANOUT_ENTRIES];
        for (bucket, slot) in fanout.iter_mut().enumerate() {
            *slot = read_u32(bytes, 8 + bucket * 4)?;
        }

        // The last fanout entry is the total object count, and the buckets must
        // rise monotonically or the binary search below is meaningless.
        let count = fanout[FANOUT_ENTRIES - 1] as usize;
        if fanout.windows(2).any(|w| w[0] > w[1]) {
            return Err(Error::malformed(
                "pack index",
                8,
                "fanout table is not monotonic",
            ));
        }

        let oids_start = 8 + FANOUT_ENTRIES * 4;
        let crcs_start = oids_start + count * Oid::LEN;
        let offsets_start = crcs_start + count * 4;
        let large_start = offsets_start + count * 4;

        if bytes.len() < large_start {
            return Err(Error::malformed(
                "pack index",
                bytes.len(),
                format!("index claims {count} objects but the file is too short"),
            ));
        }

        let mut oids = Vec::with_capacity(count);
        for i in 0..count {
            let at = oids_start + i * Oid::LEN;
            oids.push(
                Oid::from_bytes(&bytes[at..at + Oid::LEN])
                    .ok_or_else(|| Error::malformed("pack index", at, "truncated object id"))?,
            );
        }

        let mut offsets = Vec::with_capacity(count);
        for i in 0..count {
            let packed = read_u32(bytes, offsets_start + i * 4)?;
            if packed & LARGE_OFFSET_FLAG == 0 {
                offsets.push(packed as u64);
                continue;
            }
            // Packs over 2 GiB store the real offset in a separate table.
            let slot = (packed & !LARGE_OFFSET_FLAG) as usize;
            let at = large_start + slot * 8;
            let wide = bytes.get(at..at + 8).ok_or_else(|| {
                Error::malformed("pack index", at, "large offset table is truncated")
            })?;
            offsets.push(u64::from_be_bytes(wide.try_into().unwrap()));
        }

        Ok(PackIndex {
            fanout,
            oids,
            offsets,
        })
    }

    pub fn len(&self) -> usize {
        self.oids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.oids.is_empty()
    }

    /// Offset of an object within the pack, if this index holds it.
    ///
    /// The fanout narrows the search to the run of ids sharing a leading byte,
    /// which is why an index is worth having at all.
    pub fn find(&self, oid: Oid) -> Option<u64> {
        let bucket = oid.first_byte() as usize;
        let start = if bucket == 0 {
            0
        } else {
            self.fanout[bucket - 1] as usize
        };
        let end = self.fanout[bucket] as usize;

        let slice = self.oids.get(start..end)?;
        let found = slice.binary_search(&oid).ok()?;
        self.offsets.get(start + found).copied()
    }

    /// Every id in the pack, in index order.
    pub fn oids(&self) -> &[Oid] {
        &self.oids
    }
}

/// A packfile and its index. Cheap to share; the file itself is opened per
/// reader so that threads get independent cursors.
pub struct Pack {
    path: PathBuf,
    index: PackIndex,
}

impl Pack {
    /// Load every pack in `objects/pack`.
    ///
    /// A pack whose index will not parse is skipped with its error returned
    /// alongside, rather than failing the whole run: one bad pack should not
    /// hide the objects in the others.
    pub fn discover(git_dir: &Path) -> (Vec<Pack>, Vec<Error>) {
        let dir = git_dir.join("objects").join("pack");
        let mut packs = Vec::new();
        let mut problems = Vec::new();

        let Ok(entries) = fs::read_dir(&dir) else {
            return (packs, problems);
        };

        for entry in entries.flatten() {
            let idx_path = entry.path();
            if idx_path.extension().and_then(|e| e.to_str()) != Some("idx") {
                continue;
            }
            let pack_path = idx_path.with_extension("pack");
            if !pack_path.is_file() {
                continue;
            }

            match fs::read(&idx_path)
                .map_err(|e| Error::io(&idx_path, e))
                .and_then(|bytes| PackIndex::parse(&bytes))
            {
                Ok(index) => packs.push(Pack {
                    path: pack_path,
                    index,
                }),
                Err(e) => problems.push(e),
            }
        }

        (packs, problems)
    }

    pub fn index(&self) -> &PackIndex {
        &self.index
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open an independent handle onto this pack.
    ///
    /// The standard library has no memory mapping, and `File::try_clone` shares
    /// a cursor, so each reader opens the file itself. That is what makes a
    /// reader per worker thread safe.
    pub fn reader(&self) -> Result<PackReader<'_>> {
        let file = File::open(&self.path).map_err(|e| Error::io(&self.path, e))?;
        Ok(PackReader {
            pack: self,
            file,
            cache: HashMap::new(),
            cache_order: Vec::new(),
        })
    }
}

/// A handle for pulling objects out of one pack.
pub struct PackReader<'a> {
    pack: &'a Pack,
    file: File,
    /// Reconstructed objects keyed by their offset in the pack.
    cache: HashMap<u64, (Kind, Vec<u8>)>,
    /// Insertion order, used to evict the oldest entry once the cache is full.
    cache_order: Vec<u64>,
}

impl PackReader<'_> {
    pub fn contains(&self, oid: Oid) -> bool {
        self.pack.index.find(oid).is_some()
    }

    /// Read an object by id, resolving any delta chain behind it.
    pub fn object(&mut self, oid: Oid) -> Result<Option<(Kind, Vec<u8>)>> {
        match self.pack.index.find(oid) {
            Some(offset) => self.object_at(offset).map(Some),
            None => Ok(None),
        }
    }

    /// Read the object stored at a byte offset, following deltas.
    ///
    /// The chain is walked iteratively rather than recursively: a pathological
    /// pack could nest deltas thousands deep, and that should be a slow read,
    /// not a blown stack.
    pub fn object_at(&mut self, offset: u64) -> Result<(Kind, Vec<u8>)> {
        if let Some(hit) = self.cache.get(&offset) {
            return Ok(hit.clone());
        }

        // Walk down to a non-delta base, stacking the patches to apply.
        let mut patches: Vec<Vec<u8>> = Vec::new();
        let mut current = offset;

        let (kind, mut data) = loop {
            let record = self.read_record(current)?;
            match record.base {
                None => break (record.kind, record.data),
                Some(Base::Offset(base_offset)) => {
                    // A cached base ends the walk early, which is the common
                    // case for a long chain read tip-first.
                    if let Some((kind, data)) = self.cache.get(&base_offset) {
                        patches.push(record.data);
                        break (*kind, data.clone());
                    }
                    patches.push(record.data);
                    current = base_offset;
                }
                Some(Base::Id(base_oid)) => {
                    // A REF_DELTA may point into another pack entirely. Only
                    // same-pack bases can be followed from here; the repository
                    // layer retries the rest across every pack it knows.
                    let base_offset = self
                        .pack
                        .index
                        .find(base_oid)
                        .ok_or(Error::ObjectNotFound { oid: base_oid })?;
                    patches.push(record.data);
                    current = base_offset;
                }
            }
        };

        // Apply the patches from the base outwards.
        while let Some(delta) = patches.pop() {
            data = apply_delta(&data, &delta)?;
        }

        self.remember(offset, kind, &data);
        Ok((kind, data))
    }

    fn remember(&mut self, offset: u64, kind: Kind, data: &[u8]) {
        if self.cache_order.len() >= CACHE_ENTRIES {
            let oldest = self.cache_order.remove(0);
            self.cache.remove(&oldest);
        }
        self.cache.insert(offset, (kind, data.to_vec()));
        self.cache_order.push(offset);
    }

    /// Decode one packed record: its type, its payload, and what it patches.
    fn read_record(&mut self, offset: u64) -> Result<Record> {
        // The compressed payload's length is not stored, so read a window and
        // widen it if the zlib stream turns out to run past the end.
        let mut window = INITIAL_READ;
        loop {
            let buffer = self.read_at(offset, window)?;
            match decode_record(&buffer, offset) {
                Ok(record) => return Ok(record),
                Err(e) if is_truncation(&e) && buffer.len() == window => {
                    // The window filled exactly, so the record may continue.
                    window *= 4;
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|e| Error::io(self.pack.path.clone(), e))?;
        let mut buffer = vec![0u8; len];
        let mut filled = 0;
        while filled < len {
            match self.file.read(&mut buffer[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(Error::io(self.pack.path.clone(), e)),
            }
        }
        buffer.truncate(filled);
        Ok(buffer)
    }
}

/// What a packed record patches, if anything.
enum Base {
    /// A negative offset back to another record in this same pack.
    Offset(u64),
    /// An object id, which may live in a different pack.
    Id(Oid),
}

struct Record {
    kind: Kind,
    data: Vec<u8>,
    base: Option<Base>,
}

fn is_truncation(error: &Error) -> bool {
    matches!(
        error,
        Error::Inflate { source, .. }
            if source.kind == inflate::ErrorKind::UnexpectedEof
    ) || matches!(
        error,
        Error::Malformed {
            what: "pack record",
            ..
        }
    )
}

fn decode_record(buffer: &[u8], offset: u64) -> Result<Record> {
    let mut cursor = 0;
    let first = *buffer
        .first()
        .ok_or_else(|| Error::malformed("pack record", 0, "record header is missing"))?;
    cursor += 1;

    let type_bits = (first >> 4) & 0b111;
    // The size is little-endian, four bits in the first byte then seven more
    // per continuation byte.
    let mut size = (first & 0x0f) as u64;
    let mut shift = 4;
    let mut byte = first;
    while byte & 0x80 != 0 {
        byte = *buffer.get(cursor).ok_or_else(|| {
            Error::malformed(
                "pack record",
                cursor,
                "size runs past the end of the window",
            )
        })?;
        cursor += 1;
        size |= ((byte & 0x7f) as u64) << shift;
        shift += 7;
    }

    let base = match type_bits {
        6 => {
            // OFS_DELTA carries a distance backwards, in its own encoding: each
            // continuation adds one before shifting, so no value has two spellings.
            let mut byte = *buffer.get(cursor).ok_or_else(|| {
                Error::malformed("pack record", cursor, "delta offset is truncated")
            })?;
            cursor += 1;
            let mut distance = (byte & 0x7f) as u64;
            while byte & 0x80 != 0 {
                byte = *buffer.get(cursor).ok_or_else(|| {
                    Error::malformed("pack record", cursor, "delta offset is truncated")
                })?;
                cursor += 1;
                distance = ((distance + 1) << 7) | (byte & 0x7f) as u64;
            }
            let base_offset = offset.checked_sub(distance).ok_or_else(|| {
                Error::malformed(
                    "pack record",
                    cursor,
                    format!("delta at {offset} points {distance} bytes before the pack"),
                )
            })?;
            Some(Base::Offset(base_offset))
        }
        7 => {
            let bytes = buffer.get(cursor..cursor + Oid::LEN).ok_or_else(|| {
                Error::malformed("pack record", cursor, "delta base id is truncated")
            })?;
            cursor += Oid::LEN;
            Some(Base::Id(Oid::from_bytes(bytes).expect("length checked")))
        }
        _ => None,
    };

    let kind = match type_bits {
        1 => Kind::Commit,
        2 => Kind::Tree,
        3 => Kind::Blob,
        4 => Kind::Tag,
        // Type 5 was never assigned; 6 and 7 are deltas whose real type comes
        // from whatever they patch.
        6 | 7 => Kind::Blob,
        other => {
            return Err(Error::malformed(
                "pack record",
                0,
                format!("unknown object type {other}"),
            ));
        }
    };

    let inflated =
        inflate::zlib_decompress(&buffer[cursor..], size as usize).map_err(|source| {
            Error::Inflate {
                path: PathBuf::from(format!("pack offset {offset}")),
                source,
            }
        })?;

    // A delta's payload is the patch, whose length is unrelated to the object
    // it produces, so only whole records can be size-checked here.
    if base.is_none() && inflated.data.len() as u64 != size {
        return Err(Error::malformed(
            "pack record",
            cursor,
            format!(
                "header claims {size} bytes, stream produced {}",
                inflated.data.len()
            ),
        ));
    }

    Ok(Record {
        kind,
        data: inflated.data,
        base,
    })
}

/// Apply a git delta to its base, producing the patched object.
///
/// The format is two sizes followed by a stream of copy and insert opcodes.
/// A copy names a range of the base; an insert carries literal bytes.
pub fn apply_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>> {
    let mut cursor = 0;
    let base_size = read_delta_varint(delta, &mut cursor)?;
    let target_size = read_delta_varint(delta, &mut cursor)?;

    if base.len() as u64 != base_size {
        return Err(Error::malformed(
            "delta",
            0,
            format!("expects a {base_size}-byte base, found {}", base.len()),
        ));
    }

    let mut out = Vec::with_capacity(target_size as usize);

    while cursor < delta.len() {
        let op = delta[cursor];
        cursor += 1;

        if op & 0x80 != 0 {
            // Copy. The low four bits say which offset bytes are present, the
            // next three which size bytes; absent bytes are zero.
            let mut from = 0u64;
            for i in 0..4 {
                if op & (1 << i) != 0 {
                    from |= (take(delta, &mut cursor)? as u64) << (i * 8);
                }
            }
            let mut len = 0u64;
            for i in 0..3 {
                if op & (0x10 << i) != 0 {
                    len |= (take(delta, &mut cursor)? as u64) << (i * 8);
                }
            }
            // A zero length means 65536, which is why the field is worth the
            // special case rather than being treated as a no-op.
            if len == 0 {
                len = 0x10000;
            }

            let end = from
                .checked_add(len)
                .ok_or_else(|| Error::malformed("delta", cursor, "copy range overflows"))?;
            let slice = base.get(from as usize..end as usize).ok_or_else(|| {
                Error::malformed(
                    "delta",
                    cursor,
                    format!(
                        "copy of {len} bytes at {from} runs past the {}-byte base",
                        base.len()
                    ),
                )
            })?;
            out.extend_from_slice(slice);
        } else if op != 0 {
            // Insert: the opcode itself is the number of literal bytes.
            let len = op as usize;
            let slice = delta
                .get(cursor..cursor + len)
                .ok_or_else(|| Error::malformed("delta", cursor, "insert runs past the delta"))?;
            out.extend_from_slice(slice);
            cursor += len;
        } else {
            return Err(Error::malformed(
                "delta",
                cursor - 1,
                "opcode 0 is reserved",
            ));
        }
    }

    if out.len() as u64 != target_size {
        return Err(Error::malformed(
            "delta",
            cursor,
            format!("produced {} bytes, expected {target_size}", out.len()),
        ));
    }

    Ok(out)
}

fn take(data: &[u8], cursor: &mut usize) -> Result<u8> {
    let byte = *data
        .get(*cursor)
        .ok_or_else(|| Error::malformed("delta", *cursor, "instruction is truncated"))?;
    *cursor += 1;
    Ok(byte)
}

/// The little-endian 7-bit varint used for a delta's two size fields. This is
/// not the same encoding as the offset in an OFS_DELTA header.
fn read_delta_varint(data: &[u8], cursor: &mut usize) -> Result<u64> {
    let mut value = 0u64;
    let mut shift = 0;
    loop {
        let byte = take(data, cursor)?;
        value |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 63 {
            return Err(Error::malformed(
                "delta",
                *cursor,
                "size varint is too long",
            ));
        }
    }
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32> {
    let slice = bytes
        .get(at..at + 4)
        .ok_or_else(|| Error::malformed("pack index", at, "truncated"))?;
    Ok(u32::from_be_bytes(slice.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_an_insert_only_delta() {
        // base size 0, target size 5, then a five-byte insert.
        let delta = [0x00, 0x05, 0x05, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(apply_delta(b"", &delta).unwrap(), b"hello");
    }

    #[test]
    fn applies_a_copy_from_the_base() {
        let base = b"hello world";
        // base size 11, target size 5, copy offset 6 length 5.
        let delta = [0x0b, 0x05, 0x80 | 0x01 | 0x10, 6, 5];
        assert_eq!(apply_delta(base, &delta).unwrap(), b"world");
    }

    #[test]
    fn mixes_copies_and_inserts() {
        let base = b"the quick brown fox";
        let mut delta = vec![base.len() as u8, 15];
        // Copy "the quick " (offset 0, length 10).
        delta.extend_from_slice(&[0x80 | 0x01 | 0x10, 0, 10]);
        // Insert "red fox".
        delta.push(5);
        delta.extend_from_slice(b"red f");
        assert_eq!(apply_delta(base, &delta).unwrap(), b"the quick red f");
    }

    #[test]
    fn zero_length_copy_means_65536() {
        let base = vec![b'x'; 70000];
        let mut delta = Vec::new();
        push_varint(&mut delta, base.len() as u64);
        push_varint(&mut delta, 0x10000);
        // 0x81 is a copy whose bit 0 is set, so exactly one offset byte
        // follows. No size bits are set, and a zero size means 65536.
        delta.extend_from_slice(&[0x81, 0]);

        let out = apply_delta(&base, &delta).unwrap();
        assert_eq!(out.len(), 0x10000);
    }

    #[test]
    fn rejects_a_base_of_the_wrong_size() {
        let delta = [0x0b, 0x05, 0x05, b'h', b'e', b'l', b'l', b'o'];
        let err = apply_delta(b"short", &delta).expect_err("size must be checked");
        assert!(err.to_string().contains("11-byte base"), "{err}");
    }

    #[test]
    fn rejects_a_copy_past_the_end_of_the_base() {
        let base = b"hello";
        let delta = [0x05, 0x05, 0x80 | 0x01 | 0x10, 3, 99];
        let err = apply_delta(base, &delta).expect_err("range must be checked");
        assert!(err.to_string().contains("runs past"), "{err}");
    }

    #[test]
    fn rejects_the_reserved_opcode() {
        let delta = [0x00, 0x01, 0x00];
        let err = apply_delta(b"", &delta).expect_err("opcode 0 is reserved");
        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[test]
    fn rejects_a_target_size_that_disagrees() {
        let delta = [0x00, 0x63, 0x05, b'h', b'e', b'l', b'l', b'o'];
        let err = apply_delta(b"", &delta).expect_err("target size must be checked");
        assert!(err.to_string().contains("expected 99"), "{err}");
    }

    fn push_varint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            out.push(byte);
            if value == 0 {
                return;
            }
        }
    }
}
