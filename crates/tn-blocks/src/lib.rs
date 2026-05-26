//! Warp-style command blocks (M3).
//!
//! A [`BlockModel`] consumes [`tn_shell::BlockEvent`]s — each tagged with the
//! absolute grid line it occurred on and a millisecond timestamp — and builds a
//! list of [`Block`]s (a command, its output line range, exit code, duration).
//!
//! Blocks are a **semantic index over the scrollback**, not a replacement grid:
//! they store line anchors so the renderer can draw chrome around the live grid
//! (collapse / copy / rerun) and re-resolve on reflow. State machine:
//! `Prompt → Input → Running → Finished`.
//!
//! Headless: the caller (tn-ui) supplies the current cursor line and a clock;
//! tests pass synthetic values.

use tn_shell::BlockEvent;

/// Where a block is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockState {
    /// Prompt is drawing (OSC 133 `A` seen).
    Prompt,
    /// Command input begun (`B`).
    Input,
    /// Command executing, output streaming (`C`).
    Running,
    /// Command finished (`D`) — `exit` is set.
    Finished,
}

/// One command block: prompt → command → output, with exit code + timing.
/// Line numbers are absolute grid lines (anchors into the scrollback).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub id: u64,
    pub state: BlockState,
    /// Command line text, if the shell reported it (OSC 633 `E`).
    pub command: Option<String>,
    /// Working directory at prompt time, if known.
    pub cwd: Option<String>,
    /// Line where the prompt started (`A`).
    pub prompt_line: u64,
    /// Line where command input started (`B`).
    pub input_line: Option<u64>,
    /// Line where output started (`C`).
    pub output_start: Option<u64>,
    /// Line where the command finished (`D`).
    pub output_end: Option<u64>,
    /// Exit code, if reported.
    pub exit: Option<i32>,
    /// Start time (ms): set at `B`, refined to `C` (execution start) when seen.
    pub started_at: Option<u64>,
    /// Finish time (ms), at `D`.
    pub finished_at: Option<u64>,
}

impl Block {
    fn new(id: u64, prompt_line: u64, cwd: Option<String>) -> Self {
        Self {
            id,
            state: BlockState::Prompt,
            command: None,
            cwd,
            prompt_line,
            input_line: None,
            output_start: None,
            output_end: None,
            exit: None,
            started_at: None,
            finished_at: None,
        }
    }

    /// Wall-clock duration (ms) from execution start to finish, if both known.
    pub fn duration_ms(&self) -> Option<u64> {
        Some(self.finished_at?.saturating_sub(self.started_at?))
    }

    /// `Some(true)` if the command exited 0, `Some(false)` if non-zero, `None`
    /// if it isn't finished or reported no code.
    pub fn succeeded(&self) -> Option<bool> {
        self.exit.map(|c| c == 0)
    }

    /// Whether the command is still running (output open, not yet finished).
    pub fn is_running(&self) -> bool {
        self.state == BlockState::Running
    }
}

/// Accumulates blocks from a shell-integration event stream.
#[derive(Default)]
pub struct BlockModel {
    finished: Vec<Block>,
    current: Option<Block>,
    cwd: Option<String>,
    next_id: u64,
}

