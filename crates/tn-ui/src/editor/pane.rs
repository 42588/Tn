use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    canvas, div, prelude::*, px, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    EventEmitter, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, SharedString,
    UTF16Selection, Window,
};
use tn_config::Loaded;

use crate::editor::session::DocumentSession;
use crate::quick_look::EditorHandoff;
use crate::style::{col, cola, icon, UI_SANS};

const EDITOR_ROW_H: f32 = 20.0;
const EDITOR_CODE_FS: f32 = 12.5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorStatus {
    pub line: usize,
    pub column: usize,
    pub dirty: bool,
    pub encoding: &'static str,
    pub newline: &'static str,
}

impl EditorStatus {
    fn from_session(session: &DocumentSession) -> Self {
        let (row, col) = session.cursor();
        Self {
            line: row + 1,
            column: col + 1,
            dirty: session.is_dirty(),
            encoding: "UTF-8",
            newline: "LF",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorCloseDecision {
    Close,
    ConfirmDirty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCloseIntent {
    Pane,
    Tab(usize),
    App,
}

impl EditorCloseIntent {
    fn message(self) -> &'static str {
        match self {
            Self::Pane => "关闭编辑器前保存更改？",
            Self::Tab(_) => "关闭标签前保存编辑器更改？",
            Self::App => "退出 Tn 前保存编辑器更改？",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorClosePrompt {
    intent: Option<EditorCloseIntent>,
}

impl EditorClosePrompt {
    pub fn intent(&self) -> Option<EditorCloseIntent> {
        self.intent
    }

    pub fn message(&self) -> Option<&'static str> {
        self.intent.map(EditorCloseIntent::message)
    }

    fn set(&mut self, intent: EditorCloseIntent) {
        self.intent = Some(intent);
    }

    fn clear(&mut self) {
        self.intent = None;
    }
}

pub fn request_editor_close(
    session: &DocumentSession,
    prompt: &mut EditorClosePrompt,
    intent: EditorCloseIntent,
) -> EditorCloseDecision {
    if session.is_dirty() {
        prompt.set(intent);
        EditorCloseDecision::ConfirmDirty
    } else {
        prompt.clear();
        EditorCloseDecision::Close
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorPaneEvent {
    CloseConfirmed(EditorCloseIntent),
}

pub struct EditorPane {
    session: DocumentSession,
    path: Option<PathBuf>,
    title: String,
    config: Arc<Loaded>,
    focus_handle: FocusHandle,
    last_error: Option<String>,
    close_prompt: EditorClosePrompt,
}

impl EditorPane {
    pub fn new(handoff: EditorHandoff, config: Arc<Loaded>, cx: &mut Context<Self>) -> Self {
        Self {
            session: handoff.session,
            path: handoff.path,
            title: handoff.title,
            config,
            focus_handle: cx.focus_handle(),
            last_error: None,
            close_prompt: EditorClosePrompt::default(),
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn tab_label(&self) -> String {
        if self.session.is_dirty() {
            format!("{} ●", self.title)
        } else {
            self.title.clone()
        }
    }

    pub fn session(&self) -> DocumentSession {
        self.session.clone()
    }

    pub fn status(&self) -> EditorStatus {
        EditorStatus::from_session(&self.session)
    }

    pub fn close_decision(&self) -> EditorCloseDecision {
        if self.session.is_dirty() {
            EditorCloseDecision::ConfirmDirty
        } else {
            EditorCloseDecision::Close
        }
    }

    pub fn request_close(&mut self, intent: EditorCloseIntent) -> EditorCloseDecision {
        request_editor_close(&self.session, &mut self.close_prompt, intent)
    }

    pub fn close_prompt(&self) -> &EditorClosePrompt {
        &self.close_prompt
    }

    pub fn save(&mut self) -> Result<(), String> {
        let Some(path) = self.path.clone() else {
            let err = "当前编辑器没有本地保存路径".to_string();
            self.last_error = Some(err.clone());
            return Err(err);
        };
        let lines = self.session.lines();
        let text = lines.borrow().join("\n");
        std::fs::write(&path, text).map_err(|err| err.to_string())?;
        self.session.mark_clean();
        self.last_error = None;
        Ok(())
    }

    fn save_pending_close(&mut self, cx: &mut Context<Self>) {
        let Some(intent) = self.close_prompt.intent() else {
            return;
        };
        if self.save().is_ok() {
            self.close_prompt.clear();
            cx.emit(EditorPaneEvent::CloseConfirmed(intent));
        }
        cx.notify();
    }

    fn discard_pending_close(&mut self, cx: &mut Context<Self>) {
        let Some(intent) = self.close_prompt.intent() else {
            return;
        };
        self.close_prompt.clear();
        cx.emit(EditorPaneEvent::CloseConfirmed(intent));
        cx.notify();
    }

    fn cancel_pending_close(&mut self, cx: &mut Context<Self>) {
        self.close_prompt.clear();
        cx.notify();
    }

    fn type_text(&mut self, text: &str) {
        self.session.type_text(text);
    }

    fn newline(&mut self) {
        self.session.newline();
    }

    fn backspace(&mut self) {
        self.session.backspace();
    }

    fn delete_forward(&mut self) {
        self.session.delete_forward();
    }

    fn move_cursor(&mut self, key: &str, extend: bool) {
        self.session.move_cursor(key, extend);
    }

    fn select_all(&mut self) {
        self.session.select_all();
    }

    fn copy(&self, cx: &mut Context<Self>) {
        if let Some(text) = self.session.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.session
            .insert_text(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }
}

impl EntityInputHandler for EditorPane {
    fn text_for_range(
        &mut self,
        _range: std::ops::Range<usize>,
        _actual_range: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        Some(String::new())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        None
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {}

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !text.is_empty() {
            self.type_text(text);
            cx.notify();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !new_text.is_empty() {
            self.type_text(new_text);
            cx.notify();
        }
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        _element_bounds: gpui::Bounds<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        None
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<gpui::Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for EditorPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.session.lines();
        let lines = ui.borrow().clone();
        let th = self.config.theme.clone();
        let status = self.status();
        let focus = self.focus_handle.clone();
        let input_focus = self.focus_handle.clone();
        let input_entity = cx.entity().clone();
        let title = self.tab_label();
        let error = self.last_error.clone();
        let close_notice = self.close_prompt.message().map(|message| {
            let button = |label: &'static str, primary: bool| {
                div()
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if primary {
                        cola(th.ui.accent, 0.52)
                    } else {
                        cola(th.ui.foreground, 0.14)
                    })
                    .bg(if primary {
                        cola(th.ui.accent, 0.18)
                    } else {
                        cola(th.ui.surface_2, 0.52)
                    })
                    .text_color(if primary {
                        col(th.ui.foreground)
                    } else {
                        col(th.ui.muted)
                    })
                    .child(label)
            };
            div()
                .px(px(12.))
                .py(px(7.))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .bg(cola(th.ansi.yellow, 0.12))
                .border_b_1()
                .border_color(cola(th.ansi.yellow, 0.20))
                .font_family(UI_SANS)
                .text_size(px(11.))
                .text_color(col(th.ui.foreground))
                .child(SharedString::from(message))
                .child(div().flex_1())
                .child(button("保存", false).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                        this.save_pending_close(cx);
                    }),
                ))
                .child(button("放弃", true).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                        this.discard_pending_close(cx);
                    }),
                ))
                .child(button("取消", false).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _ev: &MouseDownEvent, _window, cx| {
                        this.cancel_pending_close(cx);
                    }),
                ))
        });

        div()
            .key_context("EditorPane")
            .track_focus(&focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _window, cx| {
                let m = &ev.keystroke.modifiers;
                let key = ev.keystroke.key.as_str();
                let mut handled = true;
                if m.control || m.platform {
                    match key {
                        "s" => {
                            let _ = this.save();
                        }
                        "z" if m.shift => {
                            this.session.redo();
                        }
                        "z" => {
                            this.session.undo();
                        }
                        "y" => {
                            this.session.redo();
                        }
                        "a" => this.select_all(),
                        "c" => this.copy(cx),
                        "v" => this.paste(cx),
                        _ => handled = false,
                    }
                } else {
                    match key {
                        "enter" => this.newline(),
                        "backspace" => this.backspace(),
                        "delete" => this.delete_forward(),
                        "left" | "right" | "up" | "down" | "home" | "end" => {
                            this.move_cursor(key, m.shift)
                        }
                        _ => handled = false,
                    }
                }
                if handled {
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .flex()
            .flex_col()
            .size_full()
            .relative()
            .bg(cola(th.ui.surface_1, 0.76))
            .child(
                canvas(
                    |_bounds, _window, _cx| {},
                    move |bounds, _state, window, cx| {
                        window.handle_input(
                            &input_focus,
                            ElementInputHandler::new(bounds, input_entity.clone()),
                            cx,
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .h(px(34.))
                    .px(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(cola(th.ui.foreground, 0.08))
                    .font_family(UI_SANS)
                    .text_size(px(11.5))
                    .text_color(col(th.ui.foreground))
                    .child(icon("file-edit", 14., th.ui.accent))
                    .child(SharedString::from(title))
                    .child(div().flex_1())
                    .when(status.dirty, |d| {
                        d.child(
                            div()
                                .px(px(7.))
                                .py(px(2.))
                                .rounded(px(5.))
                                .bg(cola(th.ansi.yellow, 0.14))
                                .text_color(col(th.ansi.yellow))
                                .child("未保存"),
                        )
                    }),
            )
            .when_some(close_notice, |d, notice| d.child(notice))
            .child(
                div()
                    .flex_1()
                    .font_family(SharedString::from("JetBrains Mono"))
                    .text_size(px(EDITOR_CODE_FS))
                    .children(lines.iter().enumerate().map(|(idx, line)| {
                        let row_no = format!("{:>4}", idx + 1);
                        div()
                            .h(px(EDITOR_ROW_H))
                            .px(px(10.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(10.))
                            .when(idx + 1 == status.line, |d| d.bg(cola(th.ui.accent, 0.08)))
                            .child(
                                div()
                                    .w(px(36.))
                                    .text_color(col(th.ui.muted))
                                    .child(SharedString::from(row_no)),
                            )
                            .child(
                                div()
                                    .text_color(col(th.ui.foreground))
                                    .child(SharedString::from(line.clone())),
                            )
                    })),
            )
            .when_some(error, |d, error| {
                d.child(
                    div()
                        .px(px(12.))
                        .py(px(5.))
                        .bg(cola(th.ansi.red, 0.12))
                        .text_color(col(th.ansi.red))
                        .child(SharedString::from(error)),
                )
            })
            .child(
                div()
                    .h(px(28.))
                    .px(px(12.))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .border_t_1()
                    .border_color(cola(th.ui.foreground, 0.08))
                    .font_family(UI_SANS)
                    .text_size(px(10.5))
                    .text_color(col(th.ui.muted))
                    .child(SharedString::from(format!(
                        "Ln {}, Col {}",
                        status.line, status.column
                    )))
                    .child("UTF-8")
                    .child("LF")
                    .when(status.dirty, |d| d.child("●")),
            )
    }
}

impl EventEmitter<EditorPaneEvent> for EditorPane {}

impl gpui::Focusable for EditorPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn status_tracks_shared_session_cursor_and_dirty_state() {
        let session = DocumentSession::from_lines(lines(&["abc"]));

        session.place_cursor(0, 1, false);
        session.type_text("X");

        let status = EditorStatus::from_session(&session);

        assert_eq!(status.line, 1);
        assert_eq!(status.column, 3);
        assert!(status.dirty);
        assert_eq!(status.encoding, "UTF-8");
        assert_eq!(status.newline, "LF");

        session.mark_clean();

        assert_eq!(EditorStatus::from_session(&session).dirty, false);
    }

    #[test]
    fn dirty_session_requires_close_confirmation() {
        let session = DocumentSession::from_lines(lines(&["abc"]));
        session.type_text("X");

        assert_eq!(
            if session.is_dirty() {
                EditorCloseDecision::ConfirmDirty
            } else {
                EditorCloseDecision::Close
            },
            EditorCloseDecision::ConfirmDirty
        );

        session.mark_clean();

        assert_eq!(
            if session.is_dirty() {
                EditorCloseDecision::ConfirmDirty
            } else {
                EditorCloseDecision::Close
            },
            EditorCloseDecision::Close
        );
    }

    #[test]
    fn dirty_close_request_opens_visible_prompt_state() {
        let session = DocumentSession::from_lines(lines(&["abc"]));
        session.type_text("X");

        let mut close_prompt = EditorClosePrompt::default();
        let decision = request_editor_close(&session, &mut close_prompt, EditorCloseIntent::Pane);

        assert_eq!(decision, EditorCloseDecision::ConfirmDirty);
        assert_eq!(close_prompt.intent(), Some(EditorCloseIntent::Pane));
        assert_eq!(close_prompt.message(), Some("关闭编辑器前保存更改？"));
    }

    #[test]
    fn dirty_close_prompt_matches_close_intent() {
        let session = DocumentSession::from_lines(lines(&["abc"]));
        session.type_text("X");
        let mut close_prompt = EditorClosePrompt::default();

        request_editor_close(&session, &mut close_prompt, EditorCloseIntent::Tab(2));
        assert_eq!(close_prompt.message(), Some("关闭标签前保存编辑器更改？"));

        request_editor_close(&session, &mut close_prompt, EditorCloseIntent::App);
        assert_eq!(close_prompt.message(), Some("退出 Tn 前保存编辑器更改？"));
    }
}
