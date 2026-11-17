#!/usr/bin/env bash
# Build build/indyboot, the ECOFF file the PROM loads.
#
# Usage: ./build.sh
# Set INDYBOOT_URL to compile in a default manifest URL.
set -euo pipefail
cd "$(dirname "$0")"
TARGET=mips2-sgi-arcs-o32
cargo build --release -p indyboot --target "targets/$TARGET.json" \
  -Z build-std=core -Z build-std-features=compiler-builtins-mem
cargo build --release -p mkecoff
mkdir -p build
target/release/mkecoff "target/$TARGET/release/indyboot" build/indyboot
