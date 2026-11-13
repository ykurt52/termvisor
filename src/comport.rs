// Windows COM port with overlapped I/O.
//
// The reader and writer threads each hold a handle to the same port. With a
// synchronous handle Windows serializes every read and write on it, so a
// keystroke waits for the pending read timeout. Overlapped handles let both
// run at the same time.

use std::io;
use std::mem;
use std::ptr;
use std::time::Duration;

use serialport::{Error, ErrorKind};
use windows_sys::Win32::Devices::Communication::{
    ClearCommBreak, EscapeCommFunction, GetCommState, SetCommBreak, SetCommState, SetCommTimeouts, CLRDTR, CLRRTS,
    COMMTIMEOUTS, DCB, ESCAPE_COMM_FUNCTION, EVENPARITY, NOPARITY, ODDPARITY, ONESTOPBIT, SETDTR, SETRTS, TWOSTOPBITS,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
    ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

use crate::serial::{Flow, Parity, SerialCfg};

// DCB flag bits (fBinary .. fAbortOnError occupy bits 0-14).
const DCB_FLAGS: u32 = 0x7FFF;
const DCB_BINARY: u32 = 1 << 0;
const DCB_PARITY_CHECK: u32 = 1 << 1;
const DCB_OUTX_CTS_FLOW: u32 = 1 << 2;
const DCB_DTR_ENABLE: u32 = 1 << 4;
const DCB_OUTX: u32 = 1 << 8;
const DCB_INX: u32 = 1 << 9;
const DCB_RTS_ENABLE: u32 = 1 << 12;

pub struct ComPort {
    handle: HANDLE,
    /// Manual-reset event for the overlapped calls on this handle.
    event: HANDLE,
}

// SAFETY: plain kernel handles; each ComPort is used by one thread at a time.
unsafe impl Send for ComPort {}

/// Opens `cfg.device` with DTR and RTS raised, like a Unix tty open.
pub fn open(cfg: &SerialCfg, timeout: Duration) -> Result<ComPort, Error> {
    let name = path(&cfg.device);
    // SAFETY: name is NUL-terminated; no security attributes or template.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(last_error());
    }
    let port = ComPort::wrap(handle)?;
    port.configure(cfg)?;
    port.set_timeout(timeout)?;
    Ok(port)
}

/// COM3 -> \\.\COM3, NUL-terminated.
fn path(device: &str) -> Vec<u16> {
    let full = if device.starts_with('\\') { device.to_string() } else { format!(r"\\.\{device}") };
    full.encode_utf16().chain(Some(0)).collect()
}

fn last_error() -> Error {
    // SAFETY: no preconditions.
    let code = unsafe { GetLastError() };
    match code {
        ERROR_FILE_NOT_FOUND => Error::new(ErrorKind::NoDevice, "port not found"),
        ERROR_ACCESS_DENIED => Error::new(ErrorKind::NoDevice, "port is busy (open in another program)"),
        _ => {
            let os = io::Error::from_raw_os_error(code as i32);
            Error::new(ErrorKind::Io(os.kind()), os.to_string())
        }
    }
}

