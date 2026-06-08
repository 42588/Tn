//! Bounded remote command execution for SSH-backed workflows.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as _};
use russh::client;
use russh::ChannelMsg;

use crate::remote_fs::RemotePath;
use crate::ssh::{authenticate_for_remote_fs, ClientHandler};
use crate::SshConfig;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteCommandOutput {
    pub status: Option<u32>,
    pub stdout: String,
    pub stderr: String,
}

impl RemoteCommandOutput {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

pub trait RemoteCommandService: Send + Sync {
    fn run(
        &self,
        cfg: &SshConfig,
        cwd: &RemotePath,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> anyhow::Result<RemoteCommandOutput>;

    fn run_with_stdin(
        &self,
        cfg: &SshConfig,
        cwd: &RemotePath,
        program: &str,
        args: &[&str],
        stdin: &[u8],
        timeout: Duration,
    ) -> anyhow::Result<RemoteCommandOutput> {
        if stdin.is_empty() {
            self.run(cfg, cwd, program, args, timeout)
        } else {
            Err(anyhow!("remote command stdin unsupported"))
        }
    }
}

#[derive(Default)]
pub struct SshCommandService;

impl SshCommandService {
    pub fn shared() -> Arc<dyn RemoteCommandService> {
        Arc::new(Self)
    }
}

impl RemoteCommandService for SshCommandService {
    fn run(
        &self,
        cfg: &SshConfig,
        cwd: &RemotePath,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> anyhow::Result<RemoteCommandOutput> {
        let cfg = cfg.clone();
        let cwd = cwd.clone();
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let program = program.to_string();
        run_remote_future(move || async move {
            run_remote_command_inner(cfg, cwd, program, argv, Vec::new(), timeout).await
        })
    }

    fn run_with_stdin(
        &self,
        cfg: &SshConfig,
        cwd: &RemotePath,
        program: &str,
        args: &[&str],
        stdin: &[u8],
        timeout: Duration,
    ) -> anyhow::Result<RemoteCommandOutput> {
        let cfg = cfg.clone();
        let cwd = cwd.clone();
        let argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let program = program.to_string();
        let stdin = stdin.to_vec();
        run_remote_future(move || async move {
            run_remote_command_inner(cfg, cwd, program, argv, stdin, timeout).await
        })
    }
}

async fn run_remote_command_inner(
    mut cfg: SshConfig,
    cwd: RemotePath,
    program: String,
    args: Vec<String>,
    stdin: Vec<u8>,
    timeout: Duration,
) -> anyhow::Result<RemoteCommandOutput> {
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
    .context("remote command connect timed out")?
    .context("remote command connect")?;
    authenticate_for_remote_fs(&mut handle, &mut cfg)
        .await
        .context("remote command authenticate")?;

    let command = build_remote_command(
        cwd.as_str(),
        &program,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    let command_future = async {
        let mut channel = handle
            .channel_open_session()
            .await
            .context("open remote command session")?;
        channel
            .exec(true, command.into_bytes())
            .await
            .context("request remote command exec")?;
        if !stdin.is_empty() {
            channel
                .data_bytes(stdin)
                .await
                .context("send remote command stdin")?;
        }
        channel.eof().await.context("finish remote command stdin")?;

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut status = None;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => stdout.extend_from_slice(&data),
                Some(ChannelMsg::ExtendedData { data, .. }) => stderr.extend_from_slice(&data),
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    status = Some(exit_status);
                }
                Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                _ => {}
            }
        }
        let _ = channel.close().await;
        Ok::<RemoteCommandOutput, anyhow::Error>(RemoteCommandOutput {
            status,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    };

    let result = tokio::time::timeout(timeout, command_future)
        .await
        .context("remote command timed out")??;
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(result)
}

fn run_remote_future<T, Fut>(f: impl FnOnce() -> Fut + Send + 'static) -> anyhow::Result<T>
where
    T: Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let thread = std::thread::Builder::new()
        .name("tn-ssh-cmd".into())
        .spawn(move || -> anyhow::Result<T> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("create remote command runtime")?;
            rt.block_on(f())
        })
        .context("spawn remote command thread")?;
    thread
        .join()
        .map_err(|_| anyhow!("remote command thread panicked"))?
}

pub fn build_remote_command(cwd: &str, program: &str, args: &[&str]) -> String {
    let program = program.trim();
    if program.is_empty() {
        return format!("cd {}", posix_quote(cwd));
    }
    let mut cmd = format!("cd {} && {}", posix_quote(cwd), posix_quote(program));
    for arg in args {
        cmd.push(' ');
        cmd.push_str(&posix_quote(arg));
    }
    cmd
}

pub fn posix_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.bytes().all(|b| {
        matches!(
            b,
            b'a'..=b'z'
                | b'A'..=b'Z'
                | b'0'..=b'9'
                | b'_'
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b'='
        )
    }) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_command_wraps_cwd_and_quotes_args_for_posix_shell() {
        let cmd = build_remote_command(
            "/home/alice/my repo",
            "git",
            &["-c", "core.quotePath=false", "diff", "--", "src/a b.rs"],
        );

        assert_eq!(
            cmd,
            "cd '/home/alice/my repo' && git -c core.quotePath=false diff -- 'src/a b.rs'"
        );
    }

    #[test]
    fn posix_quote_handles_single_quotes() {
        assert_eq!(posix_quote("can\'t"), "'can'\\''t'");
    }

    #[test]
    fn remote_command_output_reports_success_from_exit_status() {
        let ok = RemoteCommandOutput {
            status: Some(0),
            stdout: "done".into(),
            stderr: String::new(),
        };
        let failed = RemoteCommandOutput {
            status: Some(1),
            stdout: String::new(),
            stderr: "bad".into(),
        };

        assert!(ok.success());
        assert!(!failed.success());
    }
}
