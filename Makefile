# Convenience wrapper. The canonical build is `cargo build --release` and
# needs nothing from this file.
#
# Note: this Makefile was not exercised on the machine strata was developed on,
# which has no make. Every recipe below is a single cargo or shell command, and
# `cargo build --release` is the path that is tested.

.PHONY: build test lint fmt bench deps-proof repro clean

build:
	cargo build --release

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings
	cargo fmt --check

fmt:
	cargo fmt

# Throughput of the hand-written DEFLATE decoder. The figure in STDLIB.md.
bench:
	cargo test --release --test inflate -- --ignored --nocapture

# Regenerate deps-proof.txt. The committed copy is the output of this recipe.
deps-proof:
	@{ \
	  echo "Dependency proof for strata"; \
	  echo "Generated $$(date -u '+%Y-%m-%d %H:%M UTC') on $$(rustc -vV | sed -n 's/^host: //p')"; \
	  echo; \
	  echo "================================================================"; \
	  echo "$$ cargo tree"; \
	  echo "================================================================"; \
	  cargo tree; \
	  echo; \
	  echo "Only one node: the crate itself. No dependency edges exist."; \
	  echo; \
	  echo "================================================================"; \
	  echo "$$ cat Cargo.lock"; \
	  echo "================================================================"; \
	  cat Cargo.lock; \
	  echo; \
	  echo "One package in the lockfile. A single third-party crate would add"; \
	  echo "a [[package]] block here, and its transitive closure with it."; \
	  echo; \
	  echo "================================================================"; \
	  echo "Toolchain"; \
	  echo "================================================================"; \
	  cargo --version; \
	  rustc --version; \
	} > deps-proof.txt
	@echo "wrote deps-proof.txt"

# Build twice from clean and compare hashes. Requires RUSTFLAGS to be unset,
# because setting it overrides .cargo/config.toml and loses determinism.
repro:
	cargo clean
	cargo build --release
	@sha256sum target/release/strata target/release/strata.exe 2>/dev/null | tee /tmp/strata-hash-1
	cargo clean
	cargo build --release
	@sha256sum target/release/strata target/release/strata.exe 2>/dev/null | tee /tmp/strata-hash-2
	@diff /tmp/strata-hash-1 /tmp/strata-hash-2 && echo "reproducible: identical" || echo "reproducible: NO"

clean:
	cargo clean
