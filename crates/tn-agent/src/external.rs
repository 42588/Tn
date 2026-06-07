//! External/sidecar agent adapter building blocks.
//!
//! This is the realtime event tier: a sidecar client can feed JSONL lines into
//! [`ExternalEventAdapter::ingest_json_line`], or [`ExternalProcessAdapter`] can
//! own a stdio child and read JSONL from stdout. Both expose facts as ordinary
//! [`AgentEvent`]s through the [`AgentAdapter`] trait. HTTP/WebSocket clients can
//! reuse the same queue without changing the UI contract.

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::Deserialize;

use crate::{AgentAdapter, AgentDescriptor, AgentEvent, AgentStatus, AiUsage};

#[derive(Clone)]
pub struct ExternalEventAdapter {
    descriptor: AgentDescriptor,
    queue: Arc<Mutex<VecDeque<AgentEvent>>>,
}

impl ExternalEventAdapter {
    pub fn new(descriptor: AgentDescriptor) -> Self {
        Self {
            descriptor,
            queue: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn ingest_event(&self, event: AgentEvent) {
        self.queue.lock().unwrap().push_back(event);
    }

    pub fn ingest_json_line(&self, line: &str) -> Result<(), serde_json::Error> {
        let event: ExternalWireEvent = serde_json::from_str(line)?;
        self.ingest_event(event.into());
        Ok(())
    }
}

impl AgentAdapter for ExternalEventAdapter {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    fn has_realtime_events(&self) -> bool {
        true
    }

    fn drain_events(&self) -> Vec<AgentEvent> {
        self.queue.lock().unwrap().drain(..).collect()
    }
}

/// A minimal stdio/JSONL realtime adapter for external sidecars.
///
/// The child process is expected to write one JSON object per line on stdout,
/// using the same wire shape accepted by [`ExternalEventAdapter::ingest_json_line`].
/// The adapter owns the child and terminates it on drop; transport-specific
/// protocols (JSON-RPC framing, WebSocket, HTTP) can build on the same event
/// queue without changing the UI contract.
pub struct ExternalProcessAdapter {
    inner: ExternalEventAdapter,
    child: Arc<Mutex<Option<Child>>>,
}

impl ExternalProcessAdapter {
    /// Spawn from the descriptor's own `realtime_command` (first token = program,
    /// rest = args). `None` when the descriptor declares no sidecar — so the host
    /// can do `if let Some(res) = ExternalProcessAdapter::from_descriptor(d)`.
    pub fn from_descriptor(descriptor: AgentDescriptor) -> Option<std::io::Result<Self>> {
        let cmd = descriptor.realtime_command.clone()?;
        let (program, args) = cmd.split_first()?;
        let program = program.clone();
        let args: Vec<String> = args.to_vec();
        Some(Self::spawn(descriptor, program, args))
    }

    pub fn spawn<I, S, P>(descriptor: AgentDescriptor, program: P, args: I) -> std::io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
        P: AsRef<OsStr>,
    {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let inner = ExternalEventAdapter::new(descriptor);

        if let Some(stdout) = stdout {
            let events = inner.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) if line.trim().is_empty() => {}
                        Ok(line) => {
                            if let Err(err) = events.ingest_json_line(&line) {
                                events.ingest_event(AgentEvent::ErrorReported(format!(
                                    "invalid external agent event: {err}"
                                )));
                            }
                        }
                        Err(err) => {
                            events.ingest_event(AgentEvent::ErrorReported(format!(
                                "external agent stdout read failed: {err}"
                            )));
                            break;
                        }
                    }
                }
                events.ingest_event(AgentEvent::SessionEnded);
            });
        }

        if let Some(stderr) = stderr {
            let events = inner.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    let line = line.trim();
                    if !line.is_empty() {
                        events.ingest_event(AgentEvent::ErrorReported(line.to_string()));
                    }
                }
            });
        }

        Ok(Self {
            inner,
            child: Arc::new(Mutex::new(Some(child))),
        })
    }
}

