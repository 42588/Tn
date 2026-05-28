//! Keyboard → terminal byte encoding.
//!
//! Follows the Windows Terminal `_encodeRegular` algorithm
//! (`terminalInput.cpp`; see docs/参考资料.md §1). The single entry point is
//! [`encode_key`], whose behavior is parameterized by [`InputMode`] (DECCKM /
//! LNM, pulled live from the alacritty `Term` via `tn-core`). Four output
//! shapes: `CSI <num>;<mod> <final>`, `SS3 <final>`, a literal byte string, or
//! an Alt = `ESC`-prefixed form.
//!
//! Out of scope (gated for later): the kitty keyboard protocol, application
//! keypad (DECKPAM) numpad encoding, and win32-input-mode.

use gpui::Keystroke;
use tn_core::InputMode;

const ESC: u8 = 0x1b;

/// VT modifier parameter: bits `SHIFT=1, ALT=2, CTRL=4`, sent on the wire as
/// `bits + 1`. Returns 1 when no modifiers are held.
fn modifier_param(ks: &Keystroke) -> u8 {
    let m = &ks.modifiers;
    let mut bits = 0u8;
    if m.shift {
        bits |= 1;
    }
    if m.alt {
        bits |= 2;
    }
    if m.control {
        bits |= 4;
    }
    bits + 1
}

/// Prepend `ESC` when Alt is held (ANSI-mode meta), else return the bytes as-is.
fn with_alt(alt: bool, mut bytes: Vec<u8>) -> Vec<u8> {
    if alt {
        let mut v = Vec::with_capacity(bytes.len() + 1);
        v.push(ESC);
        v.append(&mut bytes);
        v
    } else {
        bytes
    }
}

/// A cursor / function "final byte" key: SS3 when unmodified and `ss3` is set
/// (app-cursor mode for arrows; always for F1–F4), CSI otherwise, and
/// `CSI 1;<mod><final>` when modified. Alt folds into `<mod>` — no ESC prefix.
fn csi_or_ss3(final_byte: u8, modp: u8, ss3: bool) -> Vec<u8> {
    if modp != 1 {
        let mut v = vec![ESC, b'[', b'1', b';'];
        v.extend_from_slice(modp.to_string().as_bytes());
        v.push(final_byte);
        v
    } else if ss3 {
        vec![ESC, b'O', final_byte]
    } else {
        vec![ESC, b'[', final_byte]
    }
}

/// A DECFNK-style numbered key: `ESC [ <n> ~`, or `ESC [ <n> ; <mod> ~`.
fn tilde_key(n: u16, modp: u8) -> Vec<u8> {
    let mut v = vec![ESC, b'['];
    v.extend_from_slice(n.to_string().as_bytes());
    if modp != 1 {
        v.push(b';');
        v.extend_from_slice(modp.to_string().as_bytes());
    }
    v.push(b'~');
    v
}

/// Windows Terminal `_makeCtrlChar`: map a base character under Ctrl to its C0
/// control byte. Returns `None` for characters with no control mapping.
fn ctrl_char(c: char) -> Option<u8> {
    match c {
        ' ' => Some(0x00),
        '/' => Some(0x1f),
        '?' => Some(0x7f),
        // Letters and @ [ \ ] ^ _ all collapse via & 0x1f.
        'a'..='z' | 'A'..='Z' | '@' | '[' | '\\' | ']' | '^' | '_' => Some((c as u8) & 0x1f),
        // Ctrl + digit: 2..8 → NUL, ESC, FS, GS, RS, US, DEL.
        '2' => Some(0x00),
        '3' => Some(0x1b),
        '4' => Some(0x1c),
        '5' => Some(0x1d),
        '6' => Some(0x1e),
        '7' => Some(0x1f),
        '8' => Some(0x7f),
        _ => None,
    }
}

/// The DECFNK tilde number for F5–F20 (note the skipped values), else `None`.
fn fkey_tilde(key: &str) -> Option<u16> {
    Some(match key {
        "f5" => 15,
        "f6" => 17,
        "f7" => 18,
        "f8" => 19,
        "f9" => 20,
        "f10" => 21,
        "f11" => 23,
        "f12" => 24,
        "f13" => 25,
        "f14" => 26,
        "f15" => 28,
        "f16" => 29,
        "f17" => 31,
        "f18" => 32,
        "f19" => 33,
        "f20" => 34,
        _ => return None,
    })
}

