//! Git object identifiers.
//!
//! Only SHA-1 object IDs are supported. Repositories created with
//! `--object-format=sha256` are detected during discovery and rejected with a
//! clear message rather than being silently misread.

use std::fmt;

/// A 20-byte SHA-1 object identifier.
///
/// Copy rather than a heap allocation: history walks compare and hash millions
/// of these, and twenty bytes is cheaper to move than a pointer plus a length.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid([u8; Oid::LEN]);

impl Oid {
    pub const LEN: usize = 20;
    pub const HEX_LEN: usize = 40;

    /// The all-zero identifier, which git uses to mean "no such object".
    pub const ZERO: Oid = Oid([0; Oid::LEN]);

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        Some(Oid(bytes.try_into().ok()?))
    }

    /// Parse forty hex characters. Accepts either case, as git writes lowercase
    /// but tolerates uppercase in files a user may have edited by hand.
    pub fn parse_hex(text: &[u8]) -> Option<Self> {
        if text.len() != Self::HEX_LEN {
            return None;
        }
        // as_chunks yields fixed-size arrays, so the pair destructures and
        // no bounds check survives into the loop body.
        let (pairs, _) = text.as_chunks::<2>();
        let mut out = [0u8; Self::LEN];
        for (byte, &[high, low]) in out.iter_mut().zip(pairs) {
            *byte = (hex_value(high)? << 4) | hex_value(low)?;
        }
        Some(Oid(out))
    }

    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }

    /// Leading byte, which indexes the fanout table at the head of a pack index.
    pub fn first_byte(&self) -> u8 {
        self.0[0]
    }

    pub fn is_zero(&self) -> bool {
        self.0 == [0; Self::LEN]
    }

    /// The abbreviated form git shows by default.
    pub fn short(&self) -> String {
        self.to_string()[..7].to_string()
    }
}

fn hex_value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({self})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"4b825dc642cb6eb9a060e54bf8d69288fbee4904";

    #[test]
    fn round_trips_hex() {
        let oid = Oid::parse_hex(SAMPLE).expect("valid hex");
        assert_eq!(oid.to_string().as_bytes(), SAMPLE);
        assert_eq!(oid.first_byte(), 0x4b);
        assert_eq!(oid.short(), "4b825dc");
    }

    #[test]
    fn accepts_uppercase() {
        let upper = SAMPLE.to_ascii_uppercase();
        assert_eq!(Oid::parse_hex(&upper), Oid::parse_hex(SAMPLE));
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(Oid::parse_hex(b"").is_none());
        assert!(Oid::parse_hex(b"4b825dc").is_none(), "too short");
        assert!(
            Oid::parse_hex(b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz").is_none(),
            "non-hex characters"
        );
        // Forty characters of the right shape but one bad digit.
        let mut bad = SAMPLE.to_vec();
        bad[39] = b'g';
        assert!(Oid::parse_hex(&bad).is_none());
    }

    #[test]
    fn zero_is_recognised() {
        assert!(Oid::ZERO.is_zero());
        assert!(Oid::parse_hex(&[b'0'; 40]).unwrap().is_zero());
        assert!(!Oid::parse_hex(SAMPLE).unwrap().is_zero());
    }
}
