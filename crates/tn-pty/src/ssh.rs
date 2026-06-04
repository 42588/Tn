//! SSH backend (russh) — **M2b**.
//!
//! russh is async (tokio) and hands out a [`Channel`] with `async` send/recv,
//! whereas [`PtyBackend`] is a *synchronous* Read/Write interface (so the byte
//! pump that drives the terminal engine stays backend-agnostic). This module
//! bridges the two: a dedicated thread runs a current-thread tokio runtime that
//! connects, authenticates, opens a PTY shell channel, and pumps a `select!`
//! loop. The sync side talks to that loop over channels:
//!
//! - **reader** (`take_reader`): a [`ChannelReader`] backed by a `std::mpsc`; the
//!   loop forwards `ChannelMsg::Data` into it (blocking `recv` = backpressure-free
//!   EOF when the sender drops).
//! - **writer** (`writer`): pushes bytes onto a tokio `unbounded_channel`; the
//!   loop forwards them via `channel.data_bytes`.
//! - **resize**: `(cols, rows)` over another channel → `channel.window_change`.
//! - **kill / drop**: dropping the backend drops the senders, so every `recv`
//!   resolves to `None` and the loop exits (also `disconnect`s).
//! - **wait / try_wait**: a `Mutex<Option<i32>>` + `Condvar`, set on `ExitStatus`
//!   or channel close.
//!
//! > **Status:** compiles + unit-tested for the headless config/parsing; the live
//! > connect/auth/shell path is **unverified end-to-end** (no test host yet — see
//! > CLAUDE.md M2). Auth chain is key-file → password; **agent + known_hosts
//! > verification are TODO** (currently `check_server_key` accepts any key).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use russh::client::{self, Handle};
use russh::keys::{load_secret_key, ssh_key, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::{Killer, PtyBackend, PtySize};

/// Where + how to connect. Built from a `tn_config` SSH profile (host/user) in
/// the UI layer; `port` defaults to 22, the key is auto-discovered under
/// `~/.ssh` unless given explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Explicit private-key path; if `None`, `~/.ssh/id_*` are tried.
    pub key_path: Option<PathBuf>,
    /// Optional password (config-supplied). Prompting is not yet implemented.
    pub password: Option<String>,
}

impl SshConfig {
    pub fn new(host: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 22,
            user: user.into(),
            key_path: None,
            password: None,
        }
    }

    /// Parse a `[user@]host[:port]` target, merging with `~/.ssh/config`. `user`
    /// falls back to the ssh config, then `$USERNAME` / `$USER`, else `"root"`.
    pub fn parse(target: &str, user: Option<&str>) -> Self {
        let (target_no_user, inline_user) = match target.split_once('@') {
            Some((u, rest)) if !u.is_empty() => (rest, Some(u)),
            _ => (target, None),
        };

        let (mut host, mut port) = match target_no_user.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => {
                (h.to_string(), p.parse().unwrap())
            }
            _ => (target_no_user.to_string(), 22),
        };

        let mut user = inline_user.or(user).map(str::to_string);
        let mut key_path = None;

        if let Some(home) = home_dir() {
            let config_path = home.join(".ssh").join("config");
            if let Ok(file) = std::fs::File::open(&config_path) {
                let mut reader = std::io::BufReader::new(file);
                if let Ok(ssh_cfg) = ssh2_config::SshConfig::default().parse(&mut reader, ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS) {
                    let params = ssh_cfg.query(&host);
                    if let Some(cfg_host) = params.host_name {
                        host = cfg_host;
                    }
                    if let Some(cfg_port) = params.port {
                        port = cfg_port;
                    }
                    if let Some(cfg_user) = params.user {
                        user = Some(cfg_user);
                    }
                    if let Some(keys) = params.identity_file {
                        if !keys.is_empty() {
                            // The path might contain ~ which needs expansion, but ssh2-config
                            // already expands it to absolute path based on home_dir.
                            key_path = Some(keys[0].clone());
                        }
                    }
                }
            }
        }

        let user = user
            .or_else(|| std::env::var("USERNAME").ok())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "root".to_string());
            
        Self { host, port, user, key_path, password: None }
    }

    /// Candidate private keys: the explicit path if set, else the usual
    /// `~/.ssh/id_*` in preference order (only those that exist).
    fn key_candidates(&self) -> Vec<PathBuf> {
        if let Some(p) = &self.key_path {
            return vec![p.clone()];
        }
        match home_dir() {
            Some(home) => key_candidates_in(&home.join(".ssh")),
            None => Vec::new(),
        }
    }
}