/// Encode a keystroke into the bytes the terminal should send to the PTY, or
/// `None` if the key produces no output (or is reserved for the Tn UI).
pub fn encode_key(ks: &Keystroke, mode: InputMode) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let key = ks.key.as_str();
    let alt = m.alt;

    // Ctrl+Shift+* is reserved for Tn UI shortcuts; never sent to the shell.
    if m.control && m.shift {
        return None;
    }
    // Windows / super combos aren't terminal input.
    if m.platform {
        return None;
    }

    let modp = modifier_param(ks);

    // Arrows + Home/End: CSI/SS3 per DECCKM; modified → CSI 1;<mod>.
    let cursor_final = match key {
        "up" => Some(b'A'),
        "down" => Some(b'B'),
        "right" => Some(b'C'),
        "left" => Some(b'D'),
        "home" => Some(b'H'),
        "end" => Some(b'F'),
        _ => None,
    };
    if let Some(fb) = cursor_final {
        return Some(csi_or_ss3(fb, modp, mode.app_cursor));
    }

    // F1–F4: SS3 unmodified, CSI 1;<mod> modified (not gated by app-cursor).
    let f1_4 = match key {
        "f1" => Some(b'P'),
        "f2" => Some(b'Q'),
        "f3" => Some(b'R'),
        "f4" => Some(b'S'),
        _ => None,
    };
    if let Some(fb) = f1_4 {
        return Some(csi_or_ss3(fb, modp, true));
    }

    // F5–F20: DECFNK ESC[n~.
    if let Some(n) = fkey_tilde(key) {
        return Some(tilde_key(n, modp));
    }

    // Insert / Delete / PageUp / PageDown: ESC[n~.
    let edit_n = match key {
        "insert" => Some(2u16),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        _ => None,
    };
    if let Some(n) = edit_n {
        return Some(tilde_key(n, modp));
    }

    match key {
        // DEL by default; Ctrl flips to BS. Alt prefixes ESC.
        "backspace" => return Some(with_alt(alt, vec![if m.control { 0x08 } else { 0x7f }])),
        "tab" => {
            if m.control {
                return None; // Ctrl+Tab switches tabs (handled by the workspace)
            }
            if m.shift {
                return Some(vec![ESC, b'[', b'Z']); // CBT (back-tab)
            }
            return Some(with_alt(alt, vec![b'\t']));
        }
        "enter" => {
            let base = if m.control {
                vec![b'\n'] // Ctrl+Enter → LF
            } else if mode.line_feed_newline {
                vec![b'\r', b'\n'] // LNM
            } else {
                vec![b'\r']
            };
            return Some(with_alt(alt, base));
        }
        "escape" => return Some(with_alt(alt, vec![ESC])),
        "space" => {
            return Some(with_alt(alt, vec![if m.control { 0x00 } else { b' ' }]));
        }
        _ => {}
    }

    // Ctrl + single character → C0 control byte (_makeCtrlChar).
    if m.control && key.chars().count() == 1 {
        if let Some(b) = ctrl_char(key.chars().next().unwrap()) {
            return Some(with_alt(alt, vec![b]));
        }
    }

    // Printable character (shift/layout already resolved into key_char).
    if !m.control {
        if let Some(kc) = &ks.key_char {
            if !kc.is_empty() {
                return Some(with_alt(alt, kc.clone().into_bytes()));
            }
        }
        if key.chars().count() == 1 {
            return Some(with_alt(alt, key.as_bytes().to_vec()));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    /// Build a keystroke fixture.
    fn ks(key: &str, control: bool, alt: bool, shift: bool, key_char: Option<&str>) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                control,
                alt,
                shift,
                platform: false,
                function: false,
            },
            key: key.to_string(),
            key_char: key_char.map(str::to_string),
        }
    }

    fn off() -> InputMode {
        InputMode::default()
    }

    #[test]
    fn arrows_csi_when_app_cursor_off() {
        assert_eq!(encode_key(&ks("up", false, false, false, None), off()), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode_key(&ks("left", false, false, false, None), off()), Some(b"\x1b[D".to_vec()));
    }

    #[test]
    fn arrows_ss3_when_app_cursor_on() {
        let app = InputMode { app_cursor: true, ..off() };
        assert_eq!(encode_key(&ks("up", false, false, false, None), app), Some(b"\x1bOA".to_vec()));
        assert_eq!(encode_key(&ks("end", false, false, false, None), app), Some(b"\x1bOF".to_vec()));
    }

    #[test]
    fn modified_arrow_uses_csi_mod_param() {
        // Ctrl+Right → mod bits 4 → param 5. App-cursor must not matter.
        let app = InputMode { app_cursor: true, ..off() };
        assert_eq!(encode_key(&ks("right", true, false, false, None), app), Some(b"\x1b[1;5C".to_vec()));
        // Alt+Up folds into the mod param (2 → 3), no ESC prefix.
        assert_eq!(encode_key(&ks("up", false, true, false, None), off()), Some(b"\x1b[1;3A".to_vec()));
        // Shift+Down → param 2.
        assert_eq!(encode_key(&ks("down", false, false, true, None), off()), Some(b"\x1b[1;2B".to_vec()));
    }

    #[test]
    fn function_keys() {
        assert_eq!(encode_key(&ks("f1", false, false, false, None), off()), Some(b"\x1bOP".to_vec()));
        assert_eq!(encode_key(&ks("f4", false, false, true, None), off()), Some(b"\x1b[1;2S".to_vec()));
        assert_eq!(encode_key(&ks("f5", false, false, false, None), off()), Some(b"\x1b[15~".to_vec()));
        assert_eq!(encode_key(&ks("f6", false, false, false, None), off()), Some(b"\x1b[17~".to_vec()));
        assert_eq!(encode_key(&ks("f12", false, false, false, None), off()), Some(b"\x1b[24~".to_vec()));
        // Modified DECFNK: Shift+F5 → ESC[15;2~.
        assert_eq!(encode_key(&ks("f5", false, false, true, None), off()), Some(b"\x1b[15;2~".to_vec()));
    }

    #[test]
    fn editing_keys() {
        assert_eq!(encode_key(&ks("insert", false, false, false, None), off()), Some(b"\x1b[2~".to_vec()));
        assert_eq!(encode_key(&ks("delete", false, false, false, None), off()), Some(b"\x1b[3~".to_vec()));
        assert_eq!(encode_key(&ks("pageup", false, false, false, None), off()), Some(b"\x1b[5~".to_vec()));
        assert_eq!(encode_key(&ks("pagedown", true, false, false, None), off()), Some(b"\x1b[6;5~".to_vec()));
    }

    #[test]
    fn backspace_and_tab() {
        assert_eq!(encode_key(&ks("backspace", false, false, false, None), off()), Some(vec![0x7f]));
        assert_eq!(encode_key(&ks("backspace", true, false, false, None), off()), Some(vec![0x08]));
        assert_eq!(encode_key(&ks("backspace", false, true, false, None), off()), Some(vec![ESC, 0x7f]));
        assert_eq!(encode_key(&ks("tab", false, false, false, None), off()), Some(vec![b'\t']));
        assert_eq!(encode_key(&ks("tab", false, false, true, None), off()), Some(b"\x1b[Z".to_vec()));
        assert_eq!(encode_key(&ks("tab", true, false, false, None), off()), None); // Ctrl+Tab reserved
    }

    #[test]
    fn enter_modes() {
        assert_eq!(encode_key(&ks("enter", false, false, false, None), off()), Some(vec![b'\r']));
        let lnm = InputMode { line_feed_newline: true, ..off() };
        assert_eq!(encode_key(&ks("enter", false, false, false, None), lnm), Some(b"\r\n".to_vec()));
        assert_eq!(encode_key(&ks("enter", true, false, false, None), off()), Some(vec![b'\n']));
        assert_eq!(encode_key(&ks("enter", false, true, false, None), off()), Some(vec![ESC, b'\r']));
    }

    #[test]
    fn ctrl_chars() {
        assert_eq!(encode_key(&ks("c", true, false, false, None), off()), Some(vec![0x03])); // Ctrl+C
        assert_eq!(encode_key(&ks("a", true, false, false, None), off()), Some(vec![0x01]));
        assert_eq!(encode_key(&ks("[", true, false, false, None), off()), Some(vec![0x1b])); // Ctrl+[
        assert_eq!(encode_key(&ks("space", true, false, false, None), off()), Some(vec![0x00]));
        assert_eq!(encode_key(&ks("2", true, false, false, None), off()), Some(vec![0x00]));
        assert_eq!(encode_key(&ks("8", true, false, false, None), off()), Some(vec![0x7f]));
        // Alt+Ctrl+C → ESC then 0x03.
        assert_eq!(encode_key(&ks("c", true, true, false, None), off()), Some(vec![ESC, 0x03]));
    }

    #[test]
    fn printable_and_alt_prefix() {
        assert_eq!(encode_key(&ks("a", false, false, false, Some("a")), off()), Some(b"a".to_vec()));
        // Shift resolves into key_char.
        assert_eq!(encode_key(&ks("a", false, false, true, Some("A")), off()), Some(b"A".to_vec()));
        // Alt+letter → ESC prefix.
        assert_eq!(encode_key(&ks("b", false, true, false, Some("b")), off()), Some(vec![ESC, b'b']));
        // UTF-8 key_char passes through.
        assert_eq!(encode_key(&ks("s", false, true, false, Some("ß")), off()), {
            let mut v = vec![ESC];
            v.extend_from_slice("ß".as_bytes());
            Some(v)
        });
    }

    #[test]
    fn reserved_and_unmapped() {
        assert_eq!(encode_key(&ks("t", true, false, true, None), off()), None); // Ctrl+Shift+T
        assert_eq!(encode_key(&ks("capslock", false, false, false, None), off()), None); // no mapping
        // A bare single-char key still falls back to the literal byte.
        assert_eq!(encode_key(&ks("z", false, false, false, None), off()), Some(b"z".to_vec()));
    }

    /// Every key Tn knows about, for the no-panic sweep below.
    fn sweep_keys() -> Vec<String> {
        let mut keys: Vec<String> = [
            "up", "down", "left", "right", "home", "end", "insert", "delete", "pageup",
            "pagedown", "backspace", "tab", "enter", "escape", "space", // handled specials
            "capslock", "menu", "", "f0", "scroll-lock", "weird-key", // unhandled / edge
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for n in 1..=24 {
            keys.push(format!("f{n}"));
        }
        for c in "abyz09@#[]\\/;'`~!.".chars() {
            keys.push(c.to_string());
        }
        keys
    }

    #[test]
    fn encode_key_never_panics_or_returns_empty() {
        // 待优化清单 §7.5: sweep key × modifiers × mode. `encode_key` must never
        // panic (the test would crash) and a `Some` result must be a NON-empty
        // byte sequence — encoding a handled key to zero bytes would silently
        // swallow the keystroke. Guards the many branches without a proptest dep.
        let modes = [
            InputMode::default(),
            InputMode { app_cursor: true, ..InputMode::default() },
            InputMode { line_feed_newline: true, ..InputMode::default() },
            InputMode {
                app_cursor: true,
                line_feed_newline: true,
                app_keypad: true,
                bracketed_paste: true,
                alt_screen: true,
            },
        ];
        let mut produced = 0usize;
        for key in sweep_keys() {
            let single = key.chars().count() == 1;
            for control in [false, true] {
                for alt in [false, true] {
                    for shift in [false, true] {
                        // Mirror real input: a printable single char carries key_char.
                        let kc = if single && !control { Some(key.as_str()) } else { None };
                        let k = ks(&key, control, alt, shift, kc);
                        for mode in modes {
                            if let Some(bytes) = encode_key(&k, mode) {
                                assert!(
                                    !bytes.is_empty(),
                                    "empty encoding: key={key:?} ctrl={control} alt={alt} shift={shift}"
                                );
                                produced += 1;
                            }
                        }
                    }
                }
            }
        }
        assert!(produced > 0, "the sweep should have produced encodings");
    }

    #[test]
    fn ctrl_shift_and_platform_are_never_sent() {
        // Ctrl+Shift+* is reserved for Tn UI shortcuts; Win/super combos aren't
        // terminal input. Both must yield None for any key.
        for key in ["a", "up", "enter", "f5", "tab"] {
            assert!(
                encode_key(&ks(key, true, false, true, None), off()).is_none(),
                "Ctrl+Shift+{key} must be reserved"
            );
            let mut k = ks(key, false, false, false, Some(key));
            k.modifiers.platform = true;
            assert!(encode_key(&k, off()).is_none(), "Win+{key} is not terminal input");
        }
    }
}
