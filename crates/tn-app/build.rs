//! Sideload a modern ConPTY next to the `tn` executable (Windows only).
//!
//! Why: the system `conhost.exe` on Windows 10 (build 19041) runs ConPTY in its
//! legacy rendering mode, which **strips** a child's alternate-screen and mouse
//! DECSETs (1049/1000/1002/1003/1006/1007) instead of forwarding them to the host
//! pipe. A full-screen agent TUI such as codex (ratatui/crossterm) therefore looks
//! to our terminal engine like a plain main-screen app with no mouse capture:
//! `alt_screen=false mouse_report=false`, no scrollback, and the wheel has nothing
//! to drive — so codex can't be scrolled, even though Windows Terminal (which
//! bundles its own newer ConPTY) scrolls it fine.
//!
//! `portable-pty` already prefers a `conpty.dll` sideloaded next to the app over
//! the kernel32 export (see its `load_conpty`). The modern redistributable
//! (Microsoft.Windows.Console.ConPTY, vendored under `vendor/conpty/`) runs in
//! passthrough mode and forwards the child's modes verbatim, so our existing
//! wheel→mouse-report routing forwards the wheel to codex and it scrolls natively.
//!
//! `conpty.dll` launches `OpenConsole.exe` from its own directory, so both must
//! land beside `tn.exe`. We copy them into the Cargo output dir (`target/<profile>`).

use std::path::{Path, PathBuf};

fn main() {
    // Only meaningful on Windows; no-op elsewhere.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let sub = match arch.as_str() {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => {
            println!("cargo:warning=tn-app: no vendored ConPTY for target arch `{other}`; using system conhost (agent scrollback may be limited)");
            return;
        }
    };

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest_dir.join("vendor").join("conpty").join(sub);
    let files = ["conpty.dll", "OpenConsole.exe"];

    // Output dir holding the exe: OUT_DIR is `<target>/<profile>/build/<pkg>/out`,
    // so three parents up is `<target>/<profile>` where `tn.exe` is written.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(exe_dir) = out_dir.ancestors().nth(3) else {
        println!("cargo:warning=tn-app: could not derive target dir from OUT_DIR; ConPTY not sideloaded");
        return;
    };

    for f in files {
        let src = vendor.join(f);
        println!("cargo:rerun-if-changed={}", src.display());
        if !src.exists() {
            println!(
                "cargo:warning=tn-app: vendored ConPTY file missing: {} (agent scrollback may be limited)",
                src.display()
            );
            continue;
        }
        let dst = exe_dir.join(f);
        if let Err(e) = copy_if_changed(&src, &dst) {
            println!("cargo:warning=tn-app: failed to copy {} -> {}: {e}", src.display(), dst.display());
        }
    }
}

/// Copy only when the destination is missing or differs in size — keeps
/// incremental builds cheap and avoids touching a DLL another process may hold.
fn copy_if_changed(src: &Path, dst: &Path) -> std::io::Result<()> {
    let needs = match (std::fs::metadata(src), std::fs::metadata(dst)) {
        (Ok(s), Ok(d)) => s.len() != d.len(),
        _ => true,
    };
    if needs {
        std::fs::copy(src, dst)?;
    }
    Ok(())
}
