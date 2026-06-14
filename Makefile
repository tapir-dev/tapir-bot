.PHONY: check dev build test clean

check:
	@cargo check

dev:
	@cargo build

build:
	@cargo build --release

test:
	@cargo test

clean:
	@cargo clean
