// How this copy was installed, and (on Windows) replacing it with a newer
// release in one click.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::update::{Asset, Release};

/// Set by the portable launcher to its own path.
pub const PORTABLE_ENV: &str = "TERMVISOR_PORTABLE";

#[derive(Clone, Debug, PartialEq)]
pub enum Install {
    /// Inno Setup install; the uninstaller sits next to the exe.
    Setup,
    /// Portable launcher at this path.
    Portable(PathBuf),
    /// Distribution package (deb, AUR); the package manager updates it.
    Package,
    /// Unpacked folder or own build: nothing to update in place.
    Other,
}

impl Install {
    pub fn detect() -> Self {
        let Ok(exe) = std::env::current_exe() else { return Self::Other };
        if cfg!(windows) {
            if exe.parent().is_some_and(|d| d.join("unins000.exe").is_file()) {
                return Self::Setup;
            }
            if let Some(p) = std::env::var_os(PORTABLE_ENV).map(PathBuf::from).filter(|p| p.is_file()) {
                return Self::Portable(p);
            }
            return Self::Other;
        }
        if exe.starts_with("/usr/") {
            Self::Package
        } else {
            Self::Other
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Setup => "installer".into(),
            Self::Portable(p) => format!("portable ({})", p.display()),
            Self::Package => "system package".into(),
            Self::Other => "unpacked or own build".into(),
        }
    }
}

/// Release file this install updates from; None when it cannot update itself.
pub fn asset<'a>(install: &Install, release: &'a Release) -> Option<&'a Asset> {
    let suffix = match install {
        Install::Setup => "_setup.exe",
        Install::Portable(_) => "_portable.exe",
        Install::Package | Install::Other => return None,
    };
    release.assets.iter().find(|a| a.name.ends_with(suffix) && a.sha256.is_some())
}

/// Where the new file is downloaded to. A portable exe still named after
/// this version gets the new name next to it; a renamed one is replaced.
pub fn target(install: &Install, asset: &Asset) -> Result<PathBuf, String> {
    match install {
        Install::Portable(old) => {
            let dir = old.parent().ok_or("portable exe has no folder")?;
            let name = old.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if name.contains(&format!("_v{}", env!("CARGO_PKG_VERSION"))) {
                Ok(dir.join(&asset.name))
            } else {
                Ok(old.with_extension("new"))
            }
        }
        _ => {
            let dir = std::env::temp_dir().join("termvisor-update");
            fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            Ok(dir.join(&asset.name))
        }
    }
}

/// Starts the downloaded `file`; the caller quits right after. The
/// installer runs silently and starts termvisor again when done. A portable
/// exe takes the place of the old one.
pub fn apply(install: &Install, file: &Path) -> Result<(), String> {
    let run = match install {
        Install::Setup => {
            return Command::new(file)
                .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/relaunch=1"])
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("{}: {e}", file.display()));
        }
        Install::Portable(old) if file.extension().is_some_and(|e| e == "new") => {
            let bak = old.with_extension("old");
            fs::rename(old, &bak).map_err(|e| format!("{}: {e}", old.display()))?;
            if let Err(e) = fs::rename(file, old) {
                let _ = fs::rename(&bak, old);
                return Err(format!("{}: {e}", old.display()));
            }
            let _ = fs::remove_file(&bak);
            old.clone()
        }
        Install::Portable(old) => {
            let _ = fs::remove_file(old);
            file.to_path_buf()
        }
        Install::Package | Install::Other => return Err("this copy cannot update itself".into()),
    };
    Command::new(&run).spawn().map(|_| ()).map_err(|e| format!("{}: {e}", run.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release() -> Release {
        let a = |name: &str, sha: Option<&str>| Asset { name: name.into(), url: String::new(), size: 1, sha256: sha.map(String::from) };
        Release {
            version: "9.0.0".into(),
            url: String::new(),
            assets: vec![
                a("termvisor-windows-x64_v9.0.0_setup.exe", Some("aa")),
                a("termvisor-windows-x64_v9.0.0_portable.exe", Some("bb")),
                a("termvisor_9.0.0-1_amd64.deb", None),
            ],
        }
    }

    #[test]
    fn picks_the_file_for_the_install() {
        let r = release();
        assert!(asset(&Install::Setup, &r).unwrap().name.ends_with("_setup.exe"));
        assert!(asset(&Install::Portable(PathBuf::from("x")), &r).unwrap().name.ends_with("_portable.exe"));
        assert!(asset(&Install::Package, &r).is_none());
        assert!(asset(&Install::Other, &r).is_none());
    }

    #[test]
    fn portable_target_name() {
        let r = release();
        let a = asset(&Install::Portable(PathBuf::from("x")), &r).unwrap();
        let dir = std::env::temp_dir();
        let named = dir.join(format!("termvisor-windows-x64_v{}_portable.exe", env!("CARGO_PKG_VERSION")));
        assert_eq!(target(&Install::Portable(named), a).unwrap(), dir.join(&a.name));
        let renamed = dir.join("termvisor.exe");
        assert_eq!(target(&Install::Portable(renamed), a).unwrap(), dir.join("termvisor.new"));
    }

    #[test]
    fn portable_swap() {
        let dir = std::env::temp_dir().join(format!("termvisor-swap-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let old = dir.join("termvisor.exe");
        fs::write(&old, "old").unwrap();
        let new = dir.join("termvisor.new");
        fs::write(&new, "#!/bin/sh\n").unwrap();
        // Spawning a non-exe fails, but the swap happens first.
        let _ = apply(&Install::Portable(old.clone()), &new);
        assert_eq!(fs::read_to_string(&old).unwrap(), "#!/bin/sh\n");
        assert!(!new.exists() && !dir.join("termvisor.old").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
