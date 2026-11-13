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

/// Command for a child without a terminal of its own (adb, ssh for tunnels
/// and SFTP, ping). On Windows it runs without a console window.
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    }
    c
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

/// Lets Windows move the window when its header bar is dragged, so it
/// snaps to screen edges (top: maximize) like other apps. GTK moves
/// client-side decorated windows itself, and Windows never sees that.
/// Presses on buttons and entries are left to GTK, and so are double
/// clicks (maximize).
#[cfg(windows)]
pub fn native_move(header: &impl gtk::prelude::IsA<gtk::Widget>, window: &impl gtk::prelude::IsA<gtk::Window>) {
    use gtk::prelude::*;
    // TERMVISOR_GTK_MOVE=1 leaves moving to GTK, to compare the two.
    if std::env::var_os("TERMVISOR_GTK_MOVE").is_some() {
        return;
    }
    let g = gtk::GestureClick::new();
    g.set_button(1);
    g.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (h, w) = (header.as_ref().downgrade(), window.as_ref().downgrade());
    g.connect_pressed(move |g, n, x, y| {
        let (Some(h), Some(w)) = (h.upgrade(), w.upgrade()) else { return };
        if n != 1 || h.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|t| win::interactive(&t, &h)) {
            return;
        }
        let Some(surface) = w.surface() else { return };
        g.set_state(gtk::EventSequenceState::Claimed);
        g.reset();
        win::drag(&surface);
    });
    header.as_ref().add_controller(g);
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

    /// True for a widget that handles its own clicks: buttons, entries.
    pub fn interactive(target: &gtk::Widget, stop: &gtk::Widget) -> bool {
        use gtk::prelude::*;
        let mut cur = Some(target.clone());
        while let Some(w) = cur {
            if &w == stop {
                return false;
            }
            if w.is::<gtk::Button>() || w.is::<gtk::MenuButton>() || w.is::<gtk::Text>() || w.is::<gtk::Entry>() || w.is::<gtk::DropDown>() {
                return true;
            }
            cur = w.parent();
        }
        false
    }

    unsafe extern "C" {
        fn gdk_win32_surface_get_handle(surface: *mut gtk::gdk::ffi::GdkSurface) -> *mut std::ffi::c_void;
    }

    /// Starts the Windows move loop for `surface`, as if its title bar was
    /// pressed.
    pub fn drag(surface: &gtk::gdk::Surface) {
        use gtk::glib::translate::ToGlibPtr;
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, HTCAPTION, WM_NCLBUTTONDOWN};
        // SAFETY: the surface is a live Win32 surface; the handle is only
        // used for these two calls.
        unsafe {
            let hwnd = gdk_win32_surface_get_handle(surface.to_glib_none().0);
            if hwnd.is_null() {
                return;
            }
            ReleaseCapture();
            PostMessageW(hwnd as _, WM_NCLBUTTONDOWN, HTCAPTION as usize, 0);
        }
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
