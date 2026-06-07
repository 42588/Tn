//! Remote filesystem access for SSH panes.
//!
//! This intentionally lives in `tn-pty`: it shares SSH connection/authentication
//! code with the PTY backend but exposes a filesystem-shaped API to the UI. The
//! first implementation is a minimal SFTP v3 client, covering directory listing
//! and bounded file reads for Explorer + Quick Look.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};
use russh::client;
use russh::ChannelMsg;

use crate::ssh::{authenticate_for_remote_fs, ClientHandler};
use crate::SshConfig;

const SFTP_VERSION: u32 = 3;
const SSH_FXP_INIT: u8 = 1;
const SSH_FXP_VERSION: u8 = 2;
const SSH_FXP_OPEN: u8 = 3;
const SSH_FXP_CLOSE: u8 = 4;
const SSH_FXP_READ: u8 = 5;
const SSH_FXP_WRITE: u8 = 6;
const SSH_FXP_OPENDIR: u8 = 11;
const SSH_FXP_READDIR: u8 = 12;
const SSH_FXP_STAT: u8 = 17;
const SSH_FXP_HANDLE: u8 = 102;
const SSH_FXP_NAME: u8 = 104;
const SSH_FXP_ATTRS: u8 = 105;
const SSH_FXP_STATUS: u8 = 101;

const SSH_FXF_READ: u32 = 0x0000_0001;
const SSH_FXF_WRITE: u32 = 0x0000_0002;
const SSH_FXF_CREAT: u32 = 0x0000_0008;
const SSH_FXF_TRUNC: u32 = 0x0000_0010;
const SSH_FX_OK: u32 = 0;
const SSH_FX_EOF: u32 = 1;

const SSH_FILEXFER_ATTR_SIZE: u32 = 0x0000_0001;
const SSH_FILEXFER_ATTR_PERMISSIONS: u32 = 0x0000_0004;
const SSH_FILEXFER_ATTR_ACMODTIME: u32 = 0x0000_0008;
const SSH_FILEXFER_ATTR_EXTENDED: u32 = 0x8000_0000;
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;

/// How much Quick Look may read from one remote file in this first pass.
pub const REMOTE_READ_LIMIT: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RemotePath(String);

