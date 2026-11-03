// Embeds the bundle archive named by TERMVISOR_BUNDLE (a .tar.zst of the
// folder bundle.sh makes) and takes the version from termvisor's
// Cargo.toml, so the two never differ.

use std::hash::{DefaultHasher, Hasher};
use std::path::Path;

include!("../resource.rs");

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let manifest = root.join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let text = std::fs::read_to_string(&manifest).expect("termvisor Cargo.toml");
    let version = text
        .lines()
        .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
        .expect("version in Cargo.toml");

    println!("cargo:rerun-if-env-changed=TERMVISOR_BUNDLE");
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("bundle.tar.zst");
    let data = match std::env::var_os("TERMVISOR_BUNDLE") {
        Some(p) => {
            println!("cargo:rerun-if-changed={}", Path::new(&p).display());
            std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", Path::new(&p).display()))
        }
        None => {
            println!("cargo:warning=TERMVISOR_BUNDLE not set: the launcher has no bundle");
            Vec::new()
        }
    };
    std::fs::write(&out, &data).expect("write bundle");
    let mut h = DefaultHasher::new();
    h.write(&data);
    // Each build unpacks into its own folder, also for two builds of one version.
    println!("cargo:rustc-env=TERMVISOR_ID={version}-{:08x}", h.finish() as u32);
    println!("cargo:rustc-env=TERMVISOR_VERSION={version}");

    windows_resource(version, &root.join("packaging/windows/termvisor.ico"), "termvisor (portable)", &format!("termvisor-windows-x64_v{version}_portable.exe"));
}
