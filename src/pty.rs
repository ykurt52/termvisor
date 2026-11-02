// Terminal with its own pty. The child runs on the pty slave; a reader
// thread passes its output to the GTK thread, which runs it through the
// output pipeline (timestamps, highlight, log) and feeds the terminal.

use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use gtk::{glib, prelude::*};
use vte::prelude::*;
use vte::subclass::prelude::*;

use crate::output::Out;

/// Largest block fed to the terminal at once.
const BATCH: usize = 1 << 20;

pub enum Event {
    Data(Vec<u8>),
    /// Wait status of the child, as from waitpid().
    Exit(i32),
}

/// Running child on a pty. Dropping it hangs up the child.
pub struct Child {
    pid: libc::pid_t,
    /// Master side, for resizing.
    ctl: OwnedFd,
    input: mpsc::Sender<Vec<u8>>,
    alive: Arc<AtomicBool>,
}

impl Child {
    pub fn write(&self, data: &[u8]) {
        let _ = self.input.send(data.to_vec());
    }

    pub fn resize(&self, cols: i64, rows: i64) {
        let ws = libc::winsize {
            ws_row: rows.clamp(1, u16::MAX as i64) as u16,
            ws_col: cols.clamp(1, u16::MAX as i64) as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid fd and winsize; the kernel sends SIGWINCH.
        unsafe { libc::ioctl(self.ctl.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // Only while not reaped, so a reused pid is never hit.
        if self.alive.load(Ordering::Acquire) {
            // SAFETY: plain signal to our own child.
            unsafe { libc::kill(self.pid, libc::SIGHUP) };
        }
    }
}

fn cloexec(fd: i32) {
    // SAFETY: fcntl on an fd we own.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
}

/// `VTE_VERSION` as shells expect it (e.g. 7603); their integration
/// scripts report the working directory only when it is set.
fn vte_version() -> String {
    // SAFETY: plain getters.
    let (a, b, c) = unsafe {
        (vte::ffi::vte_get_major_version(), vte::ffi::vte_get_minor_version(), vte::ffi::vte_get_micro_version())
    };
    (a * 10000 + b * 100 + c).to_string()
}

/// Starts `argv` on a new pty of `cols` x `rows`.
pub fn spawn(argv: &[String], env: &[(String, String)], dir: &str, cols: i64, rows: i64) -> io::Result<(Child, async_channel::Receiver<Event>)> {
    let prog = argv.first().ok_or_else(|| io::Error::other("empty command"))?;
    let (mut master, mut slave) = (0, 0);
    let ws = libc::winsize { ws_row: rows.max(1) as u16, ws_col: cols.max(1) as u16, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: out pointers are valid; name and termios are optional.
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    cloexec(master);
    cloexec(slave);
    // SAFETY: openpty returned two fresh fds that nothing else owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

    let mut cmd = Command::new(prog);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("VTE_VERSION", vte_version())
        .env_remove("COLUMNS")
        .env_remove("LINES")
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // The parent's slave copies are closed when `cmd` drops, so the reader
    // sees EIO once the child side is gone.
    let mut child = cmd.spawn()?;
    drop(cmd);
    let pid = child.id() as libc::pid_t;

    let (tx, rx) = async_channel::bounded(64);
    let mut reader = File::from(master.try_clone()?);
    let mut writer = File::from(master.try_clone()?);
    let alive = Arc::new(AtomicBool::new(true));

    let (done_tx, done_rx) = mpsc::channel::<()>();
    let out = tx.clone();
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Blocks while the GTK side is behind, which stalls the child.
                    if out.send_blocking(Event::Data(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = done_tx.send(());
    });

    let (input, input_rx) = mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        for data in input_rx {
            if writer.write_all(&data).is_err() {
                break;
            }
        }
    });

    let flag = alive.clone();
    thread::spawn(move || {
        let status = child.wait().map(|s| s.into_raw()).unwrap_or(-1);
        flag.store(false, Ordering::Release);
        // Let the reader pass the last output first. A background job
        // that keeps the pty open must not delay the exit forever.
        let _ = done_rx.recv_timeout(Duration::from_millis(300));
        let _ = tx.send_blocking(Event::Exit(status));
    });

    Ok((Child { pid, ctl: master, input, alive }, rx))
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Term {
        pub child: RefCell<Option<Child>>,
        pub size: Cell<(i64, i64)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Term {
        const NAME: &'static str = "TvTerm";
        type Type = super::Term;
        type ParentType = vte::Terminal;

        fn class_init(klass: &mut Self::Class) {
            // The bindings install a commit handler that takes ownership of
            // the text VTE still owns (a double free on every commit).
            // Remove it; the commit signal still works without it.
            // SAFETY: the class struct starts with VteTerminalClass.
            let k = unsafe { &mut *(klass as *mut Self::Class as *mut vte::ffi::VteTerminalClass) };
            k.commit = None;
        }
    }

    impl ObjectImpl for Term {
        fn constructed(&self) {
            self.parent_constructed();
            // Typed text, pastes, button sends and terminal replies.
            self.obj().connect_commit(|t, text, _| {
                let sent = match t.imp().child.borrow().as_ref() {
                    Some(c) => {
                        c.write(text.as_bytes());
                        true
                    }
                    None => false,
                };
                // Stream panes have no child here and copy input themselves.
                if sent {
                    crate::multi::typed(t.upcast_ref(), text);
                }
            });
        }
    }

    impl WidgetImpl for Term {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().resized();
        }
    }

    impl TerminalImpl for Term {}
}

/// Writes `data` to the program of terminal `t` as input. False when `t`
/// runs no program here.
pub fn send(t: &vte::Terminal, data: &[u8]) -> bool {
    let Some(t) = t.downcast_ref::<Term>() else { return false };
    match t.imp().child.borrow().as_ref() {
        Some(c) => {
            c.write(data);
            true
        }
        None => false,
    }
}

glib::wrapper! {
    pub struct Term(ObjectSubclass<imp::Term>)
        @extends vte::Terminal, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl Default for Term {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Term {
    fn resized(&self) {
        let size = (self.column_count(), self.row_count());
        let imp = self.imp();
        if imp.size.replace(size) == size {
            return;
        }
        if let Some(c) = imp.child.borrow().as_ref() {
            c.resize(size.0, size.1);
        }
    }

    /// Starts `argv` in this terminal, replacing a previous child. Output
    /// goes through `out`; when the child ends, child-exited is emitted.
    pub fn start(&self, argv: &[String], env: &[(String, String)], dir: &str, out: Rc<RefCell<Out>>) -> io::Result<()> {
        let (child, rx) = spawn(argv, env, dir, self.column_count(), self.row_count())?;
        self.imp().size.set((self.column_count(), self.row_count()));
        self.imp().child.replace(Some(child));
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(t) = weak.upgrade() else { break };
                match ev {
                    Event::Data(mut data) => {
                        // Take what else is waiting, to feed in one go.
                        while data.len() < BATCH {
                            match rx.try_recv() {
                                Ok(Event::Data(more)) => data.extend_from_slice(&more),
                                Ok(Event::Exit(status)) => {
                                    t.show(&out, &data);
                                    t.exited(status);
                                    return;
                                }
                                Err(_) => break,
                            }
                        }
                        t.show(&out, &data);
                    }
                    Event::Exit(status) => {
                        t.exited(status);
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    fn show(&self, out: &RefCell<Out>, data: &[u8]) {
        let shown = out.borrow_mut().process(data);
        self.feed(&shown);
    }

    fn exited(&self, status: i32) {
        self.imp().child.replace(None);
        self.emit_by_name::<()>("child-exited", &[&status]);
    }
}
