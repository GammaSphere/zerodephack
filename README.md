# strata

**Git repository archaeology with an empty dependency manifest.**

`strata` reads a `.git` directory directly — loose objects, packfiles, delta
chains, refs — and tells you where a codebase's risk is concentrated: which
files churn hardest, which have exactly one author who has since gone quiet, and
which files keep changing together despite living in unrelated directories.

No third-party crates. No network. **It never invokes `git`.**

Zero Dependency Hackathon 2026 · **Track A — Developer Tools & CLI** · Rust 1.98, `std` only.

---

## Build and run

```sh
cargo build --release
```

That is the whole build. Then:

```sh
./target/release/strata hotspots /path/to/repo
```

Requires Rust 1.98.0. No other tooling, no network access, no configuration.

## What it tells you

```
$ strata
raptors-v2 · main

  35 commits by 1 author over 1 day
  170 files, 86856 lines, first commit 2026-08-01

  100% of files have exactly one author (170)
  0 files whose main author has been quiet for 180 days or more

Next: strata hotspots · strata owners · strata coupling
```

### `strata hotspots` — what to look at first

Files that change often **and** are large. Either signal alone misleads: a
changelog has hundreds of revisions and no complexity, a vendored library is
enormous and never touched.

```
$ strata hotspots --top 5
file             revs  authors  lines  score  last change
src/block.rs       13        1   3720   0.76   2026-08-02
src/html.rs         6        1   1801   0.32   2026-08-02
src/lib.rs         11        1     56   0.32   2026-08-02
src/markdown.rs     6        1   1129   0.30   2026-08-02
README.md           7        1    249   0.28   2026-08-03

score = revisions x size, each relative to this repository's own maximum
```

### `strata owners` — who knows this, and are they still here

Bus factor per file, plus whether the person holding it has stopped committing.
A file with one author who left six months ago is a different problem from a
file with one author sitting next to you.

```
$ strata owners --top 3
file                    revs  authors  main author  share  bus  owner last seen
server/src/routes/tg.ts    5        1  Doniyor       100%    1          48 days
miniapp/src/lib/api.ts     4        1  Doniyor       100%    1          48 days
server/src/lib/env.ts      4        1  Doniyor       100%    1          48 days
```

### `strata coupling` — the dependencies nobody drew

Files that keep changing together. The interesting rows cross a directory
boundary, because those are the couplings the layout does not show you.

```
$ strata coupling --top 4
file            changes with  together  degree
Cargo.toml      Cargo.lock           3     75%
ffi/src/lib.rs  Makefile             3     60%  crosses dirs
Makefile        Cargo.toml           3     50%
src/html.rs     src/lib.rs           3     21%

degree = commits touching both, over commits touching either
```

### `strata age` — stable, or abandoned?

Stable code is the goal, not a problem. This report exists to tell the two
apart.

## Options

```
-n, --top <N>            Rows to show [default: 20]
    --since <WHEN>       Only commits since a date (2024-08-29) or an
                         offset (30d, 6w, 3m, 1y)
    --path <GLOB>        Only paths matching a glob (*.rs, src/**/mod.rs)
    --min-co <N>         Coupling: least co-changes to report [default: 3]
    --dormant-days <N>   Owners: silence before an owner counts as gone [default: 180]
    --include-merges     Count merge commits, which are skipped by default
    --json / --csv       Machine-readable output
    --no-color           Never colour output (NO_COLOR is also honoured)
    --verify             Check every object's SHA-1 while reading
```

Exit codes: `0` success, `1` the repository could not be read, `2` the command
line was wrong.

## How it is verified

Correctness claims are worth only as much as what backs them, so:

- **Every packed object round-trips.** Across two fixture repositories totalling
  **629 objects — 73 of them deltas, with chains up to depth 40** — every
  reconstructed object hashes to the SHA-1 the pack index filed it under. A
  delta applied wrongly by one byte cannot pass this. Run it yourself with
  `strata --verify`.
- **Object types and sizes match git exactly**, checked against
  `git cat-file --batch-all-objects --batch-check`.
