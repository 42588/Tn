//! Local pseudo-terminal backed by the OS (ConPTY on Windows) via `portable-pty`.

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
