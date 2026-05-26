//! Tn PTY backends.
//!
//! Defines the [`PtyBackend`] trait — a uniform synchronous reader/writer +
//! resize + lifecycle interface — and a local implementation backed by the OS
//! (ConPTY on Windows) via `portable-pty`. WSL and SSH (`russh`) backends are
//! added in later milestones and implement the same trait, so the byte pump
//! that drives the terminal engine stays backend-agnostic.

use std::io::{Read, Write};
use std::path::PathBuf;

mod local;
pub use local::LocalPty;

/// Pseudo-terminal size in character cells (with optional pixel dimensions,
/// which some full-screen apps use for sixel/image sizing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PtySize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl From<PtySize> for portable_pty::PtySize {
    fn from(s: PtySize) -> Self {
        portable_pty::PtySize {
            rows: s.rows,
            cols: s.cols,
            pixel_width: s.pixel_width,
            pixel_height: s.pixel_height,
        }
    }
}

/// Describes a process (or remote shell) to launch inside a PTY.
#[derive(Clone, Debug, Default)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

impl SpawnSpec {
    pub fn program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Default::default()
        }
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
}

/// A handle that can terminate a spawned PTY child process.
pub trait Killer: Send {
    fn kill(&mut self) -> anyhow::Result<()>;
}

/// A pseudo-terminal session: a synchronous byte source/sink plus resize and
/// lifecycle. Implemented by every backend (local, WSL, SSH).
pub trait PtyBackend: Send {
    /// Inform the child that the window size changed.
    fn resize(&self, size: PtySize) -> anyhow::Result<()>;
    /// Take the output reader (errors if already taken).
    fn take_reader(&mut self) -> anyhow::Result<Box<dyn Read + Send>>;
    /// Obtain a writer for sending input to the PTY.
    fn writer(&self) -> anyhow::Result<Box<dyn Write + Send>>;
    /// Obtain a handle that can kill the child process.
    fn killer(&self) -> anyhow::Result<Box<dyn Killer>>;
    /// Block until the child exits, returning its exit code.
    fn wait(&mut self) -> anyhow::Result<i32>;
    /// Poll whether the child has exited yet, without blocking.
    fn try_wait(&mut self) -> anyhow::Result<Option<i32>>;
}
