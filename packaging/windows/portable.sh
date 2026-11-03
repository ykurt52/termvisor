#!/usr/bin/env bash
# Builds the portable exe: packs the folder made by bundle.sh into a
# .tar.zst and builds the launcher with it embedded. Run in an MSYS2 UCRT64
# shell.
#
#   portable.sh <bundle-dir> <out.exe>

set -euo pipefail

bundle="${1:?usage: portable.sh <bundle-dir> <out.exe>}"
out="${2:?usage: portable.sh <bundle-dir> <out.exe>}"
here="$(cd "$(dirname "$0")" && pwd)"
archive="$(dirname "$out")/bundle.tar.zst"

mkdir -p "$(dirname "$out")"
tar -C "$bundle" -cf - . | zstd -19 -T0 -q -f -o "$archive"
# The build script runs as a Windows program and needs a Windows path.
TERMVISOR_BUNDLE="$(cygpath -w "$(realpath "$archive")")" \
  cargo build --release --locked --manifest-path "$here/portable/Cargo.toml"
cp "$here/portable/target/release/termvisor-portable.exe" "$out"
rm -f "$archive"
echo "portable: $out, $(du -h "$out" | cut -f1)"
