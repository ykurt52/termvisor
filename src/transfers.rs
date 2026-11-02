// Transfer queue: uploads and downloads over SFTP on worker threads.
//
// Each worker keeps one SFTP session per host command and reuses it. A job
// is one top-level file or directory; directories are copied recursively.
// Downloads go to "<name>.part" and are renamed when complete.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::sftp::{self, Sftp};
use crate::term::SftpCmd;

pub const WORKERS: usize = 4;

/// What to do when the target already exists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Conflict {
    Rename,
    Overwrite,
    Skip,
}

impl Conflict {
    pub const ALL: [Conflict; 3] = [Conflict::Rename, Conflict::Overwrite, Conflict::Skip];

    pub fn label(self) -> &'static str {
        match self {
            Conflict::Rename => "Keep both",
            Conflict::Overwrite => "Overwrite",
            Conflict::Skip => "Skip",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    Up,
    Down,
}

/// Command that opens an SFTP session (see term::sftp_command).
pub type Cmd = Arc<SftpCmd>;

struct Job {
    id: u64,
    dir: Dir,
    local: PathBuf,
    remote: String,
    cmd: Cmd,
}

pub enum Update {
    Started { id: u64, total: u64 },
    Progress { id: u64, done: u64 },
    Done { id: u64, bytes: u64, skipped: usize },
    Failed { id: u64, msg: String },
}

/// Handle to the worker pool. Cheap to share.
pub struct Queue {
    jobs: mpsc::Sender<Job>,
    cancelled: Arc<Mutex<HashSet<u64>>>,
    conflict: Arc<AtomicU8>,
    next: Mutex<u64>,
}

impl Queue {
    /// Starts the workers. Updates arrive on the returned receiver.
    pub fn start() -> (Queue, async_channel::Receiver<Update>) {
        let (jtx, jrx) = mpsc::channel::<Job>();
        let jrx = Arc::new(Mutex::new(jrx));
        let (utx, urx) = async_channel::unbounded();
        let cancelled: Arc<Mutex<HashSet<u64>>> = Arc::default();
        let conflict = Arc::new(AtomicU8::new(0));
        for n in 0..WORKERS {
            let (jrx, utx, cancelled, conflict) = (jrx.clone(), utx.clone(), cancelled.clone(), conflict.clone());
            let spawned = thread::Builder::new()
                .name(format!("transfer-{n}"))
                .spawn(move || worker(jrx, utx, cancelled, conflict));
            if let Err(e) = spawned {
                crate::warn(&format!("transfer worker: {e}"));
            }
        }
        let q = Queue { jobs: jtx, cancelled, conflict, next: Mutex::new(1) };
        (q, urx)
    }

    /// Queues a transfer and returns its id.
    pub fn add(&self, dir: Dir, local: PathBuf, remote: String, cmd: Cmd) -> u64 {
        let id = match self.next.lock() {
            Ok(mut n) => {
                *n += 1;
                *n
            }
            Err(_) => 0,
        };
        let _ = self.jobs.send(Job { id, dir, local, remote, cmd });
        id
    }

    pub fn cancel(&self, id: u64) {
        if let Ok(mut c) = self.cancelled.lock() {
            c.insert(id);
        }
    }

    pub fn set_conflict(&self, c: Conflict) {
        let v = Conflict::ALL.iter().position(|x| *x == c).unwrap_or(0) as u8;
        self.conflict.store(v, Ordering::Relaxed);
    }
}

/// "name (1).ext", "name (2).ext", … for the first free one.
pub fn free_name(name: &str, exists: &mut dyn FnMut(&str) -> bool) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|c| !exists(c))
        .unwrap_or_else(|| name.to_string())
}

struct Ctx<'a> {
    id: u64,
    tx: &'a async_channel::Sender<Update>,
    cancelled: &'a Mutex<HashSet<u64>>,
    conflict: Conflict,
    done: u64,
    skipped: usize,
    last: Instant,
}

impl Ctx<'_> {
    fn stop(&self) -> bool {
        self.cancelled.lock().map(|c| c.contains(&self.id)).unwrap_or(false)
    }

    /// Reports `base + n` at most every 100 ms; false when cancelled.
    fn tick(&mut self, base: u64, n: u64) -> bool {
        if self.last.elapsed() >= Duration::from_millis(100) {
            self.last = Instant::now();
            let _ = self.tx.send_blocking(Update::Progress { id: self.id, done: base + n });
        }
        !self.stop()
    }
}

