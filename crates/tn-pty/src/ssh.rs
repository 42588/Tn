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
//! > **Status:** compiles + unit-tested (headless config/parsing). **Verified
//! > end-to-end on a real host (2026-06-05): publickey auth (`id_ed25519`) +
//! > interactive shell against AlmaLinux 9.7 sshd over WSL, through Tn itself.**
//! > Still *not* exercised live: the password-*prompt* UI path, the
//! > `keyboard-interactive` method (test server didn't offer it), reconnect, and
//! > TOFU first-connect (the host was already in `known_hosts`). Auth chain: probe
//! > offered methods (`none`) → key-file → password (tried via *both* the
//! > `password` method and `keyboard-interactive`, since many PAM/Linux servers
//! > only accept the latter); `check_server_key` verifies against
//! > `~/.ssh/known_hosts` with TOFU on first connect. **ssh-agent is TODO** (see
//! > `authenticate` TODO comment).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

    /// Parse a `[user@]host[:port]` target, merging with `~/.ssh/config`. Explicit
    /// target pieces win: `root@alias:2222` keeps that user/port while still using
    /// `HostName` / `IdentityFile` from the alias. `user` falls back to the ssh
    /// config, then `$USERNAME` / `$USER`, else `"root"`.
    pub fn parse(target: &str, user: Option<&str>) -> Self {
        let ssh_config = home_dir()
            .and_then(|home| std::fs::read_to_string(home.join(".ssh").join("config")).ok());
        Self::parse_with_ssh_config(target, user, ssh_config.as_deref())
    }

    fn parse_with_ssh_config(target: &str, user: Option<&str>, ssh_config: Option<&str>) -> Self {
        let (target_no_user, inline_user) = match target.split_once('@') {
            Some((u, rest)) if !u.is_empty() => (rest, Some(u)),
            _ => (target, None),
        };

        let (mut host, mut port, explicit_port) = match target_no_user.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => {
                (h.to_string(), p.parse().unwrap(), true)
            }
            _ => (target_no_user.to_string(), 22, false),
        };

        let explicit_user = inline_user.is_some() || user.is_some();
        let mut user = inline_user.or(user).map(str::to_string);
        let mut key_path = None;

        if let Some(content) = ssh_config {
            let mut reader = std::io::BufReader::new(content.as_bytes());
            if let Ok(ssh_cfg) = ssh2_config::SshConfig::default()
                .parse(&mut reader, ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS)
            {
                let params = ssh_cfg.query(&host);
                if let Some(cfg_host) = params.host_name {
                    host = cfg_host;
                }
                if !explicit_port {
                    if let Some(cfg_port) = params.port {
                        port = cfg_port;
                    }
                }
                if !explicit_user {
                    if let Some(cfg_user) = params.user {
                        user = Some(cfg_user);
                    }
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

        let user = user
            .or_else(|| std::env::var("USERNAME").ok())
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "root".to_string());

        Self {
            host,
            port,
            user,
            key_path,
            password: None,
        }
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

/// A `Host` alias resolved from `~/.ssh/config` — surfaced as the connector's
/// third section (A4). `host`/`user`/`port` come from the alias's `HostName`/
/// `User`/`Port` directives (falling back to the alias itself / defaults).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshHostEntry {
    pub alias: String,
    pub host: String,
    pub user: Option<String>,
    pub port: u16,
}

/// Enumerate concrete `Host` aliases from `~/.ssh/config` (A4), each resolved
/// via the same parser `SshConfig::parse` uses. Wildcard patterns (`*`/`?`) and
/// negations (`!`) are skipped — they're rules, not endpoints. Returns empty if
/// there's no config file.
pub fn list_ssh_config_hosts() -> Vec<SshHostEntry> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let path = home.join(".ssh").join("config");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let aliases = parse_host_aliases(&content);
    if aliases.is_empty() {
        return Vec::new();
    }
    // Parse once; resolve each alias's effective HostName/User/Port via query().
    let parsed = {
        let mut reader = std::io::BufReader::new(content.as_bytes());
        ssh2_config::SshConfig::default()
            .parse(&mut reader, ssh2_config::ParseRule::ALLOW_UNKNOWN_FIELDS)
            .ok()
    };
    aliases
        .into_iter()
        .map(|alias| {
            let mut host = alias.clone();
            let mut user = None;
            let mut port = 22;
            if let Some(cfg) = &parsed {
                let params = cfg.query(&alias);
                if let Some(h) = params.host_name {
                    host = h;
                }
                if let Some(u) = params.user {
                    user = Some(u);
                }
                if let Some(p) = params.port {
                    port = p;
                }
            }
            SshHostEntry {
                alias,
                host,
                user,
                port,
            }
        })
        .collect()
}

