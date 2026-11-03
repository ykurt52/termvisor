// Compiles data/ into an embedded GResource bundle; on Windows also adds
// the icon and version details to the exe.

include!("packaging/windows/resource.rs");

fn main() {
    glib_build_tools::compile_resources(
        &["data"],
        "data/resources.gresource.xml",
        "termvisor.gresource",
    );
    windows_resource(
        env!("CARGO_PKG_VERSION"),
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("packaging/windows/termvisor.ico"),
        "Multi-protocol terminal",
        "termvisor.exe",
    );
}
