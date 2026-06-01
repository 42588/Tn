//! Win32 glue for the Quick Terminal (M5): a global-hotkey listener thread plus
//! the topmost / borderless window manipulation that gpui 0.2.2 doesn't expose.
//!
//! gpui's `WindowKind::PopUp` already gives us a borderless, no-taskbar window
//! (`WS_EX_TOOLWINDOW`), but not always-on-top and no programmatic move/show. We
//! reach past gpui via the raw HWND for exactly those: set `WS_EX_TOPMOST`, slide
//! the window with `SetWindowPos`, and pump a private message loop for a global
//! `RegisterHotKey`. All positioning is in **physical pixels** (what `SetWindowPos`
//! and `GetMonitorInfoW` speak), which is why the [`tn_config`] geometry is
//! unit-agnostic. Windows only; a no-op stub keeps non-Windows builds compiling.

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;

    use futures::channel::mpsc::{self, UnboundedReceiver};
    use tn_config::{HotkeySpec, Rect};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        CreateRoundRectRgn, GetMonitorInfoW, MonitorFromWindow, SetWindowRgn, HRGN, MONITORINFO,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
        VK_PROCESSKEY,
    };
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
        ShowWindow, TranslateMessage, GWL_EXSTYLE, HWND_TOPMOST, MSG, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SW_HIDE, SW_SHOW, WM_HOTKEY, WM_KEYDOWN, WS_EX_TOPMOST,
    };

    fn as_hwnd(h: isize) -> HWND {
        HWND(h as *mut c_void)
    }

    /// Translate a parsed hotkey to a Win32 (modifier mask, virtual-key) pair.
    /// `MOD_NOREPEAT` suppresses auto-repeat while the chord is held. Returns
    /// `None` for an unmappable key token.
    pub fn to_win32(spec: &HotkeySpec) -> Option<(HOT_KEY_MODIFIERS, u32)> {
        let mut mods = MOD_NOREPEAT;
        if spec.ctrl {
            mods |= MOD_CONTROL;
        }
        if spec.alt {
            mods |= MOD_ALT;
        }
        if spec.shift {
            mods |= MOD_SHIFT;
        }
        if spec.win {
            mods |= MOD_WIN;
        }
        Some((mods, vk_for(&spec.key)?))
    }

    /// Map a (lowercased) key token to a Windows virtual-key code.
    fn vk_for(key: &str) -> Option<u32> {
        let b = key.as_bytes();
        if b.len() == 1 {
            return match b[0] {
                c @ b'a'..=b'z' => Some((c - b'a' + 0x41) as u32), // VK_A..VK_Z
                c @ b'0'..=b'9' => Some(c as u32),                  // VK_0..VK_9
                b'`' => Some(0xC0),                                 // VK_OEM_3
                b'-' => Some(0xBD),                                 // VK_OEM_MINUS
                b'=' => Some(0xBB),                                 // VK_OEM_PLUS
                _ => None,
            };
        }
        match key {
            "space" => Some(0x20),
            "tab" => Some(0x09),
            "enter" | "return" => Some(0x0D),
            "esc" | "escape" => Some(0x1B),
            "grave" | "backtick" => Some(0xC0),
            "minus" => Some(0xBD),
            f if f.starts_with('f') => {
                let n: u32 = f[1..].parse().ok()?;
                (1..=24).contains(&n).then_some(0x70 + (n - 1)) // VK_F1..VK_F24
            }
            _ => None,
        }
    }

    /// Spawn a dedicated thread that registers the global hotkey (`hwnd = None`,
    /// so `WM_HOTKEY` is posted to this thread's queue) and pumps its message
    /// loop, sending `()` on each press. Returns the receiver, or `None` if the
    /// key is unmappable. The OS unregisters the hotkey when the process exits.
    pub fn spawn_hotkey_listener(spec: &HotkeySpec) -> Option<UnboundedReceiver<()>> {
        let (mods, vk) = to_win32(spec)?;
        let (tx, rx) = mpsc::unbounded::<()>();
        std::thread::Builder::new()
            .name("tn-quick-hotkey".into())
            .spawn(move || unsafe {
                if RegisterHotKey(None, 1, mods, vk).is_err() {
                    return; // hotkey likely owned by another app
                }
                let mut msg = MSG::default();
                // GetMessageW: >0 normal, 0 on WM_QUIT, -1 on error.
                while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                    if msg.message == WM_HOTKEY && tx.unbounded_send(()).is_err() {
                        break; // receiver dropped — app is gone
                    }
                }
            })
            .ok();
        Some(rx)
    }

    /// Make the window an always-on-top overlay (called once, on first reveal).
    /// gpui's PopUp is already borderless + off-taskbar; this adds topmost.
    pub fn make_topmost(h: isize) {
        let h = as_hwnd(h);
        unsafe {
            let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_TOPMOST.0 as isize);
            let _ = SetWindowPos(
                h,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOSIZE | SWP_NOMOVE | SWP_NOACTIVATE,
            );
        }
    }

    /// Move + resize the window (physical px), keeping it pinned topmost.
    pub fn set_bounds(h: isize, r: Rect) {
        unsafe {
            let _ = SetWindowPos(
                as_hwnd(h),
                Some(HWND_TOPMOST),
                r.x as i32,
                r.y as i32,
                r.width as i32,
                r.height as i32,
                SWP_NOACTIVATE,
            );
        }
    }

    /// Clip the window to a rounded rectangle (physical px size + corner radius), so a
    /// borderless **transparent** window reads as a clean rounded card instead of the
    /// OS's square window rectangle showing past the card's rounded corners. The gpui
    /// quad shader already rounds the card's fill, but the window itself is a sharp
    /// rectangle (Win10 doesn't round borderless windows), so its corners can peek;
    /// `SetWindowRgn` makes the *window shape* rounded to match. The system takes
    /// ownership of the region handle (do NOT delete it). Size-relative, so it survives
    /// the slide (which only moves the window); re-set it whenever the size changes.
    pub fn set_round_region(h: isize, w: f32, ht: f32, radius: f32) {
        unsafe {
            // CreateRoundRectRgn's right/bottom edges are exclusive → +1 to cover the
            // last column/row. Ellipse diameter = 2×radius.
            let d = (radius * 2.0).round() as i32;
            let rgn = CreateRoundRectRgn(0, 0, w as i32 + 1, ht as i32 + 1, d, d);
            if !rgn.is_invalid() {
                // redraw = true. System owns `rgn` after this; we don't free it.
                let _ = SetWindowRgn(as_hwnd(h), Some(rgn), true);
            }
        }
    }

    /// Drop any window region → back to a plain rectangle (a running session fills the
    /// drop-down edge-to-edge; only the launcher card wants rounding).
    pub fn clear_region(h: isize) {
        unsafe {
            let _ = SetWindowRgn(as_hwnd(h), None::<HRGN>, true);
        }
    }

    /// Show or hide the window. Showing also pulls it to the foreground so the
    /// user can type immediately (the hotkey press grants us foreground rights).
    pub fn show(h: isize, visible: bool) {
        let h = as_hwnd(h);
        unsafe {
            let _ = ShowWindow(h, if visible { SW_SHOW } else { SW_HIDE });
            if visible {
                let _ = SetForegroundWindow(h);
            }
        }
    }

    /// DPI scale factor (1.0 = 96 DPI / 100%) of the monitor the window sits on.
    /// `set_bounds`/`work_area` speak physical px, but gpui lays out content in
    /// **logical** px — so the launcher card (sized in logical px) must scale its
    /// window bounds up by this, or it clips on a HiDPI display. `GetDpiForWindow`
    /// (Win10 1607+) reads the window's per-monitor DPI directly, so it's right on
    /// the very first summon (no render needed) and tracks cross-monitor DPI.
    pub fn scale_for(h: isize) -> f32 {
        unsafe {
            match GetDpiForWindow(as_hwnd(h)) {
                0 => 1.0, // invalid window — assume 100%
                dpi => dpi as f32 / 96.0,
            }
        }
    }

    /// The work area (monitor minus taskbar, physical px) of the monitor the
    /// window currently sits on.
    pub fn work_area(h: isize) -> Option<Rect> {
        unsafe {
            let mon = MonitorFromWindow(as_hwnd(h), MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(mon, &mut mi).as_bool() {
                let w = mi.rcWork;
                Some(Rect::new(
                    w.left as f32,
                    w.top as f32,
                    (w.right - w.left) as f32,
                    (w.bottom - w.top) as f32,
                ))
            } else {
                None
            }
        }
    }

    /// Play the system default sound for a terminal bell (BEL / `\x07`), when the
    /// user opts into `[appearance].audio_bell`. `MessageBeep(MB_OK)` is async,
    /// non-blocking, and respects the user's sound scheme (silent if they muted
    /// the "Default Beep" event). 待优化清单 §3.8.
    pub fn system_beep() {
        use windows::Win32::System::Diagnostics::Debug::MessageBeep;
        use windows::Win32::UI::WindowsAndMessaging::MB_OK;
        unsafe {
            let _ = MessageBeep(MB_OK);
        }
    }

    // ── IME key-routing fix (中文输入「输入法被限制」根治) ──────────────────────
    //
    // 病根在 gpui 0.2.2 的 Windows 文本输入走**旧的 IMM32** 而非 WT 用的 **TSF**:
    //   • gpui 判定「是否在合成」只看 `marked_text_range()`(= 应用的 marked text),
    //     而它仅在 IME 发 `GCS_COMPSTR` 时才被置位;**微软拼音等 TSF 原生输入法在
    //     IMM32 兼容层下从不发 `GCS_COMPSTR`**(自画候选窗、只在提交发 `GCS_RESULTSTR`)
    //     → gpui 的 `is_composing` **恒 false** → 它**从不把按键短路给 IME**,每个键都先
    //     派发到我们的 `on_key`(实证见 tn.log:`marked_text_range -> None`)。
    //   • gpui 只在**应用不消费该 keydown 时**才调 `translate_message` 把键交给 IME;且
    //     它的 `parse_char_message` 会**过滤掉控制字符**(退格 0x08/回车 0x0D/Esc 0x1B/Tab
    //     0x09 的 WM_CHAR 全被丢)→ 这些命名键既不能「放行」(WM_CHAR 被吞、键彻底丢失)、
    //     又必须在 `on_key` 里**编码送 PTY**(否则终端删不掉字)。结果:**合成期按退格/回车/
    //     方向键会被终端抢走**,无法删拼音、无法回车提交、无法翻候选页 = 用户感到的「受限」。
    //
    // 关键事实:**当 IME 正在处理某键(用户在合成)时,系统投递的 `WM_KEYDOWN` 其虚拟键
    // 是 `VK_PROCESSKEY`(0xE5)**。这是 OS 明确告诉我们「这个键属于 IME」。但 gpui 在
    // `handle_key_event` 里**悄悄用 `ImmGetVirtualKey` 还原出真实键**再派发给应用,所以到
    // `on_key` 这层已分不清「合成中的退格」和「终端用的退格」。
    //
    // 修法(无需 fork gpui,IME 无关):在 gpui 的 wndproc **之前**子类化窗口,凡 `WM_KEYDOWN`
    // 且 `wParam == VK_PROCESSKEY` 的键就是 IME 的——我们替它 `TranslateMessage`(驱动
    // `WM_IME_COMPOSITION` → gpui 的 `GCS_RESULTSTR` 提交链 → 我们的 `replace_text` 写 PTY)
    // 并**消费掉**(返回 0,不再下传 gpui),于是 gpui 永不会误编码它。IME **不**要的键以其
    // 真实虚拟键到达(非 VK_PROCESSKEY)→ 原样透传给 gpui → `on_key` 照常编码送终端。
    // 这样合成期的退格/回车/方向键/数字/空格全部回到 IME(删拼音、翻页、提交都正常)。
    //
    // 重入安全:子类回调在 gpui wndproc **之前**于消息派发链最前端运行,`TranslateMessage`
    // 只**投递** WM_IME_*/WM_CHAR 到队列(不同步回调 wndproc),且我们对 VK_PROCESSKEY
    // 直接 `return 0`(不调 `DefSubclassProc` → 不进 gpui wndproc)→ 不触碰 gpui 的 RefCell
    // 窗口状态,无 M5「外部调用重入借用」之忧。

    const TN_IME_SUBCLASS_ID: usize = 0x746E_0001; // "tn" + tag

    unsafe extern "system" fn ime_subclass_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _uid_subclass: usize,
        _dwref_data: usize,
    ) -> LRESULT {
        if msg == WM_KEYDOWN {
            let vk = wparam.0 as u16; // virtual-key in the loword of wParam
            if vk == VK_PROCESSKEY.0 {
                // The IME owns this key (active composition). Route it to the IME and
                // CONSUME it so gpui's keydown handler never sees it (and so can't
                // mis-encode named keys like backspace/enter/arrows to the PTY).
                let m = MSG {
                    hwnd,
                    message: WM_KEYDOWN,
                    wParam: wparam,
                    lParam: lparam,
                    time: 0,
                    pt: POINT::default(),
                };
                let _ = TranslateMessage(&m);
                return LRESULT(0);
            }
        }
        DefSubclassProc(hwnd, msg, wparam, lparam)
    }

    /// Install the IME key-routing fix on a gpui window (see the note above). Must be
    /// called on the UI thread with a live HWND (e.g. the first `render` / window
    /// init). `SetWindowSubclass` is idempotent per (proc, id) — calling twice with
    /// the same id just refreshes it — and the OS removes the subclass on destroy,
    /// so no teardown is needed. Unlike `SetWindowPos`/`ShowWindow` it does NOT
    /// re-enter the window proc, so it is safe to call synchronously inside a gpui
    /// callback.
    pub fn install_ime_keyfix(h: isize) {
        unsafe {
            let _ = SetWindowSubclass(as_hwnd(h), Some(ime_subclass_proc), TN_IME_SUBCLASS_ID, 0);
        }
    }
}

