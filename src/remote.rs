// File access shared by the Files panel and the transfer queue: one trait,
// implemented by SFTP and FTP, and the target that opens a session.

use std::io::{Read, Write};

use crate::adb::Adb;
use crate::ftp::{Ftp, FtpCfg};
use crate::sftp::{self, Attrs, DirEntry, Progress, Sftp};
use crate::term::SftpCmd;

pub trait Remote: Send {
    /// Absolute form of `path` ("." is the login directory).
    fn realpath(&mut self, path: &str) -> Result<String, String>;
    /// Entries without "." and "..", directories first.
    fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, String>;
    fn stat(&mut self, path: &str) -> Result<Attrs, String>;
    fn mkdir(&mut self, path: &str) -> Result<(), String>;
    fn rmdir(&mut self, path: &str) -> Result<(), String>;
    fn remove(&mut self, path: &str) -> Result<(), String>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), String>;
    fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String>;
    /// Reads from byte `start` into `out`; returns bytes written.
    fn download(&mut self, path: &str, start: u64, out: &mut dyn Write, progress: Progress) -> Result<u64, String>;
    /// Writes `input` at byte `start` (0 = new file); returns bytes written.
    fn upload(&mut self, input: &mut dyn Read, path: &str, mode: u32, start: u64, progress: Progress) -> Result<u64, String>;

    /// False when the session must not be reused (e.g. FTP after an abort,
    /// where the control channel can hold a stray reply).
    fn healthy(&self) -> bool {
        true
    }

    /// Deletes a file, or a directory with everything inside.
    fn remove_all(&mut self, path: &str) -> Result<(), String> {
        if !self.stat(path)?.is_dir() {
            return self.remove(path);
        }
        for e in self.list(path)? {
            let p = sftp::join(path, &e.name);
            if e.attrs.is_dir() {
                self.remove_all(&p)?;
            } else {
                self.remove(&p)?;
            }
        }
        self.rmdir(path)
    }
}

impl Remote for Sftp {
    fn realpath(&mut self, path: &str) -> Result<String, String> {
        Sftp::realpath(self, path)
    }
    fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, String> {
        Sftp::list(self, path)
    }
    fn stat(&mut self, path: &str) -> Result<Attrs, String> {
        Sftp::stat(self, path)
    }
    fn mkdir(&mut self, path: &str) -> Result<(), String> {
        Sftp::mkdir(self, path)
    }
    fn rmdir(&mut self, path: &str) -> Result<(), String> {
        Sftp::rmdir(self, path)
    }
    fn remove(&mut self, path: &str) -> Result<(), String> {
        Sftp::remove(self, path)
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        Sftp::rename(self, from, to)
    }
    fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String> {
        Sftp::chmod(self, path, mode)
    }
    fn download(&mut self, path: &str, start: u64, out: &mut dyn Write, progress: Progress) -> Result<u64, String> {
        Sftp::download(self, path, start, out, progress)
    }
    fn upload(&mut self, input: &mut dyn Read, path: &str, mode: u32, start: u64, progress: Progress) -> Result<u64, String> {
        Sftp::upload(self, input, path, mode, start, progress)
    }
    fn remove_all(&mut self, path: &str) -> Result<(), String> {
        // lstat-based: never follows a symlink into its target.
        Sftp::remove_all(self, path)
    }
}

/// Where a session connects.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    /// `ssh … -s host sftp` (or a local sftp-server in tests).
    Sftp(SftpCmd),
    /// FTP/FTPS with the password already resolved.
    Ftp(FtpCfg, String),
    /// Android device: adb binary and serial.
    Adb(String, String),
}

impl Target {
    /// Sessions with the same key can be reused.
    pub fn key(&self) -> String {
        match self {
            Target::Sftp((argv, _)) => argv.join("\u{1f}"),
            Target::Ftp(c, _) => c.id(),
            Target::Adb(_, serial) => format!("adb:{serial}"),
        }
    }

    pub fn open(&self) -> Result<Box<dyn Remote>, String> {
        match self {
            Target::Sftp((argv, env)) => Ok(Box::new(Sftp::spawn(argv, env)?)),
            Target::Ftp(c, pass) => Ok(Box::new(Ftp::open(c, pass)?)),
            Target::Adb(bin, serial) => Ok(Box::new(Adb::new(bin, serial))),
        }
    }
}
