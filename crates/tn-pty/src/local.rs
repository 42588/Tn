//! Local pseudo-terminal backed by the OS (ConPTY on Windows) via `portable-pty`.

#[cfg(windows)]
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};

use anyhow::Context;
use portable_pty::{native_pty_system, Child, ChildKiller, CommandBuilder, MasterPty, SlavePty};

use crate::{Killer, PtyBackend, PtySize, SpawnSpec};

/// A local PTY session. Holds the master side and the spawned child.
pub struct LocalPty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader: Option<Box<dyn Read + Send>>,
    // Keep the slave alive for the session. On ConPTY, dropping it early can
    // disturb the pseudo-console; we manage lifecycle via the child + killer.
    _slave: Box<dyn SlavePty + Send>,
}

impl LocalPty {
    /// Open a PTY and spawn the program described by `spec`.
    pub fn spawn(spec: &SpawnSpec, size: PtySize) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size.into())
            .context("openpty (ConPTY) failed")?;

        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        preserve_process_path(
            &mut cmd,
            std::env::var_os("PATH")
                .or_else(|| std::env::var_os("Path"))
                .as_deref(),
        );

        // Tn 是独立终端,但开发期常从 VS Code/Cursor 的集成终端启动(`cargo run`),会继承
        // `TERM_PROGRAM=vscode` + 一串 `VSCODE_*`。CommandBuilder 默认继承本进程环境
        // (get_base_env → std::env::vars_os),这些会原样传给子进程 → **Claude Code 据此
        // 误判「我在 VS Code 里」,反复尝试连接那个并不存在的 IDE 后端 → 右下角「IDE
        // extension install failed」+ 高频重试刷新 → 卡顿**(真机实证)。剥离这些 IDE 标记、
        // 并把 TERM_PROGRAM 声明为 Tn,让子进程(尤其 agent)以纯终端模式运行。env_remove
        // 作用于继承来的 base env(见 portable-pty 自带测试),故能真正阻断传递;对普通 shell
        // 同样适用(Tn 的 shell 也不该假装在 VS Code 里)。
        for k in [
            "TERM_PROGRAM_VERSION",
            "VSCODE_IPC_HOOK_CLI",
            "VSCODE_GIT_IPC_HANDLE",
            "VSCODE_GIT_ASKPASS_MAIN",
            "VSCODE_GIT_ASKPASS_NODE",
            "VSCODE_GIT_ASKPASS_EXTRA_ARGS",
            "VSCODE_INJECTION",
            "VSCODE_NONCE",
            "VSCODE_CWD",
            "VSCODE_PID",
            "VSCODE_L10N_BUNDLE_LOCATION",
            "ELECTRON_RUN_AS_NODE",
            "CURSOR_TRACE_ID",
        ] {
            cmd.env_remove(k);
        }
        cmd.env("TERM_PROGRAM", "Tn");

        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("failed to spawn `{}`", spec.program))?;

        let reader = pair
            .master
            .try_clone_reader()
            .context("try_clone_reader failed")?;

        Ok(Self {
            master: pair.master,
            child,
            reader: Some(reader),
            _slave: pair.slave,
        })
    }

    /// Process id of the child, if available.
    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }
}

impl Drop for LocalPty {
    fn drop(&mut self) {
        // portable-pty does NOT kill the child on drop. Do it explicitly so
        // closing a pane/tab terminates its process — otherwise agents/shells
        // are orphaned and keep running.
        let _ = self.child.clone_killer().kill();
    }
}

/// Adapts portable-pty's `ChildKiller` to our [`Killer`] trait.
struct PortableKiller(Box<dyn ChildKiller + Send + Sync>);

impl Killer for PortableKiller {
    fn kill(&mut self) -> anyhow::Result<()> {
        self.0.kill().context("failed to kill child")
    }
}

impl PtyBackend for LocalPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        self.master.resize(size.into()).context("pty resize failed")
    }

    fn take_reader(&mut self) -> anyhow::Result<Box<dyn Read + Send>> {
        self.reader.take().context("pty reader already taken")
    }

    fn writer(&self) -> anyhow::Result<Box<dyn Write + Send>> {
        self.master.take_writer().context("take_writer failed")
    }

    fn killer(&self) -> anyhow::Result<Box<dyn Killer>> {
        Ok(Box::new(PortableKiller(self.child.clone_killer())))
    }

    fn wait(&mut self) -> anyhow::Result<i32> {
        let status = self.child.wait().context("wait failed")?;
        Ok(status.exit_code() as i32)
    }

    fn try_wait(&mut self) -> anyhow::Result<Option<i32>> {
        Ok(self
            .child
            .try_wait()
            .context("try_wait failed")?
            .map(|status| status.exit_code() as i32))
    }
}

#[cfg(windows)]
fn merge_path_env(builder_path: Option<&OsStr>, process_path: Option<&OsStr>) -> Option<OsString> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for path in [builder_path, process_path].into_iter().flatten() {
        for entry in path.to_string_lossy().split(';') {
            if entry.is_empty() {
                continue;
            }
            if seen.insert(entry.to_ascii_lowercase()) {
                out.push(entry.to_string());
            }
        }
    }

    if out.is_empty() {
        None
    } else {
        Some(OsString::from(out.join(";")))
    }
}

#[cfg(windows)]
fn preserve_process_path(cmd: &mut CommandBuilder, process_path: Option<&OsStr>) {
    let Some(path) = merge_path_env(cmd.get_env("Path"), process_path) else {
        return;
    };
    cmd.env("Path", path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn merge_path_env_appends_process_only_entries_case_insensitively() {
        let builder = OsStr::new(r"C:\Windows;C:\Program Files\nodejs");
        let process =
            OsStr::new(r"C:\Users\Gua\AppData\Roaming\npm;C:\Users\Gua\.cargo\bin;c:\windows");

        assert_eq!(
            merge_path_env(Some(builder), Some(process)).as_deref(),
            Some(OsStr::new(
                r"C:\Windows;C:\Program Files\nodejs;C:\Users\Gua\AppData\Roaming\npm;C:\Users\Gua\.cargo\bin"
            ))
        );
    }

    #[test]
    #[cfg(windows)]
    fn preserve_process_path_updates_command_builder_path() {
        let mut cmd = CommandBuilder::new("powershell.exe");
        cmd.env("Path", r"C:\Windows;C:\Program Files\nodejs");

        preserve_process_path(
            &mut cmd,
            Some(OsStr::new(
                r"C:\Users\Gua\AppData\Roaming\npm;C:\Users\Gua\.cargo\bin;c:\windows",
            )),
        );

        assert_eq!(
            cmd.get_env("Path"),
            Some(OsStr::new(
                r"C:\Windows;C:\Program Files\nodejs;C:\Users\Gua\AppData\Roaming\npm;C:\Users\Gua\.cargo\bin"
            ))
        );
    }
}