fn worker(
    jobs: Arc<Mutex<mpsc::Receiver<Job>>>,
    tx: async_channel::Sender<Update>,
    cancelled: Arc<Mutex<HashSet<u64>>>,
    conflict: Arc<AtomicU8>,
) {
    let mut sessions: HashMap<Vec<String>, Sftp> = HashMap::new();
    loop {
        let job = match jobs.lock() {
            Ok(rx) => match rx.recv() {
                Ok(j) => j,
                Err(_) => return,
            },
            Err(_) => return,
        };
        let policy = Conflict::ALL.get(conflict.load(Ordering::Relaxed) as usize).copied().unwrap_or(Conflict::Rename);
        let mut ctx = Ctx { id: job.id, tx: &tx, cancelled: &cancelled, conflict: policy, done: 0, skipped: 0, last: Instant::now() };
        if ctx.stop() {
            let _ = tx.send_blocking(Update::Failed { id: job.id, msg: "cancelled".into() });
            continue;
        }

        let key = job.cmd.0.clone();
        if !sessions.contains_key(&key) {
            match Sftp::spawn(&job.cmd.0, &job.cmd.1) {
                Ok(s) => {
                    sessions.insert(key.clone(), s);
                }
                Err(e) => {
                    let _ = tx.send_blocking(Update::Failed { id: job.id, msg: e });
                    continue;
                }
            }
        }
        let Some(s) = sessions.get_mut(&key) else { continue };
        let res = run(s, &job, &mut ctx);
        match res {
            Ok(()) => {
                let _ = tx.send_blocking(Update::Done { id: job.id, bytes: ctx.done, skipped: ctx.skipped });
            }
            Err(msg) => {
                // A broken pipe leaves the session unusable; start fresh next time.
                if msg != "cancelled" && !msg.starts_with("no such file") && !msg.starts_with("permission denied") {
                    sessions.remove(&key);
                }
                let _ = tx.send_blocking(Update::Failed { id: job.id, msg });
            }
        }
    }
}

fn run(s: &mut Sftp, job: &Job, ctx: &mut Ctx) -> Result<(), String> {
    match job.dir {
        Dir::Down => {
            let total = remote_size(s, &job.remote)?;
            let _ = ctx.tx.send_blocking(Update::Started { id: job.id, total });
            download(s, &job.remote, &job.local, ctx)
        }
        Dir::Up => {
            let total = local_size(&job.local);
            let _ = ctx.tx.send_blocking(Update::Started { id: job.id, total });
            upload(s, &job.local, &job.remote, ctx)
        }
    }
}

fn remote_size(s: &mut Sftp, path: &str) -> Result<u64, String> {
    let a = s.stat(path)?;
    if !a.is_dir() {
        return Ok(a.size.unwrap_or(0));
    }
    let mut sum = 0;
    for e in s.list(path)? {
        sum += if e.attrs.is_dir() { remote_size(s, &sftp::join(path, &e.name))? } else { e.attrs.size.unwrap_or(0) };
    }
    Ok(sum)
}

fn local_size(path: &Path) -> u64 {
    match fs::metadata(path) {
        Ok(m) if m.is_dir() => fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| local_size(&e.path())).sum())
            .unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Final local path after the conflict policy; None to skip.
fn local_target(path: &Path, policy: Conflict) -> Option<PathBuf> {
    if !path.exists() {
        return Some(path.to_path_buf());
    }
    match policy {
        Conflict::Overwrite => Some(path.to_path_buf()),
        Conflict::Skip => None,
        Conflict::Rename => {
            let dir = path.parent().unwrap_or(Path::new("."));
            let name = path.file_name()?.to_string_lossy().into_owned();
            Some(dir.join(free_name(&name, &mut |c| dir.join(c).exists())))
        }
    }
}

fn download(s: &mut Sftp, remote: &str, local: &Path, ctx: &mut Ctx) -> Result<(), String> {
    if ctx.stop() {
        return Err("cancelled".into());
    }
    let a = s.stat(remote)?;
    if a.is_dir() {
        // Directories merge into an existing one of the same name.
        fs::create_dir_all(local).map_err(|e| format!("{}: {e}", local.display()))?;
        for e in s.list(remote)? {
            download(s, &sftp::join(remote, &e.name), &local.join(&e.name), ctx)?;
        }
        return Ok(());
    }
    let Some(target) = local_target(local, ctx.conflict) else {
        ctx.skipped += 1;
        ctx.done += a.size.unwrap_or(0);
        return Ok(());
    };
    let part = {
        let mut p = target.clone().into_os_string();
        p.push(".part");
        PathBuf::from(p)
    };
    let mut f = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let base = ctx.done;
    let res = s.download(remote, &mut f, &mut |n| ctx.tick(base, n));
    drop(f);
    match res {
        Ok(n) => {
            fs::rename(&part, &target).map_err(|e| format!("{}: {e}", target.display()))?;
            if let Some(p) = a.perm {
                let _ = fs::set_permissions(&target, fs::Permissions::from_mode(p & 0o777));
            }
            ctx.done = base + n;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        }
    }
}