/// Existing `id_*` private keys under `ssh_dir`, in preference order. Pure (no
/// real `~/.ssh` dependency) so it's unit-testable.
fn key_candidates_in(ssh_dir: &Path) -> Vec<PathBuf> {
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| ssh_dir.join(name))
        .filter(|p| p.is_file())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Shared exit state: `Some(code)` once the session ends.
type ExitState = Arc<(Mutex<Option<i32>>, Condvar)>;

fn set_exit(exit: &ExitState, code: i32) {
    let (m, cv) = &**exit;
    let mut g = m.lock().unwrap();
    if g.is_none() {
        *g = Some(code);
        cv.notify_all();
    }
}

/// An SSH session presented as a [`PtyBackend`]. See the module docs for the
/// sync↔async bridge.
pub struct SshBackend {
    reader: Option<Box<dyn Read + Send>>,
    out_tx: UnboundedSender<Vec<u8>>,
    resize_tx: UnboundedSender<(u32, u32)>,
    kill_tx: UnboundedSender<()>,
    event_rx: std::sync::mpsc::Receiver<crate::PtyEvent>,
    waker: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    exit: ExitState,
}

impl SshBackend {
    /// Start the session thread. Returns immediately; connect/auth happen on the
    /// thread, so an early read simply blocks until the shell produces output (or
    /// gets EOF if the connection fails — the error is logged).
    pub fn spawn(cfg: SshConfig, size: PtySize) -> anyhow::Result<Self> {
        let (out_tx, out_rx) = unbounded_channel::<Vec<u8>>();
        let (resize_tx, resize_rx) = unbounded_channel::<(u32, u32)>();
        let (kill_tx, kill_rx) = unbounded_channel::<()>();
        let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let (event_tx, event_rx) = std::sync::mpsc::channel::<crate::PtyEvent>();
        let waker: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>> = Arc::new(Mutex::new(None));
        let exit: ExitState = Arc::new((Mutex::new(None), Condvar::new()));
        let exit_thread = exit.clone();
        let waker_clone = waker.clone();

        std::thread::Builder::new()
            .name("tn-ssh".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("ssh: tokio runtime: {e}");
                        set_exit(&exit_thread, -1);
                        return;
                    }
                };
                let exit_run = exit_thread.clone();
                rt.block_on(async move {
                    if let Err(e) =
                        run_session(cfg, size, out_rx, resize_rx, kill_rx, in_tx, event_tx, waker_clone, &exit_run).await
                    {
                        tracing::error!("ssh session: {e:#}");
                    }
                });
                set_exit(&exit_thread, -1); // ensure waiters wake even on early error
            })
            .context("spawn ssh thread")?;

        Ok(Self {
            reader: Some(Box::new(ChannelReader { rx: in_rx, buf: Vec::new(), pos: 0 })),
            out_tx,
            resize_tx,
            kill_tx,
            event_rx,
            waker,
            exit,
        })
    }
}

