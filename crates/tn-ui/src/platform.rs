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
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
        ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE,
        SW_SHOW, WM_HOTKEY, WS_EX_TOPMOST,
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
                    tracing::warn!("RegisterHotKey failed (hotkey likely owned by another app)");
                    return;
                }
                tracing::info!("quick-terminal global hotkey registered");
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
    pub fn show(_h: isize, _visible: bool) {}
    pub fn work_area(_h: isize) -> Option<Rect> {
        None
    }
    pub fn system_beep() {}
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
