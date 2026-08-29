# STDLIB.md

Every third-party crate `strata` would normally have used, and what replaced it.

`Cargo.toml` has an empty `[dependencies]` table. `Cargo.lock` contains one
package: `strata` itself. Nothing below is vendored — see
[Disclosures](#disclosures) at the end for the complete accounting of code and
data that did not originate in this repository.

Rust is the hardest language to do this in. There is no JSON in `std`, no
compression, no hashing, no calendar, no regular expressions, no random numbers,
no async runtime, and no terminal handling beyond reading and writing bytes.
Almost everything below had to be written rather than composed.

---

## 1. `flate2` → `src/util/inflate.rs`

**What it does:** DEFLATE decompression (RFC 1951) and the zlib container
format (RFC 1950), including the Adler-32 trailer.

**Why it was unavoidable:** Git stores every loose object and every packed
object as a zlib stream. Without this, `strata` reads nothing at all — it is
the floor the entire program stands on. `flate2` is one of the most-installed
crates on crates.io, and its default backend is a C library.

**How:** Canonical Huffman decoding over the three block types (stored,
fixed, dynamic), a bit reader that accumulates least-significant-bit-first, and
LZ77 back-references copied one byte at a time so overlapping matches — where
the copy source advances into bytes the same match is still producing — come out
right.

**The design decision that mattered:** every entry point reports how many input
bytes it consumed. Packfiles concatenate zlib streams with no separator and no
length prefix, so a decoder that cannot say where one stream ended is useless
for reading them. There is a test pinning exactly that property.

**Honest performance:** measured against C zlib on the same 44-stream corpus,
20 rounds each:

| | Throughput |
|---|---|
| `strata` (`cargo test --release --test inflate -- --ignored --nocapture`) | 545 MiB/s |
| C zlib (via Python's `zlib.decompress`) | 1510 MiB/s |

**About 2.8× slower.** The decoder walks Huffman codes one bit at a time
instead of using a lookup table, which is the standard optimisation and was
skipped for legibility. The comparison is rough — the Python side carries
per-call interpreter overhead — but the order of magnitude is right, and the
direction is not flattering. In practice it does not matter: `strata` runs in
under 200 ms on a 35-commit repository, and decompression is not the bottleneck.

---

## 2. `git2` / `libgit2` / `gix` → `src/git/`

**What it does:** reads the git object database — loose objects, packfiles,
pack indexes, delta chains, refs, `packed-refs`, and `HEAD`.

**Why:** this is the project. `git2` binds `libgit2`, a **native C library**,
so displacing it with `std`-only Rust removes a build-time C dependency as well
as a crate.

**How:**

- `pack.rs` — `.idx` version 2 parsing, using the 256-entry fanout table to
  narrow a lookup to the run of ids sharing a leading byte. Packs over 2 GiB
  store their real offsets in a separate 64-bit table; both forms are handled.
- Delta chains (`OFS_DELTA` and `REF_DELTA`) are resolved **iteratively, not
  recursively**. A pathological pack could nest deltas thousands deep, and that
  should be a slow read rather than a blown stack.
- `object.rs` — commit, tree, blob and tag parsing. Commit headers can continue
  across lines with a leading space, which every gpg-signed commit does; a
  parser that misses this never reaches the `committer` line after a signature
  block.
- `refs.rs` — loose refs override packed ones, symbolic refs follow to a bounded
  depth so a hand-edited cycle cannot hang, and a ref naming a path outside the
  repository is ignored rather than followed.

**Verification:** across two fixture repositories totalling 629 objects, 73 of
them deltas with chains up to depth 40, every reconstructed object hashes to the
id the pack index filed it under, and every object's type and size matches
`git cat-file --batch-all-objects`. Across seven repositories the per-commit
change set is byte-identical to `git log --no-merges --name-only`.

**Not supported, and stated rather than discovered:** SHA-256 repositories
(detected and refused by name), pack index version 1 (superseded in 2006),
`objects/info/alternates`, and cross-pack `REF_DELTA` bases. That last one only
occurs in thin packs, which exist during transfer and are resolved by
`index-pack` before anything lands on disk.

---

## 3. `sha1` → `src/git/sha1.rs`

**What it does:** SHA-1, from FIPS 180-4, streaming.

**Why:** `std` ships no cryptographic hashes at all, so there was no primitive
to compose — this is implemented from the specification rather than assembled
from parts.

**What it is for, precisely:** content addressing, exactly as git uses it. It
is *not* a security boundary, and the module says so at the top. SHA-1 has been
unsafe against collision attacks since SHAttered in 2017; an attacker who can
write to your `.git` directory has already won by easier routes.

**Why it earned its place:** it makes object reconstruction self-checking. If a
delta chain were applied wrongly by a single byte, the hash would not match the
id the index filed it under. `strata --verify` runs exactly this check, and it
caught two real bugs during development — a streaming path that dropped buffered
bytes when an input did not complete a 64-byte block, and a malformed copy
opcode in a delta test.

---

## 4. `serde_json` → `src/render/json.rs`

**What it does:** writes JSON. The most-installed crate on crates.io, displaced
by about 240 lines.

**Honest scoping:** only the *writing* half. `strata` reads no JSON, and
writing is by far the easier and more forgiving direction. Claiming to have
replaced `serde_json` outright would be overstating it.

**What actually needed care:** escaping. Rust strings are already valid UTF-8
and JSON accepts UTF-8 directly, so multi-byte text passes through untouched —
what must be escaped is the quote, the backslash, and every control character
below `U+0020`. A commit summary containing a tab produces output no parser will
accept otherwise. Non-finite floats become `null`, because JSON has no `NaN`.
There are tests for the trailing-comma bug that every hand-rolled serialiser has
exactly once.

---

## 5. `chrono` / `time` → `src/util/date.rs`

**What it does:** epoch seconds to civil dates, and back.

**Why:** `std` can tell you a `SystemTime` is some number of seconds after 1970
and nothing more. There is no calendar in it.

**How:** Howard Hinnant's `civil_from_days`, which handles the proleptic
Gregorian calendar without tables or loops by shifting the year to start in
March, so the leap day lands at the end and month lengths become regular enough
to compute.

**The subtle part:** Rust's `/` and `%` truncate toward zero, but a calendar
needs floor division, so the conversion uses `div_euclid` and `rem_euclid`.
Without that, every timestamp before 1970 lands on the wrong day — and
repositories contain imported history from the 1980s and the occasional clock
set wrong. There is a test that walks 400 consecutive days checking the date
advances by exactly one each step, which catches the off-by-ones that spot
checks miss.

**Not implemented:** any timezone database. Git records each commit's UTC
offset alongside its timestamp, which is all the calendar this tool needs.

---

## 6. `clap` → `src/cli.rs`

**What it does:** subcommands, long and short flags, `--flag value` and
`--flag=value`, `--` to stop option parsing, a help screen, and exit codes.

**Why it is a fair swap:** `clap` is excellent and is usually the largest thing
in a small Rust binary's dependency tree. For a five-command tool, roughly 200
lines covers it.

**What was kept because it matters:** `--` handling, so a directory whose name
begins with a dash is still reachable. Distinct exit codes — 0 success, 1 the
repository could not be read, 2 the command line was wrong — so a script can
tell "your repo has a problem" from "you typed it wrong".

**Not implemented:** shell completions, `--help` for individual subcommands,
suggestion-on-typo, or coloured help.

---

## 7. `globset` / `glob` → `src/util/glob.rs`

**What it does:** `?`, `*` within a path segment, and `**` across segments.

**One borrowed behaviour:** a pattern with no `/` matches at any depth, so
`--path '*.rs'` finds `src/git/pack.rs` rather than only top-level files. That
is git's pathspec rule and what people mean when they type it.

**Honest limitation:** the matcher backtracks, so an adversarial pattern like
`a*a*a*a*b` is exponential — the same trap a naive regex engine has. Path
filters are typed by hand against short paths, so the simple recursion is the
right trade, but it is a trade rather than an oversight. No character classes,
no brace expansion.

---

## 8. `colored` / `owo-colors` → `src/render/ansi.rs`

**What it does:** ANSI SGR colour, and — more importantly — deciding whether to
emit it.

**Why the decision is the real work:** the escape sequences are a handful of
bytes. Getting the *policy* right is what stops
`strata hotspots > report.txt` from filling the file with escape codes. The
order is `--no-color`, then `NO_COLOR` (per no-color.org), then
`CLICOLOR_FORCE`, then whether stdout is a terminal.

**Already in `std`:** `std::io::IsTerminal`, stable since Rust 1.70, which
retired the `is-terminal` and `atty` crates for everyone.

---

## 9. `comfy-table` / `tabled` → `src/render/table.rs`

**What it does:** column-aligned output with a flexible column that absorbs
overflow.

**The detail that makes it work:** the path column truncates from the *left*,
because the informative end of a path is the right-hand one. `…/git/pack.rs`
is useful; `src/git/pa…` is not.

**Honest limitation:** `std` cannot ask the terminal how wide it is — there is
no `ioctl` in it — so `COLUMNS` is the only available hint, and the fallback is
100 columns.

---

## 10. `unicode-width` → `src/util/width.rs`

**What it does:** display width in terminal cells.

**Why it is needed:** `str::len` counts bytes and `chars().count()` counts code
points. A terminal cares about neither. A CJK ideograph occupies two columns and
a combining accent occupies none, so a table aligned on either of the other two
measures comes out visibly crooked the moment a non-Latin name appears.

**Honest scoping — this is a subset of UAX#11, not an implementation of it.**
The real standard is a large generated table that changes with each Unicode
release. What is here covers CJK, Hangul, fullwidth forms, the common emoji
blocks and the combining marks; everything else is assumed to be one column,
which is correct for Latin, Greek, Cyrillic, Hebrew and Arabic. Emoji sequences
joined by `U+200D` — a family, a flag, a profession — are measured as the sum of
their parts and therefore over-count, because getting those right needs grapheme
cluster breaking and a bigger table still.

---

## 11. `strip-ansi-escapes` → `strip_ansi` in `src/render/table.rs`

About fifteen lines. Needed because a cell may already carry colour, and
measuring its width without removing the escapes first would push the column out
of true — the exact failure `unicode-width` was brought in to prevent.

---

## 12. `csv` → `src/render/csv.rs`

**What it does:** RFC 4180 writing.

**Why the rule is worth stating:** a field needs quoting when it contains a
comma, a quote, or a line break, and a quote inside a quoted field is written
twice. Get it wrong and a commit summary containing a comma silently shifts
every column after it — corruption that stays invisible until someone opens the
file in a spreadsheet and the numbers are in the wrong place.

---

## 13. `rust-ini` / `configparser` → `src/git/config.rs`

**What it does:** enough of git's config format to answer two questions —
`extensions.objectformat` and `core.bare`.

**Why it is honest to call it "enough":** git config is INI with subsections
(`[remote "origin"]`), `#` and `;` comments, keys that fold to lowercase while
subsection names keep their case, and bare keys meaning boolean true. All of
that is handled. Include directives, the full quoting escape set, and type
coercion are not, and the module says so rather than pretending to be a general
parser.

---

## 14. `memmap2` → per-thread `File` + `seek` in `src/git/pack.rs`

**What it does:** random access into a packfile.

**Why there was no choice:** `std` has no memory mapping at all, and
`File::try_clone` returns a handle that *shares a cursor* with the original, so
it cannot give threads independent positions. Each reader opens the file itself.

**The consequence, which is a real cost:** the compressed length of a packed
record is not stored anywhere, so records are read through a window that widens
when the zlib stream runs past its end. That keeps memory bounded on large packs
at the price of occasionally reading a region twice. A memory map would avoid
both. The upside is that this design is already thread-safe.

---

## 15. `thiserror` / `anyhow` → `src/git/error.rs`

Hand-written `Display` and `std::error::Error` implementations. Written to be
read by a person at a terminal, so they name the file and the byte offset
wherever one is known: `src/git/pack.rs: corrupt compressed data at byte 909:
compressed stream ended early`.

---

## 16. `adler32` → `adler32` in `src/util/inflate.rs`

Eight lines. The inner loop runs 5552 bytes at a time, the largest count that
cannot overflow a `u32` before the modulo is applied.

---

## 17. `rayon` → `std::thread` + `std::sync::mpsc`

**Status: not currently used.** `strata` is single-threaded. Tree diffing is
embarrassingly parallel and the object layer was built for it — `Repository` is
immutable and shareable, and readers hold no shared state precisely so that a
reader per worker thread would be safe.

It is listed here because the design accommodates it and the temptation to claim
it was real. The tool runs in under 200 ms on the repositories tested, so
threading would have been complexity bought with nothing.

---

## Summary

| Crate | Replaced by | Lines |
|---|---|---|
| `flate2` | `util/inflate.rs` | 572 |
| `git2` / `libgit2` | `git/pack.rs` + `git/object.rs` + `git/repo.rs` | 1394 |
| `sha1` | `git/sha1.rs` | 255 |
| `serde_json` | `render/json.rs` | 241 |
| `chrono` / `time` | `util/date.rs` | 330 |
| `clap` | `cli.rs` | 378 |
| `globset` | `util/glob.rs` | 162 |
| `colored` | `render/ansi.rs` | 134 |
| `comfy-table` | `render/table.rs` | 255 |
| `unicode-width` | `util/width.rs` | 202 |
| `strip-ansi-escapes` | `strip_ansi` | ~15 |
| `csv` | `render/csv.rs` | 98 |
| `rust-ini` | `git/config.rs` | 158 |
| `memmap2` | per-thread `File` + `seek` | — |
| `thiserror` | `git/error.rs` | 95 |
| `adler32` | `inflate::adler32` | ~8 |
| `is-terminal` / `atty` | `std::io::IsTerminal` (already in std) | 0 |

**Sixteen crates displaced, about 6,450 lines of Rust, and an empty manifest.**

---

## Disclosures

Everything in this section is code or data that did not originate in this
repository this weekend. Nothing here ships inside the binary.

**Algorithms implemented from published specifications.** RFC 1951 (DEFLATE),
RFC 1950 (zlib), RFC 4180 (CSV), FIPS 180-4 (SHA-1), the git packfile and index
formats, and UAX#11 (character width, partially). These are specifications
implemented from their descriptions, not code that was copied.

**One algorithm taken from a named source:** the `civil_from_days` and
`days_from_civil` conversions in `src/util/date.rs` follow Howard Hinnant's
public-domain `chrono-Compatible Low-Level Date Algorithms`. The arithmetic is
his; the Rust, the Euclidean-division correction and the tests are mine. It is
credited in the module.

**Development-time tools that are not runtime dependencies:**

- **Python 3.14** ran `tests/generate_inflate_fixtures.py` once, to produce the
  zlib corpus. The output is committed, so `cargo test` needs nothing but cargo.
  The script is committed too, so the corpus can be regenerated and inspected.
- **git** created the fixture repositories under `tests/fixtures/`, and was used
  by hand to cross-check results. **`strata` never invokes git at runtime**, and
  the test suite never invokes it either — the fixtures are committed as bytes.

**Reading files that git produced** is the tool's entire premise, and the
hackathon FAQ addresses it directly: *"Parsing files those tools already
produced is fine, because nothing third-party ends up in your artifact."* The
two conditions it attaches are met — the reliance is disclosed here, and
`strata` degrades gracefully when `.git` is absent, reporting
`no git repository found at or above <path>` and exiting 1.

**No vendored source.** No third-party code has been copied into `src/`.

**AI assistance:** this project was built with Claude Code, which the hackathon
rules explicitly expect. The design decisions, the honest limitations recorded
throughout, and the bugs found and fixed are documented in the commit history,
which is deliberately granular for that reason.