impl Drop for ExternalProcessAdapter {
    fn drop(&mut self) {
        let Some(mut child) = self.child.lock().unwrap().take() else {
            return;
        };
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

impl AgentAdapter for ExternalProcessAdapter {
    fn descriptor(&self) -> &AgentDescriptor {
        self.inner.descriptor()
    }

    fn has_realtime_events(&self) -> bool {
        true
    }

    fn drain_events(&self) -> Vec<AgentEvent> {
        self.inner.drain_events()
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ExternalWireEvent {
    SessionStarted,
    SessionEnded,
    CwdChanged { cwd: String },
    ModelChanged { model: String },
    UsageUpdated { usage: AiUsage },
    StatusChanged { status: AgentStatus },
    TranscriptAppended { text: String },
    DiffChanged,
    PermissionRequested { prompt: String },
    ErrorReported { message: String },
}

impl From<ExternalWireEvent> for AgentEvent {
    fn from(value: ExternalWireEvent) -> Self {
        match value {
            ExternalWireEvent::SessionStarted => AgentEvent::SessionStarted,
            ExternalWireEvent::SessionEnded => AgentEvent::SessionEnded,
            ExternalWireEvent::CwdChanged { cwd } => AgentEvent::CwdChanged(cwd),
            ExternalWireEvent::ModelChanged { model } => AgentEvent::ModelChanged(model),
            ExternalWireEvent::UsageUpdated { usage } => AgentEvent::UsageUpdated(usage),
            ExternalWireEvent::StatusChanged { status } => AgentEvent::StatusChanged(status),
            ExternalWireEvent::TranscriptAppended { text } => AgentEvent::TranscriptAppended(text),
            ExternalWireEvent::DiffChanged => AgentEvent::DiffChanged,
            ExternalWireEvent::PermissionRequested { prompt } => {
                AgentEvent::PermissionRequested(prompt)
            }
            ExternalWireEvent::ErrorReported { message } => AgentEvent::ErrorReported(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentId, AgentNetworkPolicy, AgentRuntimeKind};
    use std::time::{Duration, Instant};

    fn descriptor() -> AgentDescriptor {
        let mut d = AgentDescriptor::generic(AgentId::new("sidecar"), "Sidecar");
        d.runtime_support = vec![AgentRuntimeKind::Structured, AgentRuntimeKind::Http];
        d.network_policy = AgentNetworkPolicy::Ask;
        d.capabilities.usage = true;
        d.capabilities.transcript = true;
        d.capabilities.permission_prompts = true;
        d
    }

    #[test]
    fn parses_jsonl_events_into_agent_events() {
        let adapter = ExternalEventAdapter::new(descriptor());
        adapter
            .ingest_json_line(r#"{"type":"status_changed","status":"running"}"#)
            .unwrap();
        adapter
            .ingest_json_line(r#"{"type":"model_changed","model":"gpt-5"}"#)
            .unwrap();
        adapter
            .ingest_json_line(r#"{"type":"permission_requested","prompt":"Run tests?"}"#)
            .unwrap();

        let events = adapter.drain_events();
        assert_eq!(
            events,
            vec![
                AgentEvent::StatusChanged(AgentStatus::Running),
                AgentEvent::ModelChanged("gpt-5".into()),
                AgentEvent::PermissionRequested("Run tests?".into()),
            ]
        );
        assert!(adapter.drain_events().is_empty());
    }

    #[test]
    fn parses_usage_event() {
        let adapter = ExternalEventAdapter::new(descriptor());
        adapter
            .ingest_json_line(
                r#"{"type":"usage_updated","usage":{"model":"m","input":1,"output":2,"cache_create":3,"cache_read":4,"context_used":5,"context_max":10,"cost_usd":0.25,"turns":1}}"#,
            )
            .unwrap();
        assert_eq!(
            adapter.drain_events(),
            vec![AgentEvent::UsageUpdated(AiUsage {
                model: "m".into(),
                input: 1,
                output: 2,
                cache_create: 3,
                cache_read: 4,
                context_used: 5,
                context_max: 10,
                cost_usd: 0.25,
                turns: 1,
            })]
        );
    }

    #[test]
    fn external_process_adapter_reads_stdout_jsonl() {
        let script = std::env::temp_dir().join(format!(
            "tn-agent-event-{}-{}.cmd",
            std::process::id(),
            "stdout"
        ));
        std::fs::write(
            &script,
            "@echo off\r\necho {\"type\":\"status_changed\",\"status\":\"running\"}\r\n",
        )
        .unwrap();
        let adapter =
            ExternalProcessAdapter::spawn(descriptor(), &script, std::iter::empty::<&str>())
                .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            events.extend(adapter.drain_events());
            if events
                .iter()
                .any(|e| e == &AgentEvent::StatusChanged(AgentStatus::Running))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(script);
        assert!(events.contains(&AgentEvent::StatusChanged(AgentStatus::Running)));
    }
}