/// Pure scan of ssh_config text for `Host` directive aliases, in file order,
/// deduped. Skips comments, wildcard patterns (`*`/`?`) and negations (`!`);
/// one `Host` line may declare several patterns. Unit-testable (no real `~/.ssh`).
fn parse_host_aliases(content: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let kw = parts.next().unwrap_or("");
        if !kw.eq_ignore_ascii_case("Host") {
            continue;
        }
        let Some(rest) = parts.next() else { continue };
        for pat in rest.split_whitespace() {
            if pat.contains('*') || pat.contains('?') || pat.starts_with('!') {
                continue;
            }
            if !out.iter().any(|a| a == pat) {
                out.push(pat.to_string());
            }
        }
    }
    out
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
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("ssh: tokio runtime: {e}");
                        set_exit(&exit_thread, -1);
                        return;
                    }
                };
                let exit_run = exit_thread.clone();
                rt.block_on(async move {
                    if let Err(e) = run_session(
                        cfg,
                        size,
                        out_rx,
                        resize_rx,
                        kill_rx,
                        in_tx,
                        event_tx,
                        waker_clone,
                        &exit_run,
                    )
                    .await
                    {
                        tracing::error!("ssh session: {e:#}");
                    }
                });
                set_exit(&exit_thread, -1); // ensure waiters wake even on early error
            })
            .context("spawn ssh thread")?;

        Ok(Self {
            reader: Some(Box::new(ChannelReader {
                rx: in_rx,
                buf: Vec::new(),
                pos: 0,
            })),
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
        let key_rejected = Arc::new(AtomicBool::new(false));
        let handler = ClientHandler {
            host: cfg.host.clone(),
            port: cfg.port,
            in_tx: in_tx.clone(),
            event_tx: event_tx.clone(),
            waker: waker.clone(),
            key_rejected: key_rejected.clone(),
        };

        let _ = in_tx.send(
            format!(
                "\r\n\x1b[36m[SSH]\x1b[0m 正在连接 {}@{}:{} ...\r\n",
                cfg.user, cfg.host, cfg.port
            )
            .into_bytes(),
        );
        emit_event(
            &event_tx,
            &waker,
            crate::PtyEvent::SshProgress {
                phase: crate::SshPhase::Connecting,
                detail: format!("{}:{}", cfg.host, cfg.port),
            },
        );

        let connect_res = tokio::time::timeout(
            Duration::from_secs(15),
            client::connect(config.clone(), (cfg.host.as_str(), cfg.port), handler),
        )
        .await;

        let mut handle = match connect_res {
            Ok(Ok(h)) => h,
            Ok(Err(e)) => {
                // If we deliberately rejected the host key, do NOT retry — the
                // warning was already printed by check_server_key.
                if key_rejected.load(Ordering::Relaxed) {
                    // check_server_key already emitted the precise event (mismatch
                    // danger card, or a quiet TOFU rejection) — just stop.
                    return Err(anyhow!("host key rejected"));
                }
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

        let auth_method = match authenticate(&mut handle, &cfg, &event_tx, &waker, &in_tx).await {
            Ok(m) => m,
            Err(e) => {
                let msg = format!("\r\n\x1b[31m[SSH]\x1b[0m 认证失败: {}\r\n", e);
                let _ = in_tx.send(msg.into_bytes());
                return Err(e);
            }
        };

        let _ = in_tx.send(
            "\r\n\x1b[32m[SSH]\x1b[0m 连接成功! 打开远程 shell...\r\n"
                .as_bytes()
                .to_vec(),
        );
        emit_event(
            &event_tx,
            &waker,
            crate::PtyEvent::SshProgress {
                phase: crate::SshPhase::OpeningShell,
                detail: String::new(),
            },
        );

        let mut channel = handle
            .channel_open_session()
            .await
            .context("open session")?;
        channel
            .request_pty(
                false,
                "xterm-256color",
                current_size.cols as u32,
                current_size.rows as u32,
                0,
                0,
                &[],
            )
            .await
            .context("request pty")?;
        channel.request_shell(true).await.context("request shell")?;
        // Keystrokes typed while the progress/password/host-key overlays were up
        // must not replay into the remote shell after connect/reconnect. The UI
        // swallows them now, but drain defensively for bytes queued before that fix
        // or from non-keyboard paths.
        drain_pending_input(&mut out_rx);

        // Connected — let the UI record this target as a recent connection (A1),
        // tagged with the method that actually worked.
        if event_tx
            .send(crate::PtyEvent::Connected {
                method: auth_method,
            })
            .is_ok()
        {
            if let Some(w) = waker.lock().unwrap().as_ref() {
                w();
            }
        }

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
            let _ = in_tx.send(
                "\r\n\x1b[33m[SSH]\x1b[0m 连接已断开。5 秒后自动重连... (Ctrl+D 取消)\r\n"
                    .as_bytes()
                    .to_vec(),
            );
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

/// Authenticate the session, mirroring what OpenSSH's client effectively does:
/// probe the offered methods (`none`) → public-key → password.
///
/// **The password is tried via *both* the `password` method and
/// `keyboard-interactive`** with the same secret. Many Linux/PAM servers (incl. a
/// stock WSL sshd, esp. for `root`) only accept `keyboard-interactive`; sending
/// the `password` method alone fails even with the *correct* password — the usual
/// cause of "密码正确却认证失败" (OpenSSH masks the difference by auto-falling back).
/// TODO(M2): SSH agent (`russh::keys::agent`) before key files.
async fn authenticate(
    handle: &mut Handle<ClientHandler>,
    cfg: &SshConfig,
    event_tx: &StdSender<crate::PtyEvent>,
    waker: &Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    in_tx: &StdSender<Vec<u8>>,
) -> anyhow::Result<crate::AuthKind> {
    // Probe offered methods via `none` (like `ssh -v`'s "Authentications that can
    // continue"). The failure reply carries the list — invaluable for diagnosing
    // blind. An empty list (probe errored) ⇒ assume every method is on offer and
    // just try them all.
    let mut methods: Vec<russh::MethodKind> = Vec::new();
    match handle.authenticate_none(cfg.user.as_str()).await {
        // Passwordless `none` accepted (very rare) — no secret given, badge as key.
        Ok(client::AuthResult::Success) => return Ok(crate::AuthKind::PublicKey),
        Ok(client::AuthResult::Failure {
            remaining_methods, ..
        }) => {
            methods = remaining_methods.iter().copied().collect();
            let _ = in_tx.send(
                format!(
                    "\r\n\x1b[36m[SSH]\x1b[0m 服务器支持的认证方式: {}\r\n",
                    methods_str(&methods)
                )
                .into_bytes(),
            );
        }
        Err(e) => tracing::debug!("ssh: `none` auth probe failed: {e}"),
    }

    // Connection progress: now authenticating (B1 card).
    emit_event(
        event_tx,
        waker,
        crate::PtyEvent::SshProgress {
            phase: crate::SshPhase::Authenticating,
            detail: String::new(),
        },
    );

    // 1) Public-key (explicit key or ~/.ssh/id_*).
    if server_offers(&methods, russh::MethodKind::PublicKey) {
        for path in &cfg.key_candidates() {
            let _ = in_tx.send(
                format!(
                    "\r\n\x1b[36m[SSH]\x1b[0m 尝试密钥认证 ({})...",
                    path.display()
                )
                .into_bytes(),
            );
            let key = match load_secret_key(path, None) {
                Ok(k) => k,
                Err(_) => continue,
            };
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            let res = handle
                .authenticate_publickey(
                    cfg.user.as_str(),
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await?;
            if res.success() {
                return Ok(crate::AuthKind::PublicKey);
            }
        }
    }

    // 2) Password — up to 3 interactive attempts on the SAME connection (B3:
    //    in-place retry, no teardown). A config/remembered password is tried once
    //    (re-prompting can't change it). Each attempt tries BOTH the `password`
    //    method and keyboard-interactive. Skip entirely if the server offers no
    //    password-style method (don't ask for a secret it would never accept).
    let wants_pw = server_offers(&methods, russh::MethodKind::Password)
        || server_offers(&methods, russh::MethodKind::KeyboardInteractive);
    if wants_pw {
        let mut last_error: Option<String> = None;
        for attempt in 1..=3u32 {
            let password = match &cfg.password {
                Some(pw) => Some(pw.clone()),
                None => prompt_password(cfg, event_tx, waker, in_tx, last_error.take()).await,
            };
            let Some(pw) = password.filter(|p| !p.is_empty()) else {
                break; // user cancelled / empty input
            };
            if server_offers(&methods, russh::MethodKind::Password) {
                let _ = in_tx.send(
                    "\r\n\x1b[36m[SSH]\x1b[0m 尝试密码认证 (password)..."
                        .as_bytes()
                        .to_vec(),
                );
                if handle
                    .authenticate_password(cfg.user.as_str(), pw.as_str())
                    .await?
                    .success()
                {
                    return Ok(crate::AuthKind::Password);
                }
            }
            if server_offers(&methods, russh::MethodKind::KeyboardInteractive) {
                let _ = in_tx.send(
                    "\r\n\x1b[36m[SSH]\x1b[0m 尝试键盘交互认证 (keyboard-interactive)..."
                        .as_bytes()
                        .to_vec(),
                );
                if try_keyboard_interactive(handle, cfg.user.as_str(), &pw).await? {
                    return Ok(crate::AuthKind::KeyboardInteractive);
                }
            }
            // This attempt failed. A config/remembered password won't change by
            // re-asking — fall through to the error card. An interactively-typed
            // one gets re-prompted (with a counter) until the 3-try cap.
            if cfg.password.is_some() || attempt == 3 {
                break;
            }
            last_error = Some(format!("密码错误,请重试(第 {} 次,共 3 次)", attempt + 1));
        }
    }

    // If the server never offered a password-style method, the secret the user
    // typed was never going to be accepted — point at the server config instead
    // of leaving them puzzled over a "correct" password.
    let no_pw_method = !methods.contains(&russh::MethodKind::Password)
        && !methods.contains(&russh::MethodKind::KeyboardInteractive);
    let hint = if !methods.is_empty() && no_pw_method {
        format!(
            " (服务器未开放密码登录, 仅支持 {}; 请检查 sshd_config 的 PasswordAuthentication / PermitRootLogin)",
            methods_str(&methods)
        )
    } else {
        String::new()
    };
    // Actionable error card (C1): reason + the methods the server advertised.
    emit_event(
        event_tx,
        waker,
        crate::PtyEvent::SshFailed {
            kind: crate::SshErrorKind::Auth,
            detail: hint.trim().to_string(),
            offered: methods_str(&methods),
        },
    );
    Err(anyhow!(
        "ssh authentication failed for {}@{}{}",
        cfg.user,
        cfg.host,
        hint
    ))
}

/// Send a UI [`crate::PtyEvent`] + wake the foreground so the card repaints
/// promptly (same pattern as `Connected`/`NeedPassword`).
fn emit_event(
    event_tx: &StdSender<crate::PtyEvent>,
    waker: &Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    ev: crate::PtyEvent,
) {
    if event_tx.send(ev).is_ok() {
        if let Some(w) = waker.lock().unwrap().as_ref() {
            w();
        }
    }
}

/// Whether the server offered method `m` — or the offered set is unknown (probe
/// failed), in which case we optimistically assume it might and try anyway.
fn server_offers(methods: &[russh::MethodKind], m: russh::MethodKind) -> bool {
    methods.is_empty() || methods.contains(&m)
}

/// Human-readable method list (e.g. `publickey, keyboard-interactive`).
fn methods_str(methods: &[russh::MethodKind]) -> String {
    if methods.is_empty() {
        return "(未知)".to_string();
    }
    methods
        .iter()
        .map(String::from)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Prompt the UI for a password and block (off-runtime) on the reply. `error`
/// carries a previous-attempt message for the card's red line (B3 retry).
async fn prompt_password(
    cfg: &SshConfig,
    event_tx: &StdSender<crate::PtyEvent>,
    waker: &Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    in_tx: &StdSender<Vec<u8>>,
    error: Option<String>,
) -> Option<String> {
    let _ = in_tx.send(
        "\r\n\x1b[36m[SSH]\x1b[0m 请求输入密码..."
            .as_bytes()
            .to_vec(),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    if event_tx
        .send(crate::PtyEvent::NeedPassword {
            prompt: format!("{}@{}:{}", cfg.user, cfg.host, cfg.port),
            error,
            reply: tx,
        })
        .is_err()
    {
        return None;
    }
    if let Some(w) = waker.lock().unwrap().as_ref() {
        w();
    }
    tokio::task::spawn_blocking(move || rx.recv().ok())
        .await
        .ok()
        .flatten()
}

/// Drive the `keyboard-interactive` exchange, answering every server prompt with
/// `password` (the PAM password-prompt case; an empty prompt list ⇒ no answers).
/// `Ok(true)` on success. Capped to avoid a misbehaving server looping
/// `InfoRequest`s forever.
async fn try_keyboard_interactive(
    handle: &mut Handle<ClientHandler>,
    user: &str,
    password: &str,
) -> anyhow::Result<bool> {
    use client::KeyboardInteractiveAuthResponse as Kir;
    let mut resp = handle
        .authenticate_keyboard_interactive_start(user.to_string(), None)
        .await?;
    for _ in 0..16 {
        match resp {
            Kir::Success => return Ok(true),
            Kir::Failure { .. } => return Ok(false),
            Kir::InfoRequest { prompts, .. } => {
                let answers = vec![password.to_string(); prompts.len()];
                resp = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    Ok(false)
}

/// russh client event handler. Verifies the host key against `~/.ssh/known_hosts`.
struct ClientHandler {
    host: String,
    port: u16,
    in_tx: StdSender<Vec<u8>>,
    /// For the B2 TOFU trust panel + host-key-mismatch event.
    event_tx: StdSender<crate::PtyEvent>,
    waker: Arc<Mutex<Option<Box<dyn Fn() + Send + Sync>>>>,
    /// Set to `true` when we deliberately reject a mismatched / untrusted host
    /// key, so the outer retry loop can distinguish "network error → retry" from
    /// "key rejected → abort".
    key_rejected: Arc<AtomicBool>,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let known_hosts_path = home_dir()
            .map(|h| h.join(".ssh").join("known_hosts"))
            .unwrap_or_else(|| PathBuf::from("known_hosts"));
        let fingerprint = key.fingerprint(ssh_key::HashAlg::Sha256).to_string();

        // Known + matching → accept silently. Mismatch → danger card + abort.
        // Unknown (no file / not listed) → fall through to the TOFU trust panel.
        if known_hosts_path.exists() {
            match russh::keys::check_known_hosts_path(&self.host, self.port, key, &known_hosts_path)
            {
                Ok(true) => return Ok(true),
                Ok(false) => {} // unknown → TOFU below
                Err(_) => {
                    tracing::warn!("SSH: HOST KEY MISMATCH for {}:{}!", self.host, self.port);
                    let _ = self.in_tx.send(
                        format!(
                            "\r\n\x1b[31;1m[SSH] 警告: 远程主机标识已更改!\x1b[0m 连接已中止({}:{})。\r\n",
                            self.host, self.port
                        )
                        .into_bytes(),
                    );
                    // B2 danger card: host key changed (possible MITM). Safe default
                    // = abort; surface the new fingerprint.
                    emit_event(
                        &self.event_tx,
                        &self.waker,
                        crate::PtyEvent::SshFailed {
                            kind: crate::SshErrorKind::HostKeyMismatch,
                            detail: fingerprint,
                            offered: String::new(),
                        },
                    );
                    self.key_rejected.store(true, Ordering::Relaxed);
                    return Ok(false);
                }
            }
        }

        // First contact with an unrecognized host → ask the user (B2 TOFU).
        tracing::info!(
            "SSH: unknown host {}:{}, asking to trust (TOFU)",
            self.host,
            self.port
        );
        match self.confirm_host_key(&fingerprint).await {
            crate::HostKeyVerdict::AcceptAndSave => {
                match append_known_host(&known_hosts_path, &self.host, self.port, key) {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        let _ = self.in_tx.send(
                            format!(
                                "\r\n\x1b[31m[SSH]\x1b[0m 无法写入 known_hosts({}): {e}\r\n\
                                 \x1b[33m[SSH]\x1b[0m 未保存主机信任,连接已中止。可取消「记住此主机」后仅信任本次。\r\n",
                                known_hosts_path.display()
                            )
                            .into_bytes(),
                        );
                        self.key_rejected.store(true, Ordering::Relaxed);
                        Ok(false)
                    }
                }
            }
            crate::HostKeyVerdict::AcceptOnce => Ok(true),
            crate::HostKeyVerdict::Reject => {
                let _ = self.in_tx.send(
                    "\r\n\x1b[33m[SSH]\x1b[0m 已取消:未信任主机指纹。\r\n"
                        .as_bytes()
                        .to_vec(),
                );
                self.key_rejected.store(true, Ordering::Relaxed);
                Ok(false)
            }
        }
    }
}

impl ClientHandler {
    /// Ask the UI to trust an unrecognized host key (B2 TOFU) and block (off the
    /// runtime) on the reply. A dropped channel ⇒ reject.
    async fn confirm_host_key(&self, fingerprint: &str) -> crate::HostKeyVerdict {
        let (tx, rx) = std::sync::mpsc::channel();
        if self
            .event_tx
            .send(crate::PtyEvent::NeedHostKeyConfirm {
                host: format!("{}:{}", self.host, self.port),
                fingerprint: fingerprint.to_string(),
                reply: tx,
            })
            .is_err()
        {
            return crate::HostKeyVerdict::Reject;
        }
        if let Some(w) = self.waker.lock().unwrap().as_ref() {
            w();
        }
        tokio::task::spawn_blocking(move || rx.recv().ok())
            .await
            .ok()
            .flatten()
            .unwrap_or(crate::HostKeyVerdict::Reject)
    }
}

fn append_known_host(
    path: &Path,
    host: &str,
    port: u16,
    key: &ssh_key::PublicKey,
) -> anyhow::Result<()> {
    let key_str = key.to_openssh().context("encode public key")?;
    let entry = if port == 22 {
        format!("{} {}\n", host, key_str)
    } else {
        format!("[{}]:{} {}\n", host, port, key_str)
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(entry.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn drain_pending_input(out_rx: &mut UnboundedReceiver<Vec<u8>>) {
    while out_rx.try_recv().is_ok() {}
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

    fn assert_key_path_ends_with(key_path: Option<&PathBuf>, suffix: &str) {
        let normalized = key_path
            .expect("key path")
            .to_string_lossy()
            .replace('\\', "/");
        assert!(
            normalized.ends_with(suffix),
            "{normalized:?} should end with {suffix:?}"
        );
    }

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
    fn parse_ssh_config_alias_fills_missing_pieces() {
        let cfg = "\
Host alma
  HostName 10.0.0.5
  User deploy
  Port 2200
  IdentityFile ~/.ssh/alma_ed25519
";
        let c = SshConfig::parse_with_ssh_config("alma", None, Some(cfg));
        assert_eq!(c.host, "10.0.0.5");
        assert_eq!(c.port, 2200);
        assert_eq!(c.user, "deploy");
        assert_key_path_ends_with(c.key_path.as_ref(), "/.ssh/alma_ed25519");
    }

    #[test]
    fn parse_explicit_user_and_port_override_ssh_config_alias() {
        let cfg = "\
Host alma
  HostName 10.0.0.5
  User deploy
  Port 2200
  IdentityFile ~/.ssh/alma_ed25519
";
        let c = SshConfig::parse_with_ssh_config("root@alma:2222", Some("ignored"), Some(cfg));
        assert_eq!(c.host, "10.0.0.5", "alias HostName still resolves");
        assert_eq!(c.port, 2222, "typed port beats ssh_config Port");
        assert_eq!(
            c.user, "root",
            "inline user beats explicit arg and ssh_config User"
        );
        assert_key_path_ends_with(c.key_path.as_ref(), "/.ssh/alma_ed25519");
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

    #[test]
    fn host_aliases_skip_wildcards_and_dedupe() {
        let cfg = "\
# comment
Host *
  ForwardAgent yes

Host alma  bastion
  HostName 10.0.0.5

Host web-* db?
  User deploy

Host !secret prod
  HostName prod.example.com

Host alma
  Port 2222
";
        let aliases = parse_host_aliases(cfg);
        // `*`, `web-*`, `db?`, `!secret` are patterns/negations → skipped; the
        // second `Host alma` block doesn't duplicate the alias.
        assert_eq!(aliases, vec!["alma", "bastion", "prod"]);
    }

    #[test]
    fn host_aliases_empty_when_no_host_lines() {
        assert!(parse_host_aliases("# just a comment\nForwardAgent yes\n").is_empty());
    }

    #[test]
    fn drain_pending_input_clears_buffered_keystrokes() {
        let (tx, mut rx) = unbounded_channel();
        tx.send(b"queued-before-connect".to_vec()).unwrap();
        tx.send(b"\r".to_vec()).unwrap();
        drain_pending_input(&mut rx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn append_known_host_creates_parent_and_writes_port_format() {
        let dir = std::env::temp_dir().join(format!("tn-known-hosts-{}", std::process::id()));
        let path = dir.join(".ssh").join("known_hosts");
        let key = russh::keys::parse_public_key_base64(
            "AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ",
        )
        .unwrap();
        append_known_host(&path, "example.com", 2222, &key).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("[example.com]:2222 ssh-ed25519 "));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
