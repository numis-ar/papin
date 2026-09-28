# Rust workspace: build, verify, package.
# Honors CARGO_TARGET_DIR from the environment (e.g. a tmpfs target dir);
# no hardcoded paths. fmt/clippy must be on PATH (a rustup toolchain or a
# standalone toolchain bin dir).

CARGO_TARGET_DIR ?=
ifneq ($(CARGO_TARGET_DIR),)
export CARGO_TARGET_DIR
endif

.PHONY: all build test deb clean

all: build

build:
	cargo build --release

test:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets
	cargo test --workspace

deb:
	dpkg-buildpackage -us -uc -b

clean:
	cargo clean
	rm -rf debian/papin-cli debian/papin-gateway debian/.debhelper \
		debian/files debian/*.substvars debian/*.debhelper.log \
		debian/debhelper-build-stamp
