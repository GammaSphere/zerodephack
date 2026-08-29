//! SHA-1, as specified in FIPS 180-4.
//!
//! Replaces the `sha1` crate. Rust's standard library ships no cryptographic
//! hashes at all, so there is no primitive here to compose - the algorithm is
//! implemented from the specification.
//!
//! **This is content addressing, not a security boundary.** Git names objects
//! by the SHA-1 of their contents, and strata computes the same value for one
//! purpose: to confirm that an object it reconstructed is the object git
//! stored. SHA-1 has been unsafe against collision attacks since SHAttered in
//! 2017, and nothing here should be read as a claim otherwise. An attacker who
//! can write to your `.git` directory has already won by easier routes.

use crate::git::object::Kind;
use crate::git::oid::Oid;

/// Streaming SHA-1 state.
pub struct Sha1 {
    /// The five chaining variables, H0..H4.
    state: [u32; 5],
    /// Bytes not yet absorbed into a full 64-byte block.
    buffer: [u8; 64],
    buffered: usize,
    /// Total message length in bytes, which the padding encodes in bits.
    length: u64,
}

impl Default for Sha1 {
    fn default() -> Self {
        Sha1::new()
    }
}

impl Sha1 {
    pub fn new() -> Sha1 {
        Sha1 {
            state: [
                0x6745_2301,
                0xefcd_ab89,
                0x98ba_dcfe,
                0x1032_5476,
                0xc3d2_e1f0,
            ],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.length += data.len() as u64;

        // Top up a partial block first.
        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];

            // Still short of a full block: everything is buffered, and falling
            // through would overwrite it with an empty remainder.
            if self.buffered < 64 {
                return;
            }

            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }

        // Then consume whole blocks straight from the input. as_chunks hands
        // back &[u8; 64] directly, so compress needs no fallible conversion.
        let (blocks, rest) = data.as_chunks::<64>();
        for block in blocks {
            self.compress(block);
        }
        self.buffer[..rest.len()].copy_from_slice(rest);
        self.buffered = rest.len();
    }

    /// Pad the message and return the digest.
    pub fn finish(mut self) -> [u8; 20] {
        // A single 1 bit, zeroes, then the length in bits as a big-endian u64.
        let bit_length = self.length * 8;
        self.update_raw(&[0x80]);
        while self.buffered != 56 {
            self.update_raw(&[0x00]);
        }
        self.update_raw(&bit_length.to_be_bytes());

        let mut digest = [0u8; 20];
        let (words, _) = digest.as_chunks_mut::<4>();
        for (slot, word) in words.iter_mut().zip(self.state) {
            *slot = word.to_be_bytes();
        }
        digest
    }

    /// Absorb padding bytes without counting them toward the message length.
    fn update_raw(&mut self, data: &[u8]) {
        for &byte in data {
            self.buffer[self.buffered] = byte;
            self.buffered += 1;
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        // The message schedule: sixteen words from the block, then sixty more
        // derived by xor and a one-bit rotation.
        let mut w = [0u32; 80];
        let (words, _) = block.as_chunks::<4>();
        for (slot, word) in w.iter_mut().zip(words) {
            *slot = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = self.state;

        for (i, &word) in w.iter().enumerate() {
            // Four twenty-round stages, each with its own mixing function and
            // constant. Wrapping arithmetic is the specification, not an
            // oversight: SHA-1 is defined modulo 2^32.
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };

            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);

            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
    }
}

pub fn digest(data: &[u8]) -> [u8; 20] {
    let mut hasher = Sha1::new();
    hasher.update(data);
    hasher.finish()
}

/// The object id git would give this content: the SHA-1 of the same
/// `<type> <size>\0<payload>` framing that a loose object stores.
///
/// This is what makes reconstruction self-checking. If a delta chain were
/// applied wrongly by even one byte, the id would not match the one the pack
/// index filed the object under.
pub fn object_id(kind: Kind, payload: &[u8]) -> Oid {
    let mut hasher = Sha1::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update(b" ");
    hasher.update(payload.len().to_string().as_bytes());
    hasher.update(&[0]);
    hasher.update(payload);
    Oid::from_bytes(&hasher.finish()).expect("digest is twenty bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: [u8; 20]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn matches_the_fips_vectors() {
        assert_eq!(hex(digest(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(digest(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn handles_the_padding_boundaries() {
        // 55, 56 and 64 bytes straddle the point where the length no longer
        // fits in the final block and a second one is needed.
        assert_eq!(
            hex(digest(&[b'a'; 55])),
            "c1c8bbdc22796e28c0e15163d20899b65621d65a"
        );
        assert_eq!(
            hex(digest(&[b'a'; 56])),
            "c2db330f6083854c99d4b5bfb6e8f29f201be699"
        );
        assert_eq!(
            hex(digest(&[b'a'; 64])),
            "0098ba824b5c16427bd7a1122a5a442a25ec644d"
        );
        assert_eq!(
            hex(digest(&vec![b'a'; 1_000_000])),
            "34aa973cd4c4daa4f61eeb2bdbad27316534016f"
        );
    }

    #[test]
    fn streaming_matches_one_shot() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let one_shot = digest(&data);

        // Feed it in awkward slices to exercise the partial-block path.
        let mut hasher = Sha1::new();
        let mut offset = 0;
        for step in [1usize, 7, 63, 64, 65, 200, 1000] {
            let end = (offset + step).min(data.len());
            hasher.update(&data[offset..end]);
            offset = end;
        }
        hasher.update(&data[offset..]);
        assert_eq!(hasher.finish(), one_shot);
    }

    #[test]
    fn computes_the_git_object_id() {
        // git hash-object -t blob gives this for the bytes "what is up, doc?".
        assert_eq!(
            object_id(Kind::Blob, b"what is up, doc?").to_string(),
            "bd9dbf5aae1a3862dd1526723246b20206e5fc37"
        );
        // The empty tree, a constant every git user has seen.
        assert_eq!(
            object_id(Kind::Tree, b"").to_string(),
            "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
        );
    }
}
