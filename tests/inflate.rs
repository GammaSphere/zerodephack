//! Conformance tests for the DEFLATE decoder.
//!
//! The corpus in `tests/fixtures/inflate` was produced by Python's standard
//! library `zlib` (see `generate_inflate_fixtures.py`) and is committed, so
//! these tests need nothing but cargo. Every payload is stored once and
//! compressed at four levels, which is what forces stored, fixed-Huffman and
//! dynamic-Huffman blocks to all be exercised.

use std::fs;
use std::path::PathBuf;

use strata::util::inflate::{self, ErrorKind};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/inflate")
}

/// Strip the `_L<level>` suffix a compressed fixture carries to find its source.
fn raw_name_for(stem: &str) -> String {
    match stem.rsplit_once("_L") {
        Some((name, _level)) => name.to_string(),
        None => stem.to_string(),
    }
}

#[test]
fn round_trips_every_fixture() {
    let dir = fixtures();
    let mut checked = 0;

    for entry in fs::read_dir(&dir).expect("fixture directory is present") {
        let path = entry.expect("readable entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("z") {
            continue;
        }

        let stem = path.file_stem().unwrap().to_str().unwrap();
        let expected = fs::read(dir.join(format!("{}.raw", raw_name_for(stem))))
            .unwrap_or_else(|_| panic!("{stem} has no matching .raw payload"));
        let compressed = fs::read(&path).expect("readable stream");

        let inflated = inflate::zlib_decompress(&compressed, expected.len())
            .unwrap_or_else(|e| panic!("{stem} failed to decompress: {e}"));

        assert_eq!(
            inflated.data.len(),
            expected.len(),
            "{stem} produced the wrong length"
        );
        assert!(inflated.data == expected, "{stem} produced the wrong bytes");
        assert_eq!(
            inflated.consumed,
            compressed.len(),
            "{stem} misreported how much input it used"
        );

        checked += 1;
    }

    assert!(checked >= 40, "expected the full corpus, saw {checked}");
}

/// Packfiles concatenate zlib streams with no separator, so a decoder that
/// cannot say where one ended is useless for reading them. This is the property
/// the packfile reader depends on.
#[test]
fn reports_consumed_length_across_concatenated_streams() {
    let dir = fixtures();
    let first = fs::read(dir.join("hello_L6.z")).unwrap();
    let second = fs::read(dir.join("commit_like_L9.z")).unwrap();
    let first_raw = fs::read(dir.join("hello.raw")).unwrap();
    let second_raw = fs::read(dir.join("commit_like.raw")).unwrap();

    let mut joined = first.clone();
    joined.extend_from_slice(&second);
    // Trailing bytes that belong to neither stream must not confuse the reader.
    joined.extend_from_slice(b"trailing garbage");

    let a = inflate::zlib_decompress(&joined, 0).expect("first stream decodes");
    assert_eq!(a.data, first_raw);
    assert_eq!(a.consumed, first.len());

    let b = inflate::zlib_decompress(&joined[a.consumed..], 0).expect("second stream decodes");
    assert_eq!(b.data, second_raw);
    assert_eq!(b.consumed, second.len());
}

#[test]
fn rejects_a_truncated_stream() {
    let compressed = fs::read(fixtures().join("commit_like_L9.z")).unwrap();
    let truncated = &compressed[..compressed.len() / 2];

    let err = inflate::zlib_decompress(truncated, 0).expect_err("truncation must be caught");
    assert_eq!(err.kind, ErrorKind::UnexpectedEof);
    // The offset has to point somewhere real, or it is not worth reporting.
    assert!(err.offset <= truncated.len(), "offset {} is past the input", err.offset);
}

#[test]
fn rejects_a_corrupt_header() {
    let mut compressed = fs::read(fixtures().join("hello_L6.z")).unwrap();
    compressed[1] ^= 0xff;

    let err = inflate::zlib_decompress(&compressed, 0).expect_err("bad header must be caught");
    assert!(
        matches!(
            err.kind,
            ErrorKind::InvalidZlibHeader { .. } | ErrorKind::PresetDictionary
        ),
        "unexpected kind {:?}",
        err.kind
    );
}

#[test]
fn rejects_an_unknown_compression_method() {
    let err = inflate::zlib_decompress(&[0x77, 0x00], 0).expect_err("method 7 does not exist");
    assert_eq!(err.kind, ErrorKind::UnsupportedCompressionMethod(7));
}

#[test]
fn rejects_a_flipped_payload_byte() {
    // Corrupting the compressed body should be caught either by the Huffman
    // decoder or, failing that, by the Adler-32 trailer. Silence is the one
    // unacceptable outcome.
    let original = fs::read(fixtures().join("commit_like_L9.z")).unwrap();
    let raw = fs::read(fixtures().join("commit_like.raw")).unwrap();

    let mut caught = 0;
    for index in (10..original.len() - 8).step_by(37) {
        let mut damaged = original.clone();
        damaged[index] ^= 0b0001_0000;

        match inflate::zlib_decompress(&damaged, 0) {
            Err(_) => caught += 1,
            // A flipped bit can occasionally still decode to the same bytes.
            Ok(out) => assert_ne!(
                out.data, raw,
                "corruption at {index} silently produced different-but-accepted output"
            ),
        }
    }
    assert!(caught > 0, "no corruption was detected at all");
}

#[test]
fn rejects_a_length_that_disagrees_with_the_header() {
    let compressed = fs::read(fixtures().join("hello_L6.z")).unwrap();
    let err = inflate::zlib_decompress_exact(&compressed, 999)
        .expect_err("a wrong expected length must be caught");
    assert_eq!(
        err.kind,
        ErrorKind::LengthMismatch {
            expected: 999,
            actual: 11
        }
    );
}

#[test]
fn empty_input_does_not_panic() {
    assert!(inflate::zlib_decompress(&[], 0).is_err());
    assert!(inflate::zlib_decompress(&[0x78], 0).is_err());
    assert!(inflate::inflate(&[], 0).is_err());
}

#[test]
fn adler32_matches_the_rfc_examples() {
    assert_eq!(inflate::adler32(b""), 1);
    assert_eq!(inflate::adler32(b"a"), 0x0062_0062);
    assert_eq!(inflate::adler32(b"Wikipedia"), 0x11E6_0398);
}
