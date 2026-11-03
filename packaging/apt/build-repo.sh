#!/usr/bin/env bash
# Builds a signed apt repository from a folder of .deb files.
#
#   build-repo.sh <debs-dir> <out-dir>
#
# Signs with the gpg key in $APT_KEY_ID (default: the only secret key in
# the keyring). The result is a static tree ready for any web server.

set -euo pipefail

debs="${1:?usage: build-repo.sh <debs-dir> <out-dir>}"
out="${2:?usage: build-repo.sh <debs-dir> <out-dir>}"
here="$(cd "$(dirname "$0")" && pwd)"
suite=stable
key="${APT_KEY_ID:-$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')}"
test -n "$key" || { echo "no signing key"; exit 1; }

shopt -s nullglob
files=("$debs"/*.deb)
test ${#files[@]} -gt 0 || { echo "no .deb files in $debs"; exit 1; }

mkdir -p "$out/pool/main"
cp "${files[@]}" "$out/pool/main/"
cd "$out"

archs=$(for f in pool/main/*.deb; do dpkg-deb -f "$f" Architecture; done | sort -u | tr '\n' ' ')
for a in $archs; do
  dir="dists/$suite/main/binary-$a"
  mkdir -p "$dir"
  apt-ftparchive --arch "$a" packages pool > "$dir/Packages"
  gzip -9nkf "$dir/Packages"
done

apt-ftparchive \
  -o APT::FTPArchive::Release::Origin=termvisor \
  -o APT::FTPArchive::Release::Label=termvisor \
  -o APT::FTPArchive::Release::Suite=$suite \
  -o APT::FTPArchive::Release::Codename=$suite \
  -o APT::FTPArchive::Release::Components=main \
  -o "APT::FTPArchive::Release::Architectures=${archs% }" \
  release "dists/$suite" > "dists/$suite/Release"

gpg --batch --yes --pinentry-mode loopback ${APT_KEY_PASS:+--passphrase "$APT_KEY_PASS"} \
  -u "$key" --clearsign -o "dists/$suite/InRelease" "dists/$suite/Release"
gpg --batch --yes --pinentry-mode loopback ${APT_KEY_PASS:+--passphrase "$APT_KEY_PASS"} \
  -u "$key" -abs -o "dists/$suite/Release.gpg" "dists/$suite/Release"
gpg --armor --export "$key" > key.gpg

cp "$here/index.html" index.html
echo "repository in $out: $(ls pool/main | wc -l) packages, ${archs% }"
