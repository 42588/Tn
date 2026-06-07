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

mod ssh;
pub use ssh::{list_ssh_config_hosts, SshBackend, SshConfig, SshHostEntry};

pub mod remote_fs;

pub mod wsl;

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
    /// Start a spec for `program` (an executable name or path); chain
    /// [`arg`](Self::arg) / [`cwd`](Self::cwd) / [`env`](Self::env) to refine it.
    pub fn program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            ..Default::default()
        }
    }

    /// Append one command-line argument (builder style).
    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    /// Set the child's working directory.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Add one environment variable for the child.
    pub fn env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }
}

/// A handle that can terminate a spawned PTY child process.
pub trait Killer: Send {
    fn kill(&mut self) -> anyhow::Result<()>;
}

/// Which SSH auth method ultimately succeeded — surfaced to the UI so the
/// recent-connections list can show a key/password badge (A1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthKind {
    PublicKey,
    Password,
    KeyboardInteractive,
}

/// A step in establishing an SSH session — drives the connection progress card
/// (B1). Ordinal order = display order; the UI marks steps before the current as
/// done, the current as active, and later ones as pending. Only phases the
/// backend can *honestly* observe are listed (TCP handshake + host-key check are
/// one atomic `connect()` step in russh, so they fold into `Connecting`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshPhase {
    /// TCP connect + SSH transport handshake + host-key verification.
    Connecting,
    /// Offering keys / password to the server.
    Authenticating,
    /// Opening the remote PTY + shell.
    OpeningShell,
}

impl SshPhase {
    /// Position in the fixed step list (for done/active/pending comparison).
    pub fn ordinal(self) -> usize {
        match self {
            SshPhase::Connecting => 0,
            SshPhase::Authenticating => 1,
            SshPhase::OpeningShell => 2,
        }
    }
}

/// Why an SSH connection failed — drives the actionable error card (C1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshErrorKind {
    /// All offered auth methods were rejected (keys + password).
    Auth,
    /// The server's host key didn't match `~/.ssh/known_hosts` (possible MITM).
    HostKeyMismatch,
}

/// The user's decision on an unrecognized SSH host key (B2 TOFU trust panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// Abort — don't trust this key.
    Reject,
    /// Trust for this session only (do not persist to known_hosts).
    AcceptOnce,
    /// Trust and append to `~/.ssh/known_hosts`.
    AcceptAndSave,
}

/// Reply from the UI to an SSH password prompt.
pub struct PasswordReply {
    pub password: String,
    /// Cache this password for the current in-memory SSH session only.
    pub remember: bool,
}

/// An event emitted by a PTY backend that requires UI interaction.
pub enum PtyEvent {
    /// The backend needs a password to continue authentication.
    NeedPassword {
        /// The prompt to display to the user.
        prompt: String,
        /// A previous-attempt error to show in red (B3 in-place retry), e.g.
        /// "密码错误,请重试(第 2 次,共 3 次)". `None` on the first ask.
        error: Option<String>,
        /// A channel to send the password back. If dropped without sending, auth fails.
        reply: std::sync::mpsc::Sender<PasswordReply>,
    },
    /// SSH connection progressed to a new phase (B1 progress card). `detail` is a
    /// short human note for the active step (resolved host, key file, …).
    SshProgress { phase: SshPhase, detail: String },
    /// SSH connection failed unrecoverably (C1 error card). `offered` lists the
    /// auth methods the server advertised (e.g. `publickey · password`), empty if
    /// unknown.
    SshFailed {
        kind: SshErrorKind,
        detail: String,
        offered: String,
    },
    /// First connection to an unrecognized host (B2 TOFU): the UI shows a trust
    /// panel with the SHA256 `fingerprint` and replies with the user's verdict.
    /// Dropping the reply without sending ⇒ reject.
    NeedHostKeyConfirm {
        host: String,
        fingerprint: String,
        reply: std::sync::mpsc::Sender<HostKeyVerdict>,
    },
    /// Authentication succeeded and the remote shell is open — the UI records
    /// this target as a recent connection, tagged with the method used.
    Connected { method: AuthKind },
    /// The connection was lost. The UI can choose to reconnect.
    Disconnected,
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

    /// Try to receive an asynchronous event from the backend (e.g. password prompt).
    fn try_recv_event(&mut self) -> Option<PtyEvent> {
        None
    }
    /// Provide a waker callback that the backend can call when a new event is available.
    fn set_waker(&mut self, _waker: Box<dyn Fn() + Send + Sync>) {}
}