fn upload(s: &mut Sftp, local: &Path, remote: &str, ctx: &mut Ctx) -> Result<(), String> {
    if ctx.stop() {
        return Err("cancelled".into());
    }
    let meta = fs::metadata(local).map_err(|e| format!("{}: {e}", local.display()))?;
    if meta.is_dir() {
        if s.stat(remote).is_err() {
            s.mkdir(remote)?;
        }
        let mut kids: Vec<PathBuf> = fs::read_dir(local)
            .map_err(|e| format!("{}: {e}", local.display()))?
            .flatten()
            .map(|e| e.path())
            .collect();
        kids.sort();
        for k in kids {
            let name = k.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            upload(s, &k, &sftp::join(remote, &name), ctx)?;
        }
        return Ok(());
    }

    let mut target = remote.to_string();
    if s.stat(remote).is_ok() {
        match ctx.conflict {
            Conflict::Overwrite => {}
            Conflict::Skip => {
                ctx.skipped += 1;
                ctx.done += meta.len();
                return Ok(());
            }
            Conflict::Rename => {
                let dir = sftp::parent(remote);
                let name = remote.rsplit('/').next().unwrap_or(remote).to_string();
                target = sftp::join(&dir, &free_name(&name, &mut |c| s.stat(&sftp::join(&dir, c)).is_ok()));
            }
        }
    }
    let mut f = File::open(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let base = ctx.done;
    let n = s.upload(&mut f, &target, meta.permissions().mode() & 0o777, &mut |n| ctx.tick(base, n))?;
    ctx.done = base + n;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> Option<Cmd> {
        ["/usr/lib/openssh/sftp-server", "/usr/lib/ssh/sftp-server"]
            .iter()
            .find(|p| Path::new(p).exists())
            .map(|p| Arc::new((vec![p.to_string()], Vec::new())))
    }

    #[test]
    fn free_names() {
        let taken = ["a (1).txt", "a (2).txt"];
        assert_eq!(free_name("a.txt", &mut |c| taken.contains(&c)), "a (3).txt");
        assert_eq!(free_name("noext", &mut |_| false), "noext (1)");
        assert_eq!(free_name(".bashrc", &mut |_| false), ".bashrc (1)");
    }

    fn wait(rx: &async_channel::Receiver<Update>, id: u64) -> Result<(u64, usize), String> {
        loop {
            match rx.recv_blocking() {
                Ok(Update::Done { id: i, bytes, skipped }) if i == id => return Ok((bytes, skipped)),
                Ok(Update::Failed { id: i, msg }) if i == id => return Err(msg),
                Ok(_) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    }

    #[test]
    fn queue_copies_trees_both_ways() {
        let Some(cmd) = server() else { return };
        let root = std::env::temp_dir().join(format!("tv-xfer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        fs::create_dir_all(src.join("sub")).expect("dirs");
        fs::write(src.join("a.txt"), b"alpha").expect("a");
        fs::write(src.join("sub/b.bin"), vec![7u8; 100_000]).expect("b");
        let remote = root.join("remote");
        fs::create_dir_all(&remote).expect("remote");
        let back = root.join("back");
        fs::create_dir_all(&back).expect("back");

        let (q, rx) = Queue::start();
        let rdir = remote.join("src").to_string_lossy().into_owned();
        let up = q.add(Dir::Up, src.clone(), rdir.clone(), cmd.clone());
        assert_eq!(wait(&rx, up), Ok((100_005, 0)));
        assert_eq!(fs::read(remote.join("src/sub/b.bin")).expect("b").len(), 100_000);

        let down = q.add(Dir::Down, back.join("src"), rdir.clone(), cmd.clone());
        assert_eq!(wait(&rx, down), Ok((100_005, 0)));
        assert_eq!(fs::read(back.join("src/a.txt")).expect("a"), b"alpha");
        assert!(!back.join("src/a.txt.part").exists());

        // Same download again keeps both copies by default.
        let again = q.add(Dir::Down, back.join("src"), rdir.clone(), cmd.clone());
        assert!(wait(&rx, again).is_ok());
        assert!(back.join("src/a (1).txt").exists());

        q.set_conflict(Conflict::Skip);
        let skip = q.add(Dir::Down, back.join("src"), rdir, cmd.clone());
        assert_eq!(wait(&rx, skip).map(|r| r.1), Ok(2));

        let missing = q.add(Dir::Down, back.join("x"), "/nonexistent/termvisor".into(), cmd);
        assert!(wait(&rx, missing).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
