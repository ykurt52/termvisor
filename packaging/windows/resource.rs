// Windows icon and version details for an exe, shared by the build scripts
// of termvisor and the portable launcher (`include!`d, not a module).
// Needs a resource compiler (windres in MSYS2); without one the exe builds
// without them, unless TERMVISOR_RC_REQUIRED is set (CI sets it).

#[allow(dead_code)]
fn windows_resource(version: &str, icon: &std::path::Path, description: &str, file: &str) {
    println!("cargo:rerun-if-changed={}", icon.display());
    println!("cargo:rerun-if-env-changed=TERMVISOR_RC_REQUIRED");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut nums: Vec<u32> = version.split(['-', '+']).next().unwrap_or("0").split('.').map(|n| n.parse().unwrap_or(0)).collect();
    nums.resize(4, 0);
    let commas = nums.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let icon = icon.display().to_string().replace('\\', "/");
    let rc = format!(
        r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {commas}
PRODUCTVERSION {commas}
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Yunus Kurt"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "termvisor"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
      VALUE "OriginalFilename", "{file}"
      VALUE "ProductName", "termvisor"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let path = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("termvisor.rc");
    std::fs::write(&path, rc).expect("write termvisor.rc");
    match embed_resource::compile(&path, embed_resource::NONE) {
        embed_resource::CompilationResult::Ok | embed_resource::CompilationResult::NotWindows => {}
        embed_resource::CompilationResult::NotAttempted(why) if std::env::var_os("TERMVISOR_RC_REQUIRED").is_none() => {
            println!("cargo:warning=no icon or version info in the exe: {why}");
        }
        other => panic!("windows resource: {other}"),
    }
}
