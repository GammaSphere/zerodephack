# strata — design

**Date:** 2026-08-29
**Event:** Zero Dependency Hackathon (28–31 August 2026), Track A — Developer Tools & CLI
**Language:** Rust 1.98.0, `std` only. `Cargo.toml` `[dependencies]` is empty.

## Problem

Teams want to know where a codebase's risk is concentrated: which files churn hardest,
which have exactly one author who has since gone quiet, and which files keep changing
together despite living in unrelated directories. The analyses are well established —
Adam Tornhill's code-maat popularised them — but every existing implementation either
shells out to `git` (git-of-theseus), links a git library (hercules via go-git), or is a
commercial SaaS you upload your repository to (CodeScene, Calyntro, repowise).

None of them is a self-contained offline binary that reads `.git` itself.

## Solution

`strata` reads the git object database directly — loose objects and packfiles, including
delta chains — with no third-party code, no network access, and without invoking `git`.

The hackathon FAQ blesses this shape explicitly: *"Parsing files those tools already
produced is fine, because nothing third-party ends up in your artifact."* Two conditions
apply and both are met: the reliance on git-produced files is disclosed in STDLIB.md, and
the tool degrades gracefully when `.git` is absent rather than being useless without it.

### Commands

```
strata hotspots              churn x size — what to refactor first
strata owners --bus-factor   files with a single author, and whether they have gone quiet
strata coupling              files that change together but live apart
strata age                   stable vs. thrashing
```

Flags: `--json`, `--csv`, `--html`, `--since <date>`, `--path <glob>`, `--top <n>`,
`--no-color`, `--verify`.

## Architecture

Four layers, dependencies pointing strictly downward. Each unit is independently testable.

```
main.rs · cli.rs          subcommand parser, --help, exit codes
   |
analysis/  walk · churn · ownership · coupling · age
   |
git/       repo · objdb · loose · pack · object · refs · mailmap · sha1 · oid
   |
util/      inflate · date · glob · pool · width · error
render/    ansi · table · json · csv · html      (leaf, consumes analysis output)
```

The `git/` layer exposes a single facade, `Repository::object(oid) -> Object`, so
`analysis/` never learns whether an object arrived from a loose file or from a delta chain
six levels deep inside a packfile. That boundary keeps the hard part swappable and the
easy part testable.

## Key decisions

**Churn is measured in revisions, not lines.** Line-level churn requires inflating every
blob at every version and diffing it. Revision count — commits touching a file — is
code-maat's primary metric and needs only tree objects. Blobs are inflated once, for HEAD
only, to obtain current size. Line churn sits behind `--lines` as a stretch goal. This
choice removes most of the project's performance risk.

**No memory mapping.** Rust's standard library has none. Each worker thread holds its own
`File` handle and seeks; the `.idx` (26 bytes per object) is read into memory once and
shared read-only. Cross-platform, `std`-only, and it displaces `memmap2`.

**Concurrency: sequential walk, parallel diff.** Commit-graph traversal is inherently
ordered, so it stays single-threaded — commits are small and cheap. Tree-diffing each
commit against its parent is embarrassingly parallel, chunked across a hand-rolled thread
pool built on `std::thread` and `mpsc`. The delta-chain cache is per-thread and therefore
duplicated; the README says so.

**SHA-1 is optional and clearly labelled.** It sits behind `--verify`, off the critical
path. Rust's standard library offers no hashing at all, so there is no primitive to
compose; we implement published FIPS 180-4. The README states plainly that this is content
addressing exactly as git uses it, not a security boundary.

### Stated limits

Declared in the README rather than left for a judge to discover:

- SHA-1 repositories only. SHA-256 repositories are detected and rejected with a clear message.
- Shallow clones are detected and warned about — history before the graft point is absent.
- Commits touching more than 50 files are excluded from coupling analysis. A mass rename
  should not couple an entire tree; this threshold is standard practice.

## Standard-library substitutions

| Module | Replaces | Note |
|---|---|---|
| `util/inflate.rs` | `flate2` | RFC 1951 fixed + dynamic Huffman + stored blocks, RFC 1950 wrapper |
| `git/pack.rs` | `git2`/`libgit2`, `gix` | idx v2, OFS_DELTA + REF_DELTA chains, bounded base cache |
| `render/json.rs` | `serde_json` | correct string escaping, `\u` for control characters |
| `util/date.rs` | `chrono`/`time` | days-from-civil, git's `1724928000 +0200` format |
| `render/html.rs` | `plotters` | inline SVG, no CDN, single self-contained file |
| `git/objdb.rs` | `memmap2` | per-thread `File` + seek |
| `util/glob.rs` | `globset` | pathspec matching |
| `util/pool.rs` | `rayon` | `std::thread` + `mpsc` |
| `render/table.rs` | `comfy-table`, `unicode-width` | column alignment; UAX#11 subset, limits disclosed |
| `render/ansi.rs` | `colored` | `NO_COLOR`, TTY detection |
| `cli.rs` | `clap` | subcommands, `--help`, exit codes 0/1/2 |
| `git/sha1.rs` | `sha1` | FIPS 180-4, optional |
| `render/csv.rs` | `csv` | RFC 4180 quoting |

Thirteen substitutions, four of them deep. The Package Killer claim targets `flate2`; the
stronger framing is that `libgit2` is a native C library displaced by `std`-only Rust.

## Testing

Fixture `.git` directories are committed as binary under `tests/fixtures/`. **Tests never
invoke `git`** — the fixtures' provenance is disclosed in STDLIB.md.

Coverage: delta chains, merges, renames, unicode paths, empty commits, detached HEAD,
empty repository, shallow clone, SHA-256 repository (must fail cleanly), truncated pack
(must error *with a byte offset*), missing `.git` (must degrade gracefully).

Inflate carries its own corpus, round-tripped against fixtures generated by Python's
standard-library `zlib` at authoring time and committed as bytes.

## Build order

Inflate comes first because both loose objects and packfiles need it. The loose-object
path lands next, giving a working end-to-end slice against real repositories before
packfiles — the highest-risk component — are attempted. A repository holding only loose
objects still produces every analysis correctly, so the fallback is a working tool.

| Block | Work | Gate |
|---|---|---|
| Aug 29 AM | inflate, loose objects, refs, commit walk, tree diff | revision counts on a real repository |
| Aug 29 PM | packfile idx, pack, delta chains | fixture repository with a real pack resolves |
| Aug 30 | hotspots, ownership/bus factor, coupling, age; ANSI table; `--json` | all four subcommands work |
| Aug 30 PM | tests, edge cases, fixtures | suite green |
| Aug 31 AM | README, STDLIB.md, deps-proof, reproducible build, `.zero-dep.toml`, `--html` | deliverables complete |
| Aug 31 | demo video, final verification | submitted before 18:00 UTC |

## Deliverables

Public repository, `cargo build --release` as the single build command, empty dependency
manifest, `deps-proof.txt`, README.md, STDLIB.md, `.zero-dep.toml`, MIT license, and a
five-minute demo video.

Bonus claims: STDLIB Log (+3, thirteen substitutions), Package Killer (+3, `flate2`), and
Reproducible Build (+5) if and only if two `cargo build --release` runs produce a
byte-identical binary. The claim is dropped rather than fudged if they do not.
