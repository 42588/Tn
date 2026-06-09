use std::time::Instant;

use tn_config::EffectiveMotion;

use super::geometry::disp_width;

pub const CARET_GLIDE_MS: u64 = 90;
pub const SETTLE_MS: u64 = CARET_GLIDE_MS;
const CARET_CHASE_FACTOR: f32 = 0.4;
const CARET_CHASE_SNAP_PX: f32 = 0.5;
const MAX_GLIDE_COLS: i32 = 12;
const LARGE_FILE_LINE_THRESHOLD: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionKind {
    Instant,
    Subtle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionTrigger {
    Insert {
        from: (usize, usize),
        to: (usize, usize),
        inserted: Option<char>,
    },
    Delete {
        from: (usize, usize),
        to: (usize, usize),
    },
    Move {
        from: (usize, usize),
        to: (usize, usize),
    },
    Snap,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaretMotionInput {
    pub policy: EffectiveMotion,
    pub high_load: bool,
    pub ime_active: bool,
    pub selecting: bool,
    pub visual_from: Option<(usize, usize)>,
    pub visual_to: Option<(usize, usize)>,
    pub char_w: f32,
    pub line_h: f32,
    pub large_file: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettleGlyph {
    pub row: usize,
    pub col: usize,
    pub ch: char,
    pub alpha: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionSnapshot {
    pub caret_dx: f32,
    pub caret_dy: f32,
    pub caret_scale_x: f32,
    pub caret_scale_y: f32,
    pub settle: Option<SettleGlyph>,
}

impl Default for MotionSnapshot {
    fn default() -> Self {
        Self {
            caret_dx: 0.0,
            caret_dy: 0.0,
            caret_scale_x: 1.0,
            caret_scale_y: 1.0,
            settle: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActiveMotion {
    started_at: Instant,
    visual_row: usize,
    target_col: f32,
    drawn_col: f32,
    char_w: f32,
    line_h: f32,
    forward: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaretMotionState {
    active: Option<ActiveMotion>,
}

impl CaretMotionState {
    pub fn record(&mut self, trigger: MotionTrigger, now: Instant, input: CaretMotionInput) {
        let carried_drawn_col = self.active.and_then(|active| {
            let visual_from = input.visual_from?;
            let still_active = now.saturating_duration_since(active.started_at).as_millis()
                < CARET_GLIDE_MS as u128;
            (still_active && active.visual_row == visual_from.0).then_some(active.drawn_col)
        });
        self.active = active_motion(trigger, now, input, carried_drawn_col);
    }

    pub fn snap(&mut self) {
        self.active = None;
    }

    pub fn is_animating(&self, now: Instant) -> bool {
        self.active
            .map(|active| {
                now.saturating_duration_since(active.started_at).as_millis()
                    < CARET_GLIDE_MS as u128
            })
            .unwrap_or(false)
    }
}

pub fn large_file_motion_gate(line_count: usize) -> bool {
    line_count >= LARGE_FILE_LINE_THRESHOLD
}

fn effective_kind(input: CaretMotionInput) -> MotionKind {
    if input.high_load || input.ime_active || input.selecting || input.large_file {
        return MotionKind::Instant;
    }
    match input.policy {
        EffectiveMotion::Instant => MotionKind::Instant,
        EffectiveMotion::Subtle | EffectiveMotion::Full => MotionKind::Subtle,
    }
}

fn active_motion(
    trigger: MotionTrigger,
    now: Instant,
    input: CaretMotionInput,
    carried_drawn_col: Option<f32>,
) -> Option<ActiveMotion> {
    if effective_kind(input) == MotionKind::Instant {
        return None;
    }
    match trigger {
        MotionTrigger::Insert { .. } | MotionTrigger::Delete { .. } => {}
        MotionTrigger::Move { .. } => return None,
        MotionTrigger::Snap => return None,
    }
    let (Some(visual_from), Some(visual_to)) = (input.visual_from, input.visual_to) else {
        return None;
    };
    if visual_from.0 != visual_to.0 {
        return None;
    }
    let delta_cols = visual_to.1 as i32 - visual_from.1 as i32;
    if delta_cols == 0 || delta_cols.abs() > MAX_GLIDE_COLS {
        return None;
    }
    if input.char_w <= 0.0 || input.line_h <= 0.0 {
        return None;
    }
    Some(ActiveMotion {
        started_at: now,
        visual_row: visual_to.0,
        target_col: visual_to.1 as f32,
        drawn_col: carried_drawn_col.unwrap_or(visual_from.1 as f32),
        char_w: input.char_w,
        line_h: input.line_h,
        forward: delta_cols > 0,
    })
}

pub fn motion_snapshot(state: &mut CaretMotionState, now: Instant) -> MotionSnapshot {
    let Some(mut active) = state.active else {
        return MotionSnapshot::default();
    };
    let elapsed_ms = now
        .saturating_duration_since(active.started_at)
        .as_secs_f32()
        * 1000.0;
    let caret_t = (elapsed_ms / CARET_GLIDE_MS as f32).clamp(0.0, 1.0);
    if caret_t >= 1.0 {
        state.active = None;
        return MotionSnapshot::default();
    }

    let delta_px = (active.target_col - active.drawn_col) * active.char_w;
    if delta_px.abs() > CARET_CHASE_SNAP_PX {
        active.drawn_col += (active.target_col - active.drawn_col) * CARET_CHASE_FACTOR;
        let remaining_px = (active.target_col - active.drawn_col) * active.char_w;
        if remaining_px.abs() < CARET_CHASE_SNAP_PX {
            active.drawn_col = active.target_col;
        }
    } else {
        active.drawn_col = active.target_col;
    }

    let pop = 4.0 * caret_t * (1.0 - caret_t);
    let caret_dx = (active.drawn_col - active.target_col) * active.char_w;
    let forward = active.forward;
    state.active = Some(active);

    MotionSnapshot {
        caret_dx,
        caret_dy: 0.0,
        caret_scale_x: if forward {
            1.0 + 0.70 * pop
        } else {
            1.0 - 0.45 * pop
        },
        caret_scale_y: if forward {
            1.0 - 0.30 * pop
        } else {
            1.0 + 0.45 * pop
        },
        settle: None,
    }
}

pub fn inserted_char_from_text(text: &str) -> Option<char> {
    let mut chars = text.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

pub fn visual_col_for_prefix(line: &str, char_col: usize) -> usize {
    line.chars()
        .take(char_col)
        .map(|c| {
            if c.is_ascii() {
                1
            } else {
                disp_width(&c.to_string())
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use tn_config::EffectiveMotion;

    #[test]
    fn caret_motion_glides_only_for_typing_on_same_visual_row() {
        let now = Instant::now();
        let mut state = CaretMotionState::default();

        state.record(
            MotionTrigger::Insert {
                from: (2, 4),
                to: (2, 5),
                inserted: Some('x'),
            },
            now,
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: false,
                ime_active: false,
                selecting: false,
                visual_from: Some((8, 4)),
                visual_to: Some((8, 5)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
        );

        let snap = motion_snapshot(&mut state, now + Duration::from_millis(30));
        assert!(
            snap.caret_dx < 0.0,
            "caret should ease in from the previous cell"
        );
        assert!(snap.caret_scale_x > 1.0, "forward typing widens the block");
        assert!(
            snap.caret_scale_y < 1.0,
            "forward typing shortens the block slightly"
        );
        assert_eq!(snap.settle, None, "typing animation is cursor-only");
    }

    #[test]
    fn caret_motion_snaps_for_disabled_policy_ime_selection_large_and_cross_row() {
        let now = Instant::now();
        let cases = [
            CaretMotionInput {
                policy: EffectiveMotion::Instant,
                high_load: false,
                ime_active: false,
                selecting: false,
                visual_from: Some((8, 4)),
                visual_to: Some((8, 5)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: true,
                ime_active: false,
                selecting: false,
                visual_from: Some((8, 4)),
                visual_to: Some((8, 5)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: false,
                ime_active: true,
                selecting: false,
                visual_from: Some((8, 4)),
                visual_to: Some((8, 5)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: false,
                ime_active: false,
                selecting: true,
                visual_from: Some((8, 4)),
                visual_to: Some((8, 5)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: false,
                ime_active: false,
                selecting: false,
                visual_from: Some((8, 4)),
                visual_to: Some((8, 5)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: true,
            },
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: false,
                ime_active: false,
                selecting: false,
                visual_from: Some((8, 4)),
                visual_to: Some((9, 0)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
        ];

        for input in cases {
            let mut state = CaretMotionState::default();
            state.record(
                MotionTrigger::Insert {
                    from: (2, 4),
                    to: (2, 5),
                    inserted: Some('x'),
                },
                now,
                input,
            );
            assert_eq!(
                motion_snapshot(&mut state, now + Duration::from_millis(30)),
                MotionSnapshot::default()
            );
        }
    }

    #[test]
    fn helper_gates_multi_char_text_and_large_files() {
        assert_eq!(inserted_char_from_text("a"), Some('a'));
        assert_eq!(inserted_char_from_text("中"), Some('中'));
        assert_eq!(inserted_char_from_text("ab"), None);
        assert_eq!(inserted_char_from_text(""), None);
        assert!(!large_file_motion_gate(3999));
        assert!(large_file_motion_gate(4000));
    }

    #[test]
    fn caret_motion_reports_active_window_for_frame_driver() {
        let now = Instant::now();
        let mut state = CaretMotionState::default();
        state.record(
            MotionTrigger::Insert {
                from: (0, 0),
                to: (0, 1),
                inserted: Some('x'),
            },
            now,
            CaretMotionInput {
                policy: EffectiveMotion::Subtle,
                high_load: false,
                ime_active: false,
                selecting: false,
                visual_from: Some((0, 0)),
                visual_to: Some((0, 1)),
                char_w: 10.0,
                line_h: 18.0,
                large_file: false,
            },
        );

        assert!(state.is_animating(now + Duration::from_millis(30)));
        assert!(!state.is_animating(now + Duration::from_millis(CARET_GLIDE_MS + 1)));
    }

    #[test]
    fn terminal_style_motion_is_cursor_only_for_typing_and_deleting() {
        let now = Instant::now();
        let input = CaretMotionInput {
            policy: EffectiveMotion::Subtle,
            high_load: false,
            ime_active: false,
            selecting: false,
            visual_from: Some((8, 4)),
            visual_to: Some((8, 5)),
            char_w: 10.0,
            line_h: 18.0,
            large_file: false,
        };

        let mut insert = CaretMotionState::default();
        insert.record(
            MotionTrigger::Insert {
                from: (2, 4),
                to: (2, 5),
                inserted: Some('x'),
            },
            now,
            input,
        );
        let snap = motion_snapshot(&mut insert, now + Duration::from_millis(30));
        assert!(snap.caret_dx < 0.0);
        assert!(snap.caret_scale_x > 1.0);
        assert!(snap.caret_scale_y < 1.0);
        assert_eq!(
            snap.settle, None,
            "terminal-style typing has no glyph afterglow"
        );

        let mut delete = CaretMotionState::default();
        delete.record(
            MotionTrigger::Delete {
                from: (2, 5),
                to: (2, 4),
            },
            now,
            CaretMotionInput {
                visual_from: Some((8, 5)),
                visual_to: Some((8, 4)),
                ..input
            },
        );
        let snap = motion_snapshot(&mut delete, now + Duration::from_millis(30));
        assert!(snap.caret_dx > 0.0);
        assert!(snap.caret_scale_x < 1.0);
        assert!(snap.caret_scale_y > 1.0);
        assert_eq!(snap.settle, None);

        let mut nav = CaretMotionState::default();
        nav.record(
            MotionTrigger::Move {
                from: (2, 4),
                to: (2, 5),
            },
            now,
            input,
        );
        assert_eq!(
            motion_snapshot(&mut nav, now + Duration::from_millis(30)),
            MotionSnapshot::default(),
            "plain cursor movement must not animate Quick Look typing effects"
        );
    }

    #[test]
    fn terminal_chase_continues_from_drawn_position_across_rapid_typing() {
        let now = Instant::now();
        let input = CaretMotionInput {
            policy: EffectiveMotion::Subtle,
            high_load: false,
            ime_active: false,
            selecting: false,
            visual_from: Some((8, 4)),
            visual_to: Some((8, 5)),
            char_w: 10.0,
            line_h: 18.0,
            large_file: false,
        };
        let mut state = CaretMotionState::default();

        state.record(
            MotionTrigger::Insert {
                from: (2, 4),
                to: (2, 5),
                inserted: Some('x'),
            },
            now,
            input,
        );
        let first = motion_snapshot(&mut state, now + Duration::from_millis(16));
        assert!(
            (first.caret_dx - -6.0).abs() < 0.01,
            "terminal chase advances the drawn cursor 40% toward the target per frame"
        );

        state.record(
            MotionTrigger::Insert {
                from: (2, 5),
                to: (2, 6),
                inserted: Some('y'),
            },
            now + Duration::from_millis(20),
            CaretMotionInput {
                visual_from: Some((8, 5)),
                visual_to: Some((8, 6)),
                ..input
            },
        );
        let second = motion_snapshot(&mut state, now + Duration::from_millis(36));
        assert!(
            (second.caret_dx - -9.6).abs() < 0.01,
            "rapid typing should keep chasing from the in-flight drawn position"
        );
    }
}