impl PtyBackend for SshBackend {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        self.resize_tx
            .send((size.cols as u32, size.rows as u32))
            .map_err(|_| anyhow!("ssh session closed"))
    }

    fn take_reader(&mut self) -> anyhow::Result<Box<dyn Read + Send>> {
        self.reader.take().context("ssh reader already taken")
    }

    fn writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        Ok(Box::new(SshWriter(self.out_tx.clone())))
    }

    fn killer(&self) -> anyhow::Result<Box<dyn Killer>> {
        Ok(Box::new(SshKiller(self.kill_tx.clone())))
    }

    fn wait(&mut self) -> anyhow::Result<i32> {
        let (m, cv) = &*self.exit;
        let mut g = m.lock().unwrap();
        while g.is_none() {
            g = cv.wait(g).unwrap();
        }
        Ok(g.unwrap())
    }

    fn try_wait(&mut self) -> anyhow::Result<Option<i32>> {
        Ok(*self.exit.0.lock().unwrap())
    }

    fn try_recv_event(&mut self) -> Option<crate::PtyEvent> {
        self.event_rx.try_recv().ok()
    }

    fn set_waker(&mut self, waker: Box<dyn Fn() + Send + Sync>) {
        *self.waker.lock().unwrap() = Some(waker);
    }
}

async fn run_session(
    cfg: SshConfig,
    size: PtySize,
    mut out_rx: UnboundedReceiver<Vec<u8>>,
    mut resize_rx: UnboundedReceiver<(u32, u32)>,
    mut kill_rx: UnboundedReceiver<()>,
    in_tx: StdSender<Vec<u8>>,
    event_tx: StdSender<crate::PtyEvent>,
    waker: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    exit: &ExitState,
) -> anyhow::Result<()> {
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        ..Default::default()
    });

    let mut current_size = size;

    loop {
        let handler = ClientHandler {
            host: cfg.host.clone(),
            port: cfg.port,
            in_tx: in_tx.clone(),
        };
        
        let _ = in_tx.send(format!("\r\n\x1b[36m[SSH]\x1b[0m 正在连接 {}@{}:{} ...\r\n", cfg.user, cfg.host, cfg.port).into_bytes());

        let connect_res = tokio::time::timeout(
            Duration::from_secs(15),
            client::connect(config.clone(), (cfg.host.as_str(), cfg.port), handler)
        ).await;

        let mut handle = match connect_res {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => {
                let msg = format!("\r\n\x1b[31m[SSH]\x1b[0m 连接失败: {}\r\n\x1b[33m[SSH]\x1b[0m 正在 5 秒后重试... (Ctrl+D 取消)\r\n", e);
                let _ = in_tx.send(msg.into_bytes());
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    _ = kill_rx.recv() => return Ok(()),
                }
            }
            Err(_) => {
                let msg = format!("\r\n\x1b[31m[SSH]\x1b[0m 连接超时 ({}:{}, 15s)\r\n\x1b[33m[SSH]\x1b[0m 正在 5 秒后重试... (Ctrl+D 取消)\r\n", cfg.host, cfg.port);
                let _ = in_tx.send(msg.into_bytes());
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    _ = kill_rx.recv() => return Ok(()),
                }
            }
        };

        if let Err(e) = authenticate(&mut handle, &cfg, &event_tx, &waker, &in_tx).await {
            let msg = format!("\r\n\x1b[31m[SSH]\x1b[0m 认证失败: {}\r\n", e);
            let _ = in_tx.send(msg.into_bytes());
            return Err(e);
        }

        let _ = in_tx.send("\r\n\x1b[32m[SSH]\x1b[0m 连接成功! 打开远程 shell...\r\n".as_bytes().to_vec());

        let mut channel = handle.channel_open_session().await.context("open session")?;
        channel
            .request_pty(false, "xterm-256color", current_size.cols as u32, current_size.rows as u32, 0, 0, &[])
            .await
            .context("request pty")?;
        channel.request_shell(true).await.context("request shell")?;

        let mut explicit_exit = false;

        loop {
            tokio::select! {
                out = out_rx.recv() => match out {
                    Some(bytes) => { let _ = channel.data_bytes(bytes).await; }
                    None => return Ok(()),
                },
                rz = resize_rx.recv() => {
                    if let Some((cols, rows)) = rz {
                        current_size = PtySize::new(rows as u16, cols as u16);
                        let _ = channel.window_change(cols, rows, 0, 0).await;
                    }
                },
                _ = kill_rx.recv() => return Ok(()),
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if in_tx.send(data.to_vec()).is_err() {
                            return Ok(());
                        }
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => {
                        set_exit(exit, exit_status as i32);
                        explicit_exit = true;
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                },
            }
        }

        let _ = handle.disconnect(Disconnect::ByApplication, "", "en").await;

        if explicit_exit {
            return Ok(());
        } else {
            let _ = in_tx.send("\r\n\x1b[33m[SSH]\x1b[0m 连接已断开。5 秒后自动重连... (Ctrl+D 取消)\r\n".as_bytes().to_vec());
            if event_tx.send(crate::PtyEvent::Disconnected).is_ok() {
                if let Some(w) = waker.lock().unwrap().as_ref() {
                    w();
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                _ = kill_rx.recv() => return Ok(()),
            }
        }
    }
}

