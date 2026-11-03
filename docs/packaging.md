# Packaging and CI

| Target | How | Files |
|---|---|---|
| Debian, Ubuntu 24.04+ | `.deb` built with cargo-deb | `[package.metadata.deb]` in `Cargo.toml` |
| Arch Linux (AUR) | PKGBUILD from the release tarball | `packaging/arch/` |
| Desktop menu, software centers | Desktop file, AppStream metainfo, icon | `data/` |
| Checks | GitHub Actions on every push and PR | `.github/workflows/ci.yml` |
| Releases | GitHub Actions on a `v*` tag | `.github/workflows/release.yml` |
| APT repository | Signed repo on GitHub Pages, rebuilt on each release | `packaging/apt/`, `.github/workflows/apt.yml` |

Rust 1.92 or newer is needed (the gtk-rs 0.11 crates require it).

## .deb

```bash
cargo build --release --locked
cargo install cargo-deb --locked
cargo deb --no-build
```

Output: `target/debian/termvisor_<version>-1_<arch>.deb`. Library
dependencies are found by `dpkg-shlibdeps`; it also depends on
`openssh-client`, recommends `iputils-ping` and `iputils-tracepath`, and
suggests `mosh` and `adb`. Install with:

```bash
sudo apt install ./target/debian/termvisor_0.1.0-1_amd64.deb
```

It installs `/usr/bin/termvisor`, the desktop file, the metainfo, the icon
and the docs (`/usr/share/doc/termvisor/`).

### Ubuntu PPA or the official archives

Launchpad and the archives build offline with the distribution's Rust.
Ubuntu 24.04 offers Rust up to 1.91 (`rustc-1.91`), one short of what
termvisor needs, so a PPA build for 24.04 is not possible yet. Ways
forward:

- wait for `rustc-1.92` (or newer) in noble-updates, then package with
  vendored crates;
- or build on a newer Ubuntu release whose Rust is 1.92 or newer.

Until then the `.deb` from the releases page (or from CI) installs on
24.04: at run time it only needs the system libraries.

## APT repository

Users add the repository once, then install and upgrade with apt:

```bash
curl -fsSL https://ykurt52.github.io/termvisor/key.gpg | sudo gpg --dearmor -o /usr/share/keyrings/termvisor.gpg
echo "deb [signed-by=/usr/share/keyrings/termvisor.gpg] https://ykurt52.github.io/termvisor stable main" | sudo tee /etc/apt/sources.list.d/termvisor.list
sudo apt update && sudo apt install termvisor
```

`apt.yml` runs after every release (and by hand from the Actions tab).
It downloads the `.deb` of every GitHub release, so older versions stay
in the repository, then runs `packaging/apt/build-repo.sh` and deploys
the result to GitHub Pages.

`build-repo.sh <debs-dir> <out-dir>` makes the tree:

| Path | Content |
|---|---|
| `pool/main/*.deb` | the packages |
| `dists/stable/main/binary-<arch>/Packages{,.gz}` | package index |
| `dists/stable/Release`, `InRelease`, `Release.gpg` | checksums, signed |
| `key.gpg` | public signing key (armored) |
| `index.html` | install instructions |

It signs with the key in `$APT_KEY_ID` (default: the first secret key)
and the passphrase in `$APT_KEY_PASS`, if any. To try it locally with a
throwaway key:

```bash
export GNUPGHOME=$(mktemp -d)
gpg --batch --passphrase '' --quick-gen-key "test <test@example.invalid>" ed25519 sign never
packaging/apt/build-repo.sh target/debian /tmp/apt-site
```

### One-time setup

1. Make a signing key used only for this repository:
   `gpg --quick-gen-key "termvisor apt <yunuskkurt@gmail.com>" ed25519 sign never`
2. Add repository secrets (Settings > Secrets and variables > Actions):
   `APT_SIGNING_KEY` with the output of
   `gpg --armor --export-secret-keys <fingerprint>`, and
   `APT_SIGNING_PASS` with its passphrase (leave it out for a key
   without one).
3. Settings > Pages > Source: GitHub Actions.
4. Keep a backup of the key offline. A lost key means every user has to
   fetch a new `key.gpg`.

## AUR

```bash
cd packaging/arch
updpkgsums          # after the v0.1.0 tag exists: real checksum instead of SKIP
makepkg --printsrcinfo > .SRCINFO
makepkg -si         # build and install locally to try it
```

Publishing: clone `ssh://aur@aur.archlinux.org/termvisor.git`, copy
`PKGBUILD` and `.SRCINFO` into it, commit and push. `check()` runs the
test suite.

## CI

`ci.yml` has two jobs:

| Job | Does |
|---|---|
| Ubuntu 24.04, Rust 1.92 | clippy with warnings as errors, tests under a virtual display (so the GTK test runs), desktop and metainfo validation, release build, `.deb` as an artifact |
| Arch Linux | build and tests in an `archlinux:latest` container with the distribution's Rust |

## Releasing

1. Set `version` in `Cargo.toml`, add a `<release>` to
   `data/io.github.ykurt52.termvisor.metainfo.xml`, set `pkgver` in the
   PKGBUILD.
2. Commit, then tag and push: `git tag v0.1.0 && git push origin v0.1.0`.
3. `release.yml` checks that the tag matches `Cargo.toml`, builds the
   `.deb` and creates the GitHub release with it, then `apt.yml`
   publishes the APT repository.
4. Update the AUR package (above).
