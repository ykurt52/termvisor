#!/usr/bin/env bash
# Puts termvisor.exe and the GTK runtime it needs into one folder that runs
# on any Windows 10/11 x64. Run in an MSYS2 UCRT64 shell after
# `cargo build --release`.
#
#   bundle.sh <termvisor.exe> <out-dir>
#
# GTK looks for share/ and lib/ next to the exe, so everything sits in the
# folder root: the exe, the DLLs, share/ (icons, schemas) and lib/ (image
# loaders).

set -euo pipefail

exe="${1:?usage: bundle.sh <termvisor.exe> <out-dir>}"
out="${2:?usage: bundle.sh <termvisor.exe> <out-dir>}"
prefix="${MINGW_PREFIX:-/ucrt64}"

rm -rf "$out"
mkdir -p "$out/share/glib-2.0/schemas" "$out/share/icons" "$out/lib"
cp "$exe" "$out/"

# DLLs from the MSYS2 prefix that a binary links, copied once.
deps() {
  ldd "$1" | awk -v p="$prefix/" 'index($3, p) == 1 { print $3 }' | while read -r dll; do
    [ -e "$out/$(basename "$dll")" ] || cp "$dll" "$out/"
  done
}
deps "$out/termvisor.exe"

# Image loaders (SVG icons need the librsvg one) and their DLLs.
cp -r "$prefix/lib/gdk-pixbuf-2.0" "$out/lib/"
for loader in "$out"/lib/gdk-pixbuf-2.0/*/loaders/*.dll; do
  deps "$loader"
done

cp "$prefix/share/glib-2.0/schemas/gschemas.compiled" "$out/share/glib-2.0/schemas/"
cp -r "$prefix/share/icons/Adwaita" "$prefix/share/icons/hicolor" "$out/share/icons/"

cp LICENSE "$out/LICENSE.txt"
cp README.md "$out/README.md"
echo "bundle in $out: $(find "$out" -maxdepth 1 -name '*.dll' | wc -l) DLLs, $(du -sh "$out" | cut -f1)"