/// Try key-file auth (each `~/.ssh/id_*` or the explicit key), then password.
/// TODO(M2): SSH agent (`russh::keys::agent`) before key files.
async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    cfg: &SshConfig,
    event_tx: &StdSender<crate::PtyEvent>,
    waker: &Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    in_tx: &StdSender<Vec<u8>>,
) -> anyhow::Result<()> {
    let keys = cfg.key_candidates();
    for path in &keys {
        let _ = in_tx.send(format!("\r\n\x1b[36m[SSH]\x1b[0m 尝试密钥认证 ({})...", path.display()).into_bytes());
        let key = match load_secret_key(path, None) {
            Ok(k) => k,
            Err(_) => continue,
        };
        let hash = handle.best_supported_rsa_hash().await?.flatten();
        let res = handle
            .authenticate_publickey(cfg.user.as_str(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
            .await?;
        if res.success() {
            return Ok(());
        }
    }

    if let Some(pw) = &cfg.password {
        let _ = in_tx.send("\r\n\x1b[36m[SSH]\x1b[0m 尝试配置密码认证...".as_bytes().to_vec());
        let res = handle.authenticate_password(cfg.user.as_str(), pw.as_str()).await?;
        if res.success() {
            return Ok(());
        }
    } else {
        let _ = in_tx.send("\r\n\x1b[36m[SSH]\x1b[0m 请求输入密码...".as_bytes().to_vec());
        let (tx, rx) = std::sync::mpsc::channel();
        if event_tx.send(crate::PtyEvent::NeedPassword {
            prompt: format!("Password for {}@{}:", cfg.user, cfg.host),
            reply: tx,
        }).is_ok() {
            if let Some(w) = waker.lock().unwrap().as_ref() {
                w();
            }
            if let Ok(Some(pw)) = tokio::task::spawn_blocking(move || rx.recv().ok()).await {
                let res = handle.authenticate_password(cfg.user.as_str(), pw).await?;
                if res.success() {
                    return Ok(());
                }
            }
        }
    }

    Err(anyhow!(
        "ssh authentication failed for {}@{}",
        cfg.user,
        cfg.host,
    ))
}

/// russh client event handler. Verifies the host key against `~/.ssh/known_hosts`.
struct ClientHandler {
    host: String,
    port: u16,
    in_tx: StdSender<Vec<u8>>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let known_hosts_path = home_dir()
            .map(|h| h.join(".ssh").join("known_hosts"))
            .unwrap_or_else(|| PathBuf::from("known_hosts"));

        if !known_hosts_path.exists() {
            tracing::info!("SSH: known_hosts not found, accepting key (TOFU)");
            append_known_host(&known_hosts_path, &self.host, self.port, key);
            return Ok(true);
        }

        match russh::keys::check_known_hosts_path(&self.host, self.port, key, &known_hosts_path) {
            Ok(true) => Ok(true),
            Ok(false) => {
                tracing::info!("SSH: Host {}:{} not in known_hosts, accepting (TOFU)", self.host, self.port);
                append_known_host(&known_hosts_path, &self.host, self.port, key);
                Ok(true)
            }
            Err(_) => {
                let warning = format!(
                    "\r\n\x1b[31;1m@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\r\n\
                     @ 警告: 远程主机标识已更改!     @\r\n\
                     @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\x1b[0m\r\n\
                     可能存在中间人攻击或主机已重装。\r\n\
                     主机: {}:{}\r\n\
                     请手动验证或删除 ~/.ssh/known_hosts 中对应条目。\r\n\
                     连接已中止。\r\n",
                    self.host, self.port
                );
                let _ = self.in_tx.send(warning.into_bytes());
                tracing::warn!("SSH: HOST KEY MISMATCH for {}:{}!", self.host, self.port);
                Ok(false)
            }
        }
    }
}

fn append_known_host(path: &Path, host: &str, port: u16, key: &ssh_key::PublicKey) {
    if let Ok(key_str) = key.to_openssh() {
        let entry = if port == 22 {
            format!("{} {}\n", host, key_str)
        } else {
            format!("[{}]:{} {}\n", host, port, key_str)
        };
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(entry.as_bytes());
        }
    }
}

