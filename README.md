# strata

Git repository archaeology with an empty dependency manifest.

`strata` reads a `.git` directory directly — loose objects and packfiles, including delta
chains — and reports where a codebase's risk is concentrated: hotspots, code ownership and
bus factor, and change coupling.

No third-party crates. No network. It never invokes `git`.

**Status:** under construction for the Zero Dependency Hackathon (28–31 August 2026).

## Build

```sh
cargo build --release
```

Requires Rust 1.98.0. That is the only step.
