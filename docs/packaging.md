# Packaging and CI

| Target | How | Files |
|---|---|---|
| Debian, Ubuntu 24.04+ | `.deb` built with cargo-deb | `[package.metadata.deb]` in `Cargo.toml` |
| Arch Linux (AUR) | PKGBUILD from the release tarball | `packaging/arch/` |
| Windows 10/11 x64 | installer (Inno Setup) and portable single exe, both with the GTK runtime, built with MSYS2 | `packaging/windows/` |
| Desktop menu, software centers | Desktop file, AppStream metainfo, icon | `data/` |
| Checks | GitHub Actions, started by hand | `.github/workflows/ci.yml` |
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
sudo apt install ./target/debian/termvisor_1.0.1-1_amd64.deb
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

## Windows

Built on a GitHub Windows runner with MSYS2 (UCRT64), which has GTK4,
libadwaita and Rust as packages:

```bash
pacman -S zstd mingw-w64-ucrt-x86_64-{gtk4,libadwaita,librsvg,adwaita-icon-theme,pkgconf,rust,binutils}
cargo build --release --locked
packaging/windows/bundle.sh target/release/termvisor.exe dist/termvisor
```

`bundle.sh <exe> <out-dir>` copies into one folder:

| Path | Content |
|---|---|
| `termvisor.exe`, `*.dll` | the program and every MSYS2 DLL it links (found with `ldd`) |
| `lib/gdk-pixbuf-2.0/` | image loaders, SVG included, and their DLLs |
| `share/icons/` | Adwaita and hicolor icon themes |
| `share/glib-2.0/schemas/` | compiled GSettings schemas |

GTK finds `share/` and `lib/` next to the exe. The installer and the
portable exe both ship this folder.

`build.rs` puts the icon and the version (Properties → Details) into
`termvisor.exe` through `packaging/windows/resource.rs`, with windres. When
windres is missing the exe builds without them; CI sets
`TERMVISOR_RC_REQUIRED=1` so that fails there instead.

### Portable exe

`portable.sh <bundle-dir> <out.exe>` packs the folder into a `.tar.zst`
and builds the launcher in `packaging/windows/portable/` with it embedded:

```bash
packaging/windows/portable.sh dist/termvisor dist/termvisor-windows-x64_v1.0.3_portable.exe
```

The launcher is one exe of about 25 MB. On start it unpacks the bundle
into `%LOCALAPPDATA%\termvisor\portable\<version>-<build>\` unless that
folder is already there, starts `termvisor.exe` from it with the same
arguments and exits at once (a launcher that stayed to clean up kept the
new window from being resized until it ended). The first start takes a
second or two, later starts are instant. termvisor removes the folders of
other builds in the background 10 s after start, only when it runs from
that portable folder, and skips any folder still in use. Settings and
sessions are in the same place as with the installer.

The launcher takes its version from termvisor's `Cargo.toml`. Its tests
run on Linux: `cargo test --manifest-path packaging/windows/portable/Cargo.toml`.

### Installer

`setup.ps1 <version> <bundle-dir> <out-dir>` runs Inno Setup on
`termvisor.iss` and writes `termvisor-windows-x64_v<version>_setup.exe`:

```powershell
packaging/windows/setup.ps1 1.0.1 dist/termvisor dist
```

| Behaviour | How |
|---|---|
| Install for the user, no admin rights | default; the first page offers "all users" (needs admin) |
| Start menu entry, optional desktop icon | `[Icons]`, `[Tasks]` |
| Uninstall from Settings → Apps | Inno's uninstaller |
| Same or older version installed | installs over it; old DLLs, `lib/` and `share/` are removed first |
| Newer version installed | stops with a message |

The installed copy is found by the fixed `AppGuid` in `termvisor.iss`;
never change it. The Windows runner image has Inno Setup.

`termvisor.ico` is made from the app SVG:
`packaging/windows/mkico.py data/icons/hicolor/scalable/apps/io.github.ykurt52.termvisor.svg packaging/windows/termvisor.ico`
(needs Python with GdkPixbuf). Run it again when the SVG changes.

### File names

| File | Content |
|---|---|
| `termvisor-windows-x64_v<version>_setup.exe` | the installer |
| `termvisor-windows-x64_v<version>_portable.exe` | the portable exe |

CI uploads each as its own artifact on every push
(`termvisor-windows-x64_v<version>_setup` and
`termvisor-windows-x64_v<version>_portable`); a `v*` tag attaches both
files to the release.

The Windows code paths are type-checked on Linux too, see
[windows.md](windows.md).

## AUR

```bash
cd packaging/arch
updpkgsums          # after the release tag exists: checksum of the new tarball
makepkg --printsrcinfo > .SRCINFO
makepkg -si         # build and install locally to try it
```

Publishing: clone `ssh://aur@aur.archlinux.org/termvisor.git`, copy
`PKGBUILD` and `.SRCINFO` into it, commit and push. `check()` runs the
test suite.

## CI

`ci.yml` runs only when started by hand, so pushes cost no runner time:
Actions → CI → Run workflow and pick the branch, or

```bash
gh workflow run ci.yml --ref v1.0.7
gh run watch    # follow it
```

Run it before tagging a release, and when a build for Windows is needed
to test. It has three jobs:

| Job | Does |
|---|---|
| Ubuntu 24.04, Rust 1.92 | clippy with warnings as errors, tests under a virtual display (so the GTK test runs), desktop and metainfo validation, release build, `.deb` as an artifact |
| Arch Linux | build and tests in an `archlinux:latest` container with the distribution's Rust |
| Windows (MSYS2 UCRT64) | release build, bundle, installer and portable exe as artifacts |

## Releasing

1. Set `version` in `Cargo.toml`, add a `<release>` to
   `data/io.github.ykurt52.termvisor.metainfo.xml`, set `pkgver` in the
   PKGBUILD.
2. Commit, then tag and push: `git tag v1.0.1 && git push origin refs/tags/v1.0.1`
   (full ref, since the `v1.0.1` branch has the same name).
3. `release.yml` checks that the tag matches `Cargo.toml`, builds the
   `.deb` and creates the GitHub release with it, adds the Windows installer and portable exe,
   then `apt.yml` publishes the APT repository.
4. Update the AUR package (above).