impl ComPort {
    /// Takes ownership of `handle` and gives it an event.
    fn wrap(handle: HANDLE) -> Result<Self, Error> {
        // SAFETY: default security, manual reset, initially clear, unnamed.
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            let e = last_error();
            // SAFETY: the handle is ours and not used again.
            unsafe { CloseHandle(handle) };
            return Err(e);
        }
        Ok(Self { handle, event })
    }

    fn configure(&self, cfg: &SerialCfg) -> Result<(), Error> {
        // SAFETY: an all-zero DCB is valid input for GetCommState.
        let mut dcb: DCB = unsafe { mem::zeroed() };
        dcb.DCBlength = mem::size_of::<DCB>() as u32;
        // SAFETY: dcb is a valid out pointer.
        if unsafe { GetCommState(self.handle, &mut dcb) } == 0 {
            return Err(last_error());
        }
        dcb.BaudRate = cfg.baud;
        dcb.ByteSize = cfg.data_bits;
        dcb.Parity = match cfg.parity {
            Parity::None => NOPARITY,
            Parity::Odd => ODDPARITY,
            Parity::Even => EVENPARITY,
        };
        dcb.StopBits = if cfg.stop_bits == 2 { TWOSTOPBITS } else { ONESTOPBIT };
        dcb.XonChar = 0x11;
        dcb.XoffChar = 0x13;
        dcb.ErrorChar = 0;
        dcb.EofChar = 0x1A;
        let mut bits = DCB_BINARY | DCB_DTR_ENABLE | DCB_RTS_ENABLE;
        if cfg.parity != Parity::None {
            bits |= DCB_PARITY_CHECK;
        }
        match cfg.flow {
            Flow::None => {}
            Flow::Software => bits |= DCB_OUTX | DCB_INX,
            Flow::Hardware => bits |= DCB_OUTX_CTS_FLOW,
        }
        dcb._bitfield = (dcb._bitfield & !DCB_FLAGS) | bits;
        // SAFETY: dcb is fully initialized.
        if unsafe { SetCommState(self.handle, &dcb) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// Reads return as soon as a byte is in, or time out after `timeout`.
    fn set_timeout(&self, timeout: Duration) -> Result<(), Error> {
        let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        let t = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: ms,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: ms,
        };
        // SAFETY: t is a valid COMMTIMEOUTS.
        if unsafe { SetCommTimeouts(self.handle, &t) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// A second handle to the same port, with its own event.
    pub fn try_clone_native(&self) -> Result<ComPort, Error> {
        let mut dup: HANDLE = ptr::null_mut();
        // SAFETY: source and target are this process; dup is a valid out pointer.
        let ok = unsafe {
            DuplicateHandle(GetCurrentProcess(), self.handle, GetCurrentProcess(), &mut dup, 0, 0, DUPLICATE_SAME_ACCESS)
        };
        if ok == 0 {
            return Err(last_error());
        }
        ComPort::wrap(dup)
    }

    /// Starts one overlapped call with `start` and waits for it to finish.
    fn wait(&self, start: impl FnOnce(*mut OVERLAPPED) -> i32) -> io::Result<u32> {
        // SAFETY: an all-zero OVERLAPPED is valid.
        let mut ov: OVERLAPPED = unsafe { mem::zeroed() };
        ov.hEvent = self.event;
        let ov_ptr: *mut OVERLAPPED = &mut ov;
        if start(ov_ptr) == 0 {
            // SAFETY: no preconditions.
            let code = unsafe { GetLastError() };
            if code != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(code as i32));
            }
        }
        let mut n = 0u32;
        // SAFETY: ov lives until the call completes; bWait blocks until then.
        if unsafe { GetOverlappedResult(self.handle, &ov, &mut n, 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n)
    }

    fn escape(&self, func: ESCAPE_COMM_FUNCTION) -> Result<(), Error> {
        // SAFETY: the handle is open.
        if unsafe { EscapeCommFunction(self.handle, func) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    pub fn write_data_terminal_ready(&mut self, on: bool) -> Result<(), Error> {
        self.escape(if on { SETDTR } else { CLRDTR })
    }

    pub fn write_request_to_send(&mut self, on: bool) -> Result<(), Error> {
        self.escape(if on { SETRTS } else { CLRRTS })
    }

    pub fn set_break(&self) -> Result<(), Error> {
        // SAFETY: the handle is open.
        if unsafe { SetCommBreak(self.handle) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    pub fn clear_break(&self) -> Result<(), Error> {
        // SAFETY: the handle is open.
        if unsafe { ClearCommBreak(self.handle) } == 0 {
            return Err(last_error());
        }
        Ok(())
    }
}

impl io::Read for ComPort {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let handle = self.handle;
        // SAFETY: buf outlives the call; wait() returns only after it completes.
        let n = self.wait(|ov| unsafe { ReadFile(handle, buf.as_mut_ptr(), len, ptr::null_mut(), ov) })?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "read timed out"));
        }
        Ok(n as usize)
    }
}

impl io::Write for ComPort {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let handle = self.handle;
        // SAFETY: buf outlives the call; wait() returns only after it completes.
        let n = self.wait(|ov| unsafe { WriteFile(handle, buf.as_ptr(), len, ptr::null_mut(), ov) })?;
        Ok(n as usize)
    }

    /// Each write already waits for its completion.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ComPort {
    fn drop(&mut self) {
        // SAFETY: both handles belong to this value and are closed once.
        unsafe {
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}