impl BlockModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one event. `line` = the absolute grid line it occurred on; `at_ms` =
    /// a monotonic-ish millisecond timestamp (both ignored for `CommandLine`/`Cwd`).
    pub fn on_event(&mut self, event: BlockEvent, line: u64, at_ms: u64) {
        match event {
            BlockEvent::PromptStart => {
                // A new prompt while a block is still open means the previous one
                // ended without a `D` (e.g. Ctrl+C) — finalize it implicitly.
                if let Some(mut b) = self.current.take() {
                    b.output_end.get_or_insert(line.saturating_sub(1));
                    b.state = BlockState::Finished;
                    self.finished.push(b);
                }
                let id = self.next_id;
                self.next_id += 1;
                self.current = Some(Block::new(id, line, self.cwd.clone()));
            }
            BlockEvent::CommandStart => {
                if let Some(b) = &mut self.current {
                    b.input_line = Some(line);
                    b.started_at = Some(at_ms);
                    b.state = BlockState::Input;
                }
            }
            BlockEvent::OutputStart => {
                if let Some(b) = &mut self.current {
                    b.output_start = Some(line);
                    b.started_at = Some(at_ms); // refine to execution start
                    b.state = BlockState::Running;
                }
            }
            BlockEvent::CommandFinished { exit } => {
                if let Some(mut b) = self.current.take() {
                    b.output_end = Some(line);
                    b.exit = exit;
                    b.finished_at = Some(at_ms);
                    b.state = BlockState::Finished;
                    self.finished.push(b);
                }
            }
            BlockEvent::CommandLine(cmd) => {
                if let Some(b) = &mut self.current {
                    b.command = Some(cmd);
                }
            }
            BlockEvent::Cwd(path) => {
                self.cwd = Some(path.clone());
                if let Some(b) = &mut self.current {
                    b.cwd = Some(path);
                }
            }
        }
    }

    /// All blocks, oldest first, including the open one (if any) last.
    pub fn iter(&self) -> impl Iterator<Item = &Block> {
        self.finished.iter().chain(self.current.iter())
    }

    /// The currently open (not-yet-finished) block, if any.
    pub fn current(&self) -> Option<&Block> {
        self.current.as_ref()
    }

    /// Number of finished blocks.
    pub fn finished_len(&self) -> usize {
        self.finished.len()
    }

    /// The last known working directory.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_command_cycle_builds_one_block() {
        let mut m = BlockModel::new();
        m.on_event(BlockEvent::PromptStart, 0, 0);
        m.on_event(BlockEvent::CommandLine("dir".into()), 0, 0);
        m.on_event(BlockEvent::CommandStart, 0, 100);
        m.on_event(BlockEvent::OutputStart, 1, 120);
        m.on_event(BlockEvent::CommandFinished { exit: Some(0) }, 5, 270);

        assert_eq!(m.finished_len(), 1);
        assert!(m.current().is_none());
        let b = m.iter().next().unwrap();
        assert_eq!(b.command.as_deref(), Some("dir"));
        assert_eq!(b.exit, Some(0));
        assert_eq!(b.succeeded(), Some(true));
        assert_eq!(b.prompt_line, 0);
        assert_eq!(b.output_start, Some(1));
        assert_eq!(b.output_end, Some(5));
        assert_eq!(b.state, BlockState::Finished);
        assert_eq!(b.duration_ms(), Some(150)); // 270 - 120 (execution start at C)
    }

    #[test]
    fn open_block_is_running_until_finished() {
        let mut m = BlockModel::new();
        m.on_event(BlockEvent::PromptStart, 0, 0);
        m.on_event(BlockEvent::CommandStart, 0, 10);
        m.on_event(BlockEvent::OutputStart, 1, 20);
        assert_eq!(m.finished_len(), 0);
        let cur = m.current().unwrap();
        assert!(cur.is_running());
        assert_eq!(cur.exit, None);
    }

    #[test]
    fn new_prompt_finalizes_interrupted_block() {
        let mut m = BlockModel::new();
        m.on_event(BlockEvent::PromptStart, 0, 0);
        m.on_event(BlockEvent::OutputStart, 1, 10);
        // No D — a fresh prompt arrives (e.g. Ctrl+C).
        m.on_event(BlockEvent::PromptStart, 4, 50);
        assert_eq!(m.finished_len(), 1);
        let first = m.iter().next().unwrap();
        assert_eq!(first.state, BlockState::Finished);
        assert_eq!(first.exit, None); // never reported
        assert_eq!(first.output_end, Some(3)); // line - 1
    }

    #[test]
    fn cwd_tracked_and_attached() {
        let mut m = BlockModel::new();
        m.on_event(BlockEvent::Cwd("C:/work".into()), 0, 0);
        m.on_event(BlockEvent::PromptStart, 0, 0);
        assert_eq!(m.cwd(), Some("C:/work"));
        assert_eq!(m.current().unwrap().cwd.as_deref(), Some("C:/work"));
    }

    #[test]
    fn nonzero_exit_marks_failure() {
        let mut m = BlockModel::new();
        m.on_event(BlockEvent::PromptStart, 0, 0);
        m.on_event(BlockEvent::CommandStart, 0, 0);
        m.on_event(BlockEvent::CommandFinished { exit: Some(1) }, 2, 5);
        assert_eq!(m.iter().next().unwrap().succeeded(), Some(false));
    }
}