/// Sync `Read` end of the remote→local pipe, fed by the session loop's
/// `std::mpsc` sender. `recv` blocks until data; a dropped sender = EOF.
struct ChannelReader {
    rx: StdReceiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(chunk) if chunk.is_empty() => continue,
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0), // session ended
            }
        }
        let n = (self.buf.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Sync `Write` end: pushes input bytes onto the session loop's channel.
struct SshWriter(UnboundedSender<Vec<u8>>);

impl Write for SshWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .send(buf.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ssh session closed"))?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct SshKiller(UnboundedSender<()>);

impl Killer for SshKiller {
    fn kill(&mut self) -> anyhow::Result<()> {
        let _ = self.0.send(());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_only_defaults_port_22() {
        let c = SshConfig::parse("example.com", Some("alice"));
        assert_eq!(c.host, "example.com");
        assert_eq!(c.port, 22);
        assert_eq!(c.user, "alice");
    }

    #[test]
    fn parse_host_with_port() {
        let c = SshConfig::parse("10.0.0.5:2222", Some("bob"));
        assert_eq!(c.host, "10.0.0.5");
        assert_eq!(c.port, 2222);
    }

    #[test]
    fn parse_non_numeric_suffix_is_part_of_host() {
        // A trailing non-port (e.g. IPv6-ish / typo) stays in the host.
        let c = SshConfig::parse("host:notaport", Some("u"));
        assert_eq!(c.host, "host:notaport");
        assert_eq!(c.port, 22);
    }

    #[test]
    fn parse_user_at_host_with_port() {
        let c = SshConfig::parse("admin@192.168.1.1:2222", Some("ignored"));
        assert_eq!(c.user, "admin");
        assert_eq!(c.host, "192.168.1.1");
        assert_eq!(c.port, 2222);
    }

    #[test]
    fn key_candidates_only_existing_files() {
        let dir = std::env::temp_dir().join(format!("tn-ssh-keytest-{}", std::process::id()));
        let ssh = dir.join(".ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::write(ssh.join("id_ed25519"), b"x").unwrap();
        std::fs::write(ssh.join("id_rsa"), b"x").unwrap();
        // id_ecdsa intentionally absent.
        let found = key_candidates_in(&ssh);
        assert_eq!(
            found,
            vec![ssh.join("id_ed25519"), ssh.join("id_rsa")], // preference order, ecdsa skipped
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_key_path_wins() {
        let mut c = SshConfig::new("h", "u");
        c.key_path = Some(PathBuf::from("/custom/key"));
        assert_eq!(c.key_candidates(), vec![PathBuf::from("/custom/key")]);
    }
}
