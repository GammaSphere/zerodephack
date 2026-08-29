#!/usr/bin/env python3
"""Generate the zlib corpus that tests/inflate.rs checks against.

Run once at authoring time; the output is committed, so the Rust test suite
never needs Python. This script is a development aid and is disclosed in
STDLIB.md - it is not part of the shipped artifact.

    python3 tests/generate_inflate_fixtures.py

NAME.raw holds the original bytes once, and NAME_L<level>.z holds the zlib
stream at each compression level. Level 0 produces stored blocks, level 1
favours fixed Huffman and level 9 favours dynamic Huffman, so all three block
types get covered.
"""

import os
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "fixtures", "inflate")


def cases():
    """Yield (name, payload) pairs covering the decoder's branches."""
    yield "empty", b""
    yield "single_byte", b"x"
    yield "hello", b"hello world"

    # Long runs exercise overlapping matches, where the copy source advances
    # into bytes the same match is still producing.
    yield "run_of_one", b"a" * 70000
    yield "overlap_short", b"ab" * 40000
    yield "overlap_three", b"xyz" * 30000

    # Every byte value, so no literal is left untested.
    yield "all_bytes", bytes(range(256)) * 200

    # Incompressible data forces stored blocks even at high levels, and pushes
    # past the 65535-byte limit of a single stored block.
    yield "incompressible", os.urandom(200000)

    # Realistic text, the shape git objects actually take.
    text = (
        b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\n"
        b"parent 0000000000000000000000000000000000000000\n"
        b"author Someone <someone@example.com> 1724928000 +0200\n"
        b"committer Someone <someone@example.com> 1724928000 +0200\n"
        b"\nA commit message that repeats itself. " * 500
    )
    yield "commit_like", text

    # A single block boundary case: exactly 65535 bytes, the stored block limit.
    yield "stored_limit", b"q" * 65535

    # Deep match distances, up to the 32768-byte window.
    prefix = os.urandom(32768)
    yield "far_distance", prefix + b"MARKER" + prefix[:1000] + b"MARKER"


def main():
    os.makedirs(OUT, exist_ok=True)
    for stale in os.listdir(OUT):
        os.remove(os.path.join(OUT, stale))

    count = 0
    for name, payload in cases():
        with open(os.path.join(OUT, name + ".raw"), "wb") as f:
            f.write(payload)
        for level in (0, 1, 6, 9):
            with open(os.path.join(OUT, f"{name}_L{level}.z"), "wb") as f:
                f.write(zlib.compress(payload, level))
            count += 1

    print(f"wrote {count} streams to {OUT}")


if __name__ == "__main__":
    main()