#[cfg(target_os = "windows")]
pub use imp::*;

#[cfg(not(target_os = "windows"))]
mod stub {
    use futures::channel::mpsc::UnboundedReceiver;
    use tn_config::{HotkeySpec, Rect};

    pub fn spawn_hotkey_listener(_spec: &HotkeySpec) -> Option<UnboundedReceiver<()>> {
        None
    }
    pub fn make_topmost(_h: isize) {}
    pub fn set_bounds(_h: isize, _r: Rect) {}
    pub fn set_round_region(_h: isize, _w: f32, _ht: f32, _radius: f32) {}
    pub fn clear_region(_h: isize) {}
    pub fn show(_h: isize, _visible: bool) {}
    pub fn scale_for(_h: isize) -> f32 {
        1.0
    }
    pub fn work_area(_h: isize) -> Option<Rect> {
        None
    }
    pub fn system_beep() {}
    pub fn install_ime_keyfix(_h: isize) {}
}

#[cfg(not(target_os = "windows"))]
pub use stub::*;

/// Extract the OS window handle (HWND, as `isize`) from a gpui [`Window`].
/// gpui's `Window: HasWindowHandle`, so we read the raw handle and unwrap the
/// Win32 variant. `None` on non-Windows or if the handle is unavailable.
pub fn hwnd_of(window: &gpui::Window) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    // gpui's `Window` has an *inherent* `window_handle()` returning its own
    // `AnyWindowHandle`, which shadows the `HasWindowHandle` trait method — call
    // the trait method explicitly to get the raw OS handle.
    let handle = <gpui::Window as HasWindowHandle>::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
