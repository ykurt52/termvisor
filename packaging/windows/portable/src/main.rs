// Portable termvisor: unpacks the embedded bundle once into
// %LOCALAPPDATA%\termvisor\portable\<version>-<build> and starts
// termvisor.exe from there, then exits at once. Later starts reuse the
// folder; termvisor removes the folders of other builds itself.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

static BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/bundle.tar.zst"));
const ID: &str = env!("TERMVISOR_ID");
const EXE: &str = "termvisor.exe";

fn main() {
    if let Err(e) = run() {
        fail(&format!("termvisor {} could not start:\n\n{e}", env!("TERMVISOR_VERSION")));
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let base = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    let root = Path::new(&base).join("termvisor").join("portable");
    let dir = prepare(&root, ID, BUNDLE).map_err(|e| format!("unpacking into {}: {e}", root.display()))?;
    // termvisor updates this exe in place, so it needs to know where it is.
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    Command::new(dir.join(EXE))
        .args(std::env::args_os().skip(1))
        .env("TERMVISOR_PORTABLE", me)
        .spawn()
        .map_err(|e| format!("{}: {e}", dir.join(EXE).display()))?;
    // Exit now: Windows shows the "starting" cursor while this runs, and
    // the new window cannot be resized until it is gone.
    Ok(())
}

/// Folder with the unpacked bundle `id`, unpacked now if missing. Unpacks
/// into a temporary folder and renames it, so a folder that exists is whole.
fn prepare(root: &Path, id: &str, bundle: &[u8]) -> io::Result<PathBuf> {
    let dir = root.join(id);
    if dir.join(EXE).is_file() {
        return Ok(dir);
    }
    if bundle.is_empty() {
        return Err(io::Error::other("this build has no bundle"));
    }
    fs::create_dir_all(root)?;
    let tmp = root.join(format!("{id}.tmp{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let unpacked = ruzstd::decoding::StreamingDecoder::new(bundle)
        .map_err(io::Error::other)
        .and_then(|z| tar::Archive::new(z).unpack(&tmp));
    if let Err(e) = unpacked {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    if !tmp.join(EXE).is_file() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(io::Error::other(format!("{EXE} missing from the bundle")));
    }
    // A second copy started at the same time may have won the race.
    if let Err(e) = fs::rename(&tmp, &dir) {
        let _ = fs::remove_dir_all(&tmp);
        if !dir.join(EXE).is_file() {
            return Err(e);
        }
    }
    Ok(dir)
}

#[cfg(windows)]
fn fail(msg: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(msg), wide("termvisor"));
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn fail(msg: &str) {
    eprintln!("{msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        let tar = b.into_inner().unwrap();
        ruzstd::encoding::compress_to_vec(&tar[..], ruzstd::encoding::CompressionLevel::Fastest)
    }

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("termvisor-portable-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn unpacks_once() {
        let root = scratch("once");
        let b = bundle(&[(EXE, b"exe"), ("lib/a.dll", b"dll")]);
        let dir = prepare(&root, "1.0.3-aa", &b).unwrap();
        assert_eq!(fs::read(dir.join("lib/a.dll")).unwrap(), b"dll");
        fs::write(dir.join("marker"), "x").unwrap();
        // Second start reuses the folder.
        assert_eq!(prepare(&root, "1.0.3-aa", &b).unwrap(), dir);
        assert!(dir.join("marker").exists());
        let left: Vec<_> = fs::read_dir(&root).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left, vec![std::ffi::OsString::from("1.0.3-aa")]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn bad_bundles() {
        let root = scratch("bad");
        assert!(prepare(&root, "x", &[]).is_err());
        assert!(prepare(&root, "x", b"not zstd").is_err());
        assert!(prepare(&root, "x", &bundle(&[("other.txt", b"")])).is_err());
        // Nothing half done is left behind.
        assert_eq!(fs::read_dir(&root).map(|d| d.count()).unwrap_or(0), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
