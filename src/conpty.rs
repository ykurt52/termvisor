// Windows pseudo console (ConPTY): the Windows side of pty.rs. The child
// runs attached to a pseudo console; two pipes carry its input and output.
// Needs Windows 10 1809 or newer.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTUPINFOEXW,
};

use crate::pty::{Event, TERM_ENV};

/// The pseudo console, closed once by whoever is first: the child ending
/// or the pane closing. Closing it ends the child and the output pipe.
#[derive(Clone)]
struct Console(Arc<Mutex<Option<isize>>>);

impl Console {
    fn close(&self) {
        let h = self.0.lock().ok().and_then(|mut g| g.take());
        if let Some(h) = h {
            // SAFETY: the handle came from CreatePseudoConsole and is closed once.
            unsafe { ClosePseudoConsole(h as HPCON) };
        }
    }
}

/// Running child on a pseudo console. Dropping it ends the child.
pub struct Child {
    console: Console,
    input: mpsc::Sender<Vec<u8>>,
}

fn coord(cols: i64, rows: i64) -> COORD {
    COORD { X: cols.clamp(1, i16::MAX as i64) as i16, Y: rows.clamp(1, i16::MAX as i64) as i16 }
}

impl Child {
    pub fn write(&self, data: &[u8]) {
        let _ = self.input.send(data.to_vec());
    }

    pub fn resize(&self, cols: i64, rows: i64) {
        if let Some(h) = self.console.0.lock().ok().and_then(|g| *g) {
            // SAFETY: the console is open while it is in the slot.
            unsafe { ResizePseudoConsole(h as HPCON, coord(cols, rows)) };
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // ClosePseudoConsole may wait for the reader, which may wait for
        // the GTK thread we are on; close it elsewhere.
        let c = self.console.clone();
        thread::spawn(move || c.close());
    }
}

fn pipe() -> io::Result<(HANDLE, HANDLE)> {
    let (mut r, mut w): (HANDLE, HANDLE) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: out pointers are valid; default security, not inheritable.
    if unsafe { CreatePipe(&mut r, &mut w, std::ptr::null(), 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((r, w))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// Our environment with the terminal variables and `extra` on top, as a
/// UTF-16 block.
fn env_block(extra: &[(String, String)]) -> Vec<u16> {
    let mut vars: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !k.eq_ignore_ascii_case("COLUMNS") && !k.eq_ignore_ascii_case("LINES")).collect();
    let set = TERM_ENV.iter().map(|(k, v)| (k.to_string(), v.to_string())).chain(extra.iter().cloned());
    for (k, v) in set {
        vars.retain(|(x, _)| !x.eq_ignore_ascii_case(&k));
        vars.push((k, v));
    }
    vars.sort_by_key(|(k, _)| k.to_uppercase());
    let mut block = Vec::new();
    for (k, v) in vars {
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

/// Starts `argv` on a new pseudo console of `cols` x `rows`.
pub fn spawn(argv: &[String], env: &[(String, String)], dir: &str, cols: i64, rows: i64) -> io::Result<(Child, async_channel::Receiver<Event>)> {
    if argv.is_empty() {
        return Err(io::Error::other("empty command"));
    }
    let (in_read, in_write) = pipe()?;
    let (out_read, out_write) = pipe()?;
    let mut hpc: HPCON = 0 as HPCON;
    // SAFETY: valid pipe ends; the console keeps its own references.
    let hr = unsafe { CreatePseudoConsole(coord(cols, rows), in_read, out_write, 0, &mut hpc) };
    // SAFETY: our copies of the console side are no longer needed.
    unsafe {
        CloseHandle(in_read);
        CloseHandle(out_write);
    }
    // SAFETY: the other ends are ours alone from here on.
    let (reader, writer) = unsafe { (File::from_raw_handle(out_read as _), File::from_raw_handle(in_write as _)) };
    if hr != 0 {
        return Err(io::Error::from_raw_os_error(hr));
    }
    let console = Console(Arc::new(Mutex::new(Some(hpc as isize))));

    let mut cmd = wide(&argv.iter().map(|a| crate::sys::quote(a)).collect::<Vec<_>>().join(" "));
    let envs = env_block(env);
    let cwd = wide(dir);
    let process = {
        let mut size = 0usize;
        // SAFETY: the first call only reports the size.
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size) };
        let mut buf = vec![0u8; size];
        let list = buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        // SAFETY: `buf` has the size asked for and outlives the list; the
        // console handle is passed by value as the API wants.
        unsafe {
            if InitializeProcThreadAttributeList(list, 1, 0, &mut size) == 0 {
                console.close();
                return Err(io::Error::last_os_error());
            }
            let ok = UpdateProcThreadAttribute(list, 0, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize, hpc as *const c_void, std::mem::size_of::<HPCON>(), std::ptr::null_mut(), std::ptr::null());
            if ok == 0 {
                DeleteProcThreadAttributeList(list);
                console.close();
                return Err(io::Error::last_os_error());
            }
            let mut si: STARTUPINFOEXW = std::mem::zeroed();
            si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            si.lpAttributeList = list;
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                std::ptr::null(),
                cmd.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                envs.as_ptr() as *const c_void,
                cwd.as_ptr(),
                &si.StartupInfo,
                &mut pi,
            );
            let err = io::Error::last_os_error();
            DeleteProcThreadAttributeList(list);
            if created == 0 {
                console.close();
                return Err(io::Error::new(err.kind(), format!("{}: {err}", argv[0])));
            }
            CloseHandle(pi.hThread);
            pi.hProcess as isize
        }
    };

    let (tx, rx) = async_channel::bounded(64);
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let out = tx.clone();
    let mut reader = reader;
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.send_blocking(Event::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = done_tx.send(());
    });

    let (input, input_rx) = mpsc::channel::<Vec<u8>>();
    let mut writer = writer;
    thread::spawn(move || {
        for data in input_rx {
            if writer.write_all(&data).is_err() {
                break;
            }
        }
    });

    let c = console.clone();
    thread::spawn(move || {
        let mut code = 0u32;
        // SAFETY: the process handle is ours and closed here.
        unsafe {
            WaitForSingleObject(process as HANDLE, INFINITE);
            GetExitCodeProcess(process as HANDLE, &mut code);
            CloseHandle(process as HANDLE);
        }
        // Lets the reader see the end of the output.
        c.close();
        let _ = done_rx.recv_timeout(Duration::from_millis(300));
        let _ = tx.send_blocking(Event::Exit(code as i32));
    });

    Ok((Child { console, input }, rx))
}