impl RemotePath {
    pub fn new(path: impl Into<String>) -> Self {
        Self(normalize_remote_path(&path.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn join(&self, name: &str) -> Self {
        let name = name.trim_matches('/');
        if name.is_empty() {
            return self.clone();
        }
        if self.0 == "/" {
            Self(format!("/{name}"))
        } else {
            Self(format!("{}/{}", self.0.trim_end_matches('/'), name))
        }
    }

    pub fn parent(&self) -> Option<Self> {
        if self.0 == "/" {
            return None;
        }
        let trimmed = self.0.trim_end_matches('/');
        let Some(idx) = trimmed.rfind('/') else {
            return Some(Self("/".to_string()));
        };
        if idx == 0 {
            Some(Self("/".to_string()))
        } else {
            Some(Self(trimmed[..idx].to_string()))
        }
    }

    pub fn name(&self) -> String {
        if self.0 == "/" {
            "/".to_string()
        } else {
            self.0
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(&self.0)
                .to_string()
        }
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RemoteId {
    pub user: String,
    pub host: String,
    pub port: u16,
    pub path: RemotePath,
}

impl RemoteId {
    pub fn new(cfg: &SshConfig, path: impl Into<String>) -> Self {
        Self {
            user: cfg.user.clone(),
            host: cfg.host.clone(),
            port: cfg.port,
            path: RemotePath::new(path),
        }
    }

    pub fn child(&self, name: &str) -> Self {
        Self {
            path: self.path.join(name),
            ..self.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteDirEntry {
    pub id: RemoteId,
    pub name: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub mtime: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFileStat {
    pub is_dir: bool,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub mtime: Option<u64>,
}

pub trait RemoteFileService: Send + Sync {
    fn list_dir(&self, cfg: &SshConfig, path: &RemotePath) -> anyhow::Result<Vec<RemoteDirEntry>>;
    fn read_file(
        &self,
        cfg: &SshConfig,
        path: &RemotePath,
        max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>>;
    fn stat_file(&self, _cfg: &SshConfig, _path: &RemotePath) -> anyhow::Result<RemoteFileStat> {
        bail!("remote stat unsupported")
    }
    fn write_file(
        &self,
        _cfg: &SshConfig,
        _path: &RemotePath,
        _bytes: &[u8],
    ) -> anyhow::Result<RemoteFileStat> {
        bail!("remote write unsupported")
    }
}

#[derive(Default)]
pub struct SftpFileService;

impl SftpFileService {
    pub fn shared() -> Arc<dyn RemoteFileService> {
        Arc::new(Self)
    }
}

impl RemoteFileService for SftpFileService {
    fn list_dir(&self, cfg: &SshConfig, path: &RemotePath) -> anyhow::Result<Vec<RemoteDirEntry>> {
        let cfg = cfg.clone();
        let path = path.clone();
        run_sftp_future(move || async move {
            let mut client = SftpClient::connect(cfg.clone()).await?;
            let names = client.readdir(path.as_str()).await;
            let _ = client.close().await;
            let names = names?;
            let mut out = Vec::new();
            for name in names {
                if name.filename == "." || name.filename == ".." {
                    continue;
                }
                out.push(RemoteDirEntry {
                    id: RemoteId::new(&cfg, path.join(&name.filename).as_str()),
                    name: name.filename,
                    is_dir: name.attrs.is_dir(),
                    size: name.attrs.size,
                    permissions: name.attrs.permissions,
                    mtime: name.attrs.mtime,
                });
            }
            out.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then_with(|| {
                    a.name
                        .to_ascii_lowercase()
                        .cmp(&b.name.to_ascii_lowercase())
                })
            });
            Ok(out)
        })
    }

    fn read_file(
        &self,
        cfg: &SshConfig,
        path: &RemotePath,
        max_bytes: u64,
    ) -> anyhow::Result<Vec<u8>> {
        let cfg = cfg.clone();
        let path = path.clone();
        run_sftp_future(move || async move {
            let mut client = SftpClient::connect(cfg).await?;
            let data = client
                .read_file(path.as_str(), max_bytes.min(REMOTE_READ_LIMIT))
                .await;
            let _ = client.close().await;
            data
        })
    }

    fn stat_file(&self, cfg: &SshConfig, path: &RemotePath) -> anyhow::Result<RemoteFileStat> {
        let cfg = cfg.clone();
        let path = path.clone();
        run_sftp_future(move || async move {
            let mut client = SftpClient::connect(cfg).await?;
            let stat = client.stat(path.as_str()).await;
            let _ = client.close().await;
            stat
        })
    }

    fn write_file(
        &self,
        cfg: &SshConfig,
        path: &RemotePath,
        bytes: &[u8],
    ) -> anyhow::Result<RemoteFileStat> {
        let cfg = cfg.clone();
        let path = path.clone();
        let bytes = bytes.to_vec();
        run_sftp_future(move || async move {
            let mut client = SftpClient::connect(cfg).await?;
            let stat = client.write_file(path.as_str(), &bytes).await;
            let _ = client.close().await;
            stat
        })
    }
}

fn run_sftp_future<T, Fut>(f: impl FnOnce() -> Fut + Send + 'static) -> anyhow::Result<T>
where
    T: Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let thread = std::thread::Builder::new()
        .name("tn-sftp".into())
        .spawn(move || -> anyhow::Result<T> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("create sftp runtime")?;
            rt.block_on(f())
        })
        .context("spawn sftp thread")?;
    thread.join().map_err(|_| anyhow!("sftp thread panicked"))?
}

fn normalize_remote_path(raw: &str) -> String {
    let s = raw.trim().replace('\\', "/");
    if s.is_empty() || s == "~" {
        return ".".to_string();
    }
    let absolute = s.starts_with('/');
    let mut parts = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !parts.is_empty() {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            _ => parts.push(part),
        }
    }
    let joined = parts.join("/");
    if absolute {
        if joined.is_empty() {
            "/".to_string()
        } else {
            format!("/{joined}")
        }
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

struct SftpClient {
    channel: russh::Channel<client::Msg>,
    next_id: u32,
}

impl SftpClient {
    async fn connect(mut cfg: SshConfig) -> anyhow::Result<Self> {
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            keepalive_interval: Some(Duration::from_secs(30)),
            keepalive_max: 3,
            ..Default::default()
        });
        let handler = ClientHandler::quiet(cfg.host.clone(), cfg.port);
        let mut handle = tokio::time::timeout(
            Duration::from_secs(15),
            client::connect(config, (cfg.host.as_str(), cfg.port), handler),
        )
        .await
        .context("sftp connect timed out")?
        .context("sftp connect")?;
        authenticate_for_remote_fs(&mut handle, &mut cfg)
            .await
            .context("sftp authenticate")?;
        let channel = handle
            .channel_open_session()
            .await
            .context("open sftp session")?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .context("request sftp subsystem")?;
        let mut client = Self {
            channel,
            next_id: 1,
        };
        client.init().await?;
        Ok(client)
    }

    async fn close(&mut self) -> anyhow::Result<()> {
        let _ = self.channel.close().await;
        Ok(())
    }

    async fn init(&mut self) -> anyhow::Result<()> {
        let mut payload = Vec::new();
        put_u32(&mut payload, SFTP_VERSION);
        self.send_raw(SSH_FXP_INIT, payload).await?;
        let packet = self.recv_packet().await?;
        if packet.kind != SSH_FXP_VERSION {
            bail!("unexpected sftp init response {}", packet.kind);
        }
        let mut r = PacketReader::new(&packet.payload);
        let version = r.u32()?;
        if version < 3 {
            bail!("unsupported sftp version {version}");
        }
        Ok(())
    }

    fn next_request_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        id
    }

    async fn readdir(&mut self, path: &str) -> anyhow::Result<Vec<NameEntry>> {
        let id = self.next_request_id();
        let mut payload = Vec::new();
        put_u32(&mut payload, id);
        put_string(&mut payload, path.as_bytes());
        self.send_raw(SSH_FXP_OPENDIR, payload).await?;
        let handle = self.expect_handle(id).await?;
        let mut out = Vec::new();
        loop {
            let req = self.next_request_id();
            let mut payload = Vec::new();
            put_u32(&mut payload, req);
            put_string(&mut payload, &handle);
            self.send_raw(SSH_FXP_READDIR, payload).await?;
            let packet = self.recv_packet().await?;
            if packet.id != Some(req) {
                bail!("sftp request id mismatch");
            }
            match packet.kind {
                SSH_FXP_NAME => {
                    out.extend(parse_name_entries(&packet.payload, true)?);
                }
                SSH_FXP_STATUS => {
                    let status = parse_status_code(&packet.payload)?;
                    if status == SSH_FX_EOF {
                        break;
                    }
                    bail!("sftp readdir failed: status {status}");
                }
                other => bail!("unexpected sftp readdir response {other}"),
            }
        }
        let _ = self.close_handle(&handle).await;
        Ok(out)
    }

    async fn read_file(&mut self, path: &str, max_bytes: u64) -> anyhow::Result<Vec<u8>> {
        let id = self.next_request_id();
        let payload = build_open_payload(id, path, SSH_FXF_READ);
        self.send_raw(SSH_FXP_OPEN, payload).await?;
        let handle = self.expect_handle(id).await?;

        let mut out = Vec::new();
        let mut offset = 0u64;
        while offset < max_bytes {
            let want = ((max_bytes - offset).min(32 * 1024)) as u32;
            let req = self.next_request_id();
            let mut payload = Vec::new();
            put_u32(&mut payload, req);
            put_string(&mut payload, &handle);
            put_u64(&mut payload, offset);
            put_u32(&mut payload, want);
            self.send_raw(SSH_FXP_READ, payload).await?;
            let packet = self.recv_packet().await?;
            if packet.id != Some(req) {
                bail!("sftp request id mismatch");
            }
            match packet.kind {
                103 => {
                    let mut r = PacketReader::new(&packet.payload);
                    let _id = r.u32()?;
                    let data = r.string()?;
                    if data.is_empty() {
                        break;
                    }
                    offset += data.len() as u64;
                    out.extend_from_slice(data);
                    if data.len() < want as usize {
                        break;
                    }
                }
                SSH_FXP_STATUS => {
                    let status = parse_status_code(&packet.payload)?;
                    if status == SSH_FX_EOF {
                        break;
                    }
                    bail!("sftp read failed: status {status}");
                }
                other => bail!("unexpected sftp read response {other}"),
            }
        }
        let _ = self.close_handle(&handle).await;
        Ok(out)
    }

    async fn stat(&mut self, path: &str) -> anyhow::Result<RemoteFileStat> {
        let id = self.next_request_id();
        let mut payload = Vec::new();
        put_u32(&mut payload, id);
        put_string(&mut payload, path.as_bytes());
        self.send_raw(SSH_FXP_STAT, payload).await?;
        let packet = self.recv_packet().await?;
        if packet.id != Some(id) {
            bail!("sftp request id mismatch");
        }
        match packet.kind {
            SSH_FXP_ATTRS => parse_attrs_response(&packet.payload),
            SSH_FXP_STATUS => {
                let status = parse_status_code(&packet.payload)?;
                bail!("sftp stat failed: status {status}");
            }
            other => bail!("unexpected sftp stat response {other}"),
        }
    }

    async fn write_file(&mut self, path: &str, bytes: &[u8]) -> anyhow::Result<RemoteFileStat> {
        let id = self.next_request_id();
        let payload = build_open_payload(id, path, SSH_FXF_WRITE | SSH_FXF_CREAT | SSH_FXF_TRUNC);
        self.send_raw(SSH_FXP_OPEN, payload).await?;
        let handle = self.expect_handle(id).await?;

        let mut offset = 0u64;
        for chunk in bytes.chunks(32 * 1024) {
            let req = self.next_request_id();
            let payload = build_write_payload(req, &handle, offset, chunk);
            self.send_raw(SSH_FXP_WRITE, payload).await?;
            self.expect_status_ok(req, "write").await?;
            offset += chunk.len() as u64;
        }
        let _ = self.close_handle(&handle).await;
        self.stat(path).await.or_else(|_| {
            Ok(RemoteFileStat {
                is_dir: false,
                size: Some(bytes.len() as u64),
                permissions: None,
                mtime: None,
            })
        })
    }

    async fn expect_handle(&mut self, id: u32) -> anyhow::Result<Vec<u8>> {
        let packet = self.recv_packet().await?;
        if packet.id != Some(id) {
            bail!("sftp request id mismatch");
        }
        match packet.kind {
            SSH_FXP_HANDLE => {
                let mut r = PacketReader::new(&packet.payload);
                let _id = r.u32()?;
                Ok(r.string()?.to_vec())
            }
            SSH_FXP_STATUS => {
                let status = parse_status_code(&packet.payload)?;
                bail!("sftp open failed: status {status}");
            }
            other => bail!("unexpected sftp handle response {other}"),
        }
    }

    async fn close_handle(&mut self, handle: &[u8]) -> anyhow::Result<()> {
        let id = self.next_request_id();
        let mut payload = Vec::new();
        put_u32(&mut payload, id);
        put_string(&mut payload, handle);
        self.send_raw(SSH_FXP_CLOSE, payload).await?;
        let packet = self.recv_packet().await?;
        if packet.id != Some(id) {
            bail!("sftp request id mismatch");
        }
        Ok(())
    }

    async fn expect_status_ok(&mut self, id: u32, op: &str) -> anyhow::Result<()> {
        let packet = self.recv_packet().await?;
        if packet.id != Some(id) {
            bail!("sftp request id mismatch");
        }
        match packet.kind {
            SSH_FXP_STATUS => {
                let status = parse_status_code(&packet.payload)?;
                if status == SSH_FX_OK {
                    Ok(())
                } else {
                    bail!("sftp {op} failed: status {status}")
                }
            }
            other => bail!("unexpected sftp {op} response {other}"),
        }
    }

    async fn send_raw(&self, kind: u8, payload: Vec<u8>) -> anyhow::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 5);
        put_u32(&mut frame, (payload.len() + 1) as u32);
        frame.push(kind);
        frame.extend_from_slice(&payload);
        self.channel
            .data_bytes(frame)
            .await
            .context("send sftp packet")
    }

    async fn recv_packet(&mut self) -> anyhow::Result<SftpPacket> {
        let mut data = Vec::new();
        loop {
            if data.len() >= 4 {
                let len = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
                if data.len() >= len + 4 {
                    let packet = parse_sftp_packet(&data[..len + 4])?;
                    return Ok(packet);
                }
            }
            match self.channel.wait().await {
                Some(ChannelMsg::Data { data: chunk }) => data.extend_from_slice(&chunk),
                Some(ChannelMsg::ExtendedData { data: chunk, .. }) => {
                    data.extend_from_slice(&chunk)
                }
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => {
                    bail!("sftp channel closed")
                }
                _ => {}
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FileAttrs {
    size: Option<u64>,
    permissions: Option<u32>,
    mtime: Option<u64>,
}

impl FileAttrs {
    fn is_dir(&self) -> bool {
        self.permissions.is_some_and(|p| (p & S_IFMT) == S_IFDIR)
    }
}

impl From<FileAttrs> for RemoteFileStat {
    fn from(attrs: FileAttrs) -> Self {
        Self {
            is_dir: attrs.is_dir(),
            size: attrs.size,
            permissions: attrs.permissions,
            mtime: attrs.mtime,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NameEntry {
    filename: String,
    _longname: String,
    attrs: FileAttrs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SftpPacket {
    kind: u8,
    id: Option<u32>,
    payload: Vec<u8>,
}

fn parse_sftp_packet(bytes: &[u8]) -> anyhow::Result<SftpPacket> {
    if bytes.len() < 5 {
        bail!("short sftp packet");
    }
    let len = u32::from_be_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if len + 4 > bytes.len() || len == 0 {
        bail!("invalid sftp packet length");
    }
    let kind = bytes[4];
    let payload = bytes[5..4 + len].to_vec();
    let id = if matches!(kind, SSH_FXP_VERSION) || payload.len() < 4 {
        None
    } else {
        Some(u32::from_be_bytes(payload[0..4].try_into().unwrap()))
    };
    Ok(SftpPacket { kind, id, payload })
}

fn parse_name_entries(payload: &[u8], includes_request_id: bool) -> anyhow::Result<Vec<NameEntry>> {
    let mut r = PacketReader::new(payload);
    if includes_request_id {
        let _ = r.u32()?;
    }
    let count = r.u32()?;
    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let filename = String::from_utf8_lossy(r.string()?).to_string();
        let longname = String::from_utf8_lossy(r.string()?).to_string();
        let attrs = r.attrs()?;
        out.push(NameEntry {
            filename,
            _longname: longname,
            attrs,
        });
    }
    Ok(out)
}

fn parse_status_code(payload: &[u8]) -> anyhow::Result<u32> {
    let mut r = PacketReader::new(payload);
    let _id = r.u32()?;
    r.u32()
}

fn parse_attrs_response(payload: &[u8]) -> anyhow::Result<RemoteFileStat> {
    let mut r = PacketReader::new(payload);
    let _id = r.u32()?;
    Ok(r.attrs()?.into())
}

struct PacketReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> PacketReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            bail!("truncated sftp packet");
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u32(&mut self) -> anyhow::Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> anyhow::Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn string(&mut self) -> anyhow::Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn attrs(&mut self) -> anyhow::Result<FileAttrs> {
        let flags = self.u32()?;
        let size = if flags & SSH_FILEXFER_ATTR_SIZE != 0 {
            Some(self.u64()?)
        } else {
            None
        };
        if flags & 0x0000_0002 != 0 {
            let _uid = self.u32()?;
            let _gid = self.u32()?;
        }
        let permissions = if flags & SSH_FILEXFER_ATTR_PERMISSIONS != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let mtime = if flags & SSH_FILEXFER_ATTR_ACMODTIME != 0 {
            let _atime = self.u32()?;
            Some(self.u32()? as u64)
        } else {
            None
        };
        if flags & SSH_FILEXFER_ATTR_EXTENDED != 0 {
            let count = self.u32()?;
            for _ in 0..count {
                let _ty = self.string()?;
                let _data = self.string()?;
            }
        }
        Ok(FileAttrs {
            size,
            permissions,
            mtime,
        })
    }
}

fn put_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    put_u32(out, s.len() as u32);
    out.extend_from_slice(s);
}

fn put_attrs(out: &mut Vec<u8>, attrs: &FileAttrs) {
    let mut flags = 0;
    if attrs.size.is_some() {
        flags |= SSH_FILEXFER_ATTR_SIZE;
    }
    if attrs.permissions.is_some() {
        flags |= SSH_FILEXFER_ATTR_PERMISSIONS;
    }
    if attrs.mtime.is_some() {
        flags |= SSH_FILEXFER_ATTR_ACMODTIME;
    }
    put_u32(out, flags);
    if let Some(size) = attrs.size {
        put_u64(out, size);
    }
    if let Some(p) = attrs.permissions {
        put_u32(out, p);
    }
    if let Some(mtime) = attrs.mtime {
        put_u32(out, 0);
        put_u32(out, mtime as u32);
    }
}

fn build_open_payload(id: u32, path: &str, flags: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    put_u32(&mut payload, id);
    put_string(&mut payload, path.as_bytes());
    put_u32(&mut payload, flags);
    put_attrs(&mut payload, &FileAttrs::default());
    payload
}

fn build_write_payload(id: u32, handle: &[u8], offset: u64, bytes: &[u8]) -> Vec<u8> {
    let mut payload = Vec::new();
    put_u32(&mut payload, id);
    put_string(&mut payload, handle);
    put_u64(&mut payload, offset);
    put_string(&mut payload, bytes);
    payload
}

pub fn is_remote_path_visible_name(name: &str) -> bool {
    !matches!(name, "." | "..") && !name.is_empty()
}

pub fn remote_path_to_virtual_path(remote: &RemoteId) -> PathBuf {
    let mut p = PathBuf::from(format!(
        "ssh://{}@{}:{}",
        remote.user, remote.host, remote.port
    ));
    for part in remote.path.as_str().trim_start_matches('/').split('/') {
        if !part.is_empty() {
            p.push(part);
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name_entry(filename: &str, is_dir: bool, size: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, filename.as_bytes());
        put_string(&mut out, filename.as_bytes());
        put_u32(
            &mut out,
            SSH_FILEXFER_ATTR_SIZE | SSH_FILEXFER_ATTR_PERMISSIONS | SSH_FILEXFER_ATTR_ACMODTIME,
        );
        put_u64(&mut out, size);
        put_u32(&mut out, if is_dir { S_IFDIR | 0o755 } else { 0o100644 });
        put_u32(&mut out, 11);
        put_u32(&mut out, 22);
        out
    }

    #[test]
    fn remote_path_normalizes_without_becoming_windows_path() {
        assert_eq!(RemotePath::new("/home/me/../app//").as_str(), "/home/app");
        assert_eq!(
            RemotePath::new(r"\Users\me\proj").as_str(),
            "/Users/me/proj"
        );
        assert_eq!(RemotePath::new("").as_str(), ".");
        assert_eq!(RemotePath::new("/").as_str(), "/");
    }

    #[test]
    fn remote_id_builds_children_and_virtual_paths() {
        let cfg = SshConfig {
            host: "example.com".into(),
            port: 2222,
            user: "alice".into(),
            key_path: None,
            password: None,
        };
        let id = RemoteId::new(&cfg, "/home/alice");
        let child = id.child("src");
        assert_eq!(child.path.as_str(), "/home/alice/src");
        assert_eq!(
            remote_path_to_virtual_path(&child)
                .to_string_lossy()
                .replace('\\', "/"),
            "ssh://alice@example.com:2222/home/alice/src"
        );
    }

    #[test]
    fn parse_name_response_extracts_attrs() {
        let mut payload = Vec::new();
        put_u32(&mut payload, 7);
        put_u32(&mut payload, 2);
        payload.extend_from_slice(&name_entry("src", true, 0));
        payload.extend_from_slice(&name_entry("main.rs", false, 42));

        let entries = parse_name_entries(&payload, true).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].filename, "src");
        assert!(entries[0].attrs.is_dir());
        assert_eq!(entries[1].filename, "main.rs");
        assert!(!entries[1].attrs.is_dir());
        assert_eq!(entries[1].attrs.size, Some(42));
        assert_eq!(entries[1].attrs.mtime, Some(22));
    }

    #[test]
    fn parse_sftp_packet_reports_request_id() {
        let mut frame = Vec::new();
        let mut payload = Vec::new();
        put_u32(&mut payload, 99);
        put_u32(&mut payload, SSH_FX_EOF);
        put_string(&mut payload, b"eof");
        put_string(&mut payload, b"");
        put_u32(&mut frame, (payload.len() + 1) as u32);
        frame.push(SSH_FXP_STATUS);
        frame.extend_from_slice(&payload);

        let packet = parse_sftp_packet(&frame).unwrap();
        assert_eq!(packet.kind, SSH_FXP_STATUS);
        assert_eq!(packet.id, Some(99));
        assert_eq!(parse_status_code(&packet.payload).unwrap(), SSH_FX_EOF);
    }

    #[test]
    fn parse_attrs_response_extracts_remote_file_stat() {
        let mut payload = Vec::new();
        put_u32(&mut payload, 12);
        put_u32(
            &mut payload,
            SSH_FILEXFER_ATTR_SIZE | SSH_FILEXFER_ATTR_PERMISSIONS | SSH_FILEXFER_ATTR_ACMODTIME,
        );
        put_u64(&mut payload, 1234);
        put_u32(&mut payload, 0o100644);
        put_u32(&mut payload, 111);
        put_u32(&mut payload, 222);

        let stat = parse_attrs_response(&payload).unwrap();
        assert_eq!(stat.size, Some(1234));
        assert_eq!(stat.permissions, Some(0o100644));
        assert_eq!(stat.mtime, Some(222));
        assert!(!stat.is_dir);
    }

    #[test]
    fn write_open_payload_uses_write_create_truncate_flags() {
        let payload = build_open_payload(41, "/home/alice/app.rs", SSH_FXF_WRITE | SSH_FXF_CREAT | SSH_FXF_TRUNC);
        let mut r = PacketReader::new(&payload);
        assert_eq!(r.u32().unwrap(), 41);
        assert_eq!(r.string().unwrap(), b"/home/alice/app.rs");
        assert_eq!(r.u32().unwrap(), SSH_FXF_WRITE | SSH_FXF_CREAT | SSH_FXF_TRUNC);
    }

    #[test]
    fn write_payload_carries_offset_and_data() {
        let handle = b"h1";
        let payload = build_write_payload(42, handle, 32768, b"hello");
        let mut r = PacketReader::new(&payload);
        assert_eq!(r.u32().unwrap(), 42);
        assert_eq!(r.string().unwrap(), handle);
        assert_eq!(r.u64().unwrap(), 32768);
        assert_eq!(r.string().unwrap(), b"hello");
    }
}
