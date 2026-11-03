// Platform differences in one place: private files, executables, process
// ties. Unix has file modes; Windows keeps user files private through the
// profile's ACLs, so the mode calls do nothing there.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;
use std::process::{Child, Command};

/// Files only the user may read (0600 on Unix).
pub fn private(o: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600)
    }
    #[cfg(not(unix))]
    o
}

/// Creates `dir` and its parents; only the user may enter it (0700 on Unix).
pub fn private_dir(dir: &Path) -> io::Result<()> {
    let mut b = fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
}

/// True for a file that can be run.
pub fn executable(m: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.is_file() && m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    m.is_file()
}

/// Unix permission bits, when the platform has them.
pub fn mode(m: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(m.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        None
    }
}

/// Sets Unix permission bits; nothing on other platforms.
pub fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

/// Process id of our parent.
pub fn parent_id() -> u32 {
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id()
    }
    #[cfg(windows)]
    {
        win::parent_id()
    }
}

/// Starts `cmd` so that it ends when termvisor ends, even after a crash.
pub fn spawn_tied(cmd: &mut Command) -> io::Result<Child> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: prctl is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        cmd.spawn()
    }
    #[cfg(windows)]
    {
        let child = cmd.spawn()?;
        win::tie(&child);
        Ok(child)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    cmd.spawn()
}

/// Quotes one argument the way the Windows C runtime splits command lines.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    let mut out = String::from('"');
    let mut slashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => slashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
                out.push('"');
                slashes = 0;
            }
            _ => {
                out.extend(std::iter::repeat_n('\\', slashes));
                out.push(c);
                slashes = 0;
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(windows)]
mod win {
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub fn parent_id() -> u32 {
        let me = std::process::id();
        // SAFETY: plain snapshot walk; the entry has its size set.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return 0;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut parent = 0;
            let mut ok = Process32FirstW(snap, &mut e) != 0;
            while ok {
                if e.th32ProcessID == me {
                    parent = e.th32ParentProcessID;
                    break;
                }
                ok = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            parent
        }
    }

    /// Job whose processes are killed when its last handle closes, which
    /// happens when termvisor exits for any reason. Kept open for good.
    fn job() -> Option<isize> {
        static JOB: OnceLock<Option<isize>> = OnceLock::new();
        *JOB.get_or_init(|| {
            // SAFETY: a fresh job with a valid limit struct.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let size = std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32;
                if SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info as *const _ as *const _, size) == 0 {
                    CloseHandle(job);
                    return None;
                }
                Some(job as isize)
            }
        })
    }

    pub fn tie(child: &Child) {
        if let Some(job) = job() {
            // SAFETY: both handles are valid while `child` lives.
            unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::quote;

    #[test]
    fn quoting() {
        assert_eq!(quote("ssh"), "ssh");
        assert_eq!(quote("a b"), r#""a b""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r"C:\dir with\"), r#""C:\dir with\\""#);
        assert_eq!(quote(r"C:\plain\path"), r"C:\plain\path");
    }
}