- **Change sets match git exactly.** Across seven repositories, the
  `(commit, path)` set is byte-identical to `git log --no-merges --name-only`.
- **137 tests**, including a DEFLATE corpus of 44 streams covering overlapping
  matches, the 32 KiB window edge, the 65535-byte stored-block limit,
  incompressible input, and every byte value at four compression levels.
- **17 end-to-end tests** against a committed 19-commit repository fixture with
  a rename, a deletion, a merge, an empty commit, a binary blob and a non-ASCII
  filename. Every expected number in them came from git, and the command that
  produced it is named in the comment beside it.

```sh
cargo test
```

## Honest limits

Things `strata` does not do, stated here rather than left for you to discover:

- **Only exact renames are detected.** A file moved *and* edited in the same
  commit reads as a delete plus an add. Git scores similarity to catch those;
  `strata` matches identical blob content only.
- **SHA-1 repositories only.** A SHA-256 repository is detected and refused by
  name rather than misread.
- **Merge commits are skipped by default**, matching `git log`'s treatment of
  file history. `--include-merges` turns that off.
- **Commits touching more than 50 files are excluded from coupling** — a mass
  rename is not evidence that its files belong together. They still count toward
  churn, ownership and age.
- **Shallow clones produce lower bounds**, and `strata` says so on stderr when
  it sees one.
- **Metrics are proxies.** Revision count stands in for churn, line count for
  complexity, co-change for coupling. They point at places worth a human's
  attention, not at defects.
- **Terminal width comes from `COLUMNS`**, because `std` has no `ioctl`.
- **Single-threaded.** Fast enough not to need otherwise (under 200 ms on the
  repositories tested), and the object layer is built so that threading would
  be safe to add.
- **Decompression is about 2.8× slower than C zlib.** Measured, not estimated —
  see [STDLIB.md](STDLIB.md).
- **Not supported:** pack index version 1, `objects/info/alternates`, and
  cross-pack `REF_DELTA` bases (which only occur in thin packs, resolved before
  they reach disk).

## Zero dependencies

```sh
cargo tree          # one node: strata itself
cat Cargo.lock      # one [[package]] block
```

Full transcript in [deps-proof.txt](deps-proof.txt). Every crate that was
displaced, and how, is documented in **[STDLIB.md](STDLIB.md)** — sixteen of
them, including `flate2`, `serde_json`, `clap`, `chrono` and `libgit2`, which is
a native C library.

## Reproducible build

Two clean `cargo build --release` runs produce a byte-identical binary:

```
5de3a07b925a2c780eb0bda7e7810720d88bcaed9a4a947dbd420adaf28e209c
5de3a07b925a2c780eb0bda7e7810720d88bcaed9a4a947dbd420adaf28e209c
```

MSVC embeds a link timestamp and a debug GUID by default, so
`.cargo/config.toml` passes `/Brepro` to make both content-derived. Verify with:

```sh
cargo clean && cargo build --release && sha256sum target/release/strata*
cargo clean && cargo build --release && sha256sum target/release/strata*
```

One caveat worth knowing: setting the `RUSTFLAGS` environment variable — even
to an empty string — **overrides** `.cargo/config.toml`, and reproducibility is
lost. Leave it unset.

## Why this is legitimate under the rules

`strata` reads files that git wrote. The hackathon FAQ addresses this directly:
*"Parsing files those tools already produced is fine, because nothing
third-party ends up in your artifact."* The two conditions it attaches are both
met — the reliance is disclosed in [STDLIB.md](STDLIB.md#disclosures), and
`strata` degrades gracefully without `.git`, reporting
`no git repository found at or above <path>` and exiting 1.

## Layout

```
src/
  git/        object database: packs, deltas, refs, objects, SHA-1
  analysis/   history walk, tree diffing, the four reports
  render/     tables, colour, JSON, CSV
  util/       inflate, dates, globs, display width
tests/        DEFLATE conformance corpus and fixtures
```

## Licence

MIT. See [LICENSE](LICENSE).
