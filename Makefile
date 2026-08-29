.PHONY: build test deps-proof repro clean

build:
	cargo build --release

test:
	cargo test

deps-proof:
	@cargo tree > deps-proof.txt
	@cargo metadata --format-version 1 --no-deps > /dev/null
	@echo "" >> deps-proof.txt
	@cargo --version >> deps-proof.txt
	@rustc --version >> deps-proof.txt

clean:
	cargo clean
