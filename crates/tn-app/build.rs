//! Stage the bundled native sidecar binaries next to the `tn` executable
//! (Windows only).
//!
//! Some capabilities need real DLLs / helper exes on disk at runtime — they
//! cannot be executed from inside `tn.exe`:
//!
//! * **conpty.dll + OpenConsole.exe** — a modern ConPTY. The system conhost on
//!   Windows 10 (build 19041) strips a child's alternate-screen/mouse DECSETs, so
//!   a full-screen agent TUI (codex) can't be scrolled. `portable-pty` prefers a
//!   `conpty.dll` sideloaded next to the app; conpty.dll launches OpenConsole.exe
//!   from its own dir, so both must sit beside `tn.exe`.
//! * **pdfium.dll** — the PDF rendering engine used by Quick Look (pdfium-render
//!   binds to it via `LoadLibrary`).
//!
//! All bundled sidecars live under `vendor/<name>/<arch>/`. This script copies the
//! files for the current target arch into the Cargo output dir (`target/<profile>`)
//! where `tn.exe` is written, so `cargo run` and packaged builds both find them.
//! Embedded assets (fonts, icon, config) are NOT here — they are already compiled
//! into the exe via `include_bytes!`/`include_str!`.

use std::path::{Path, PathBuf};

fn main() {
    // Only meaningful on Windows; no-op elsewhere.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    // Embed the app icon as a Win32 resource so tn.exe — and therefore the
    // taskbar button, Explorer, Alt-Tab, and the title bar — shows the Tn brand
    // mark instead of the generic default. The runtime WM_SETICON (platform.rs)
    // only dresses a live window; the exe resource covers every surface and is
    // the canonical fix. Arch-independent, so it runs before the arch logic.
    #[cfg(windows)]
    embed_app_icon();

    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let sub = match arch.as_str() {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => {
            println!("cargo:warning=tn-app: no vendored sidecars for target arch `{other}`; ConPTY/PDF features may be limited");
            return;
        }
    };

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vendor = manifest_dir.join("vendor");
    println!("cargo:rerun-if-changed={}", vendor.display());

    // Output dir holding the exe: OUT_DIR is `<target>/<profile>/build/<pkg>/out`,
    // so three parents up is `<target>/<profile>` where `tn.exe` is written.
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let Some(exe_dir) = out_dir.ancestors().nth(3).map(Path::to_path_buf) else {
        println!("cargo:warning=tn-app: could not derive target dir from OUT_DIR; sidecars not staged");
        return;
    };

    // Each `vendor/<name>/<arch>/` directory contributes its files beside the exe.
    let entries = match std::fs::read_dir(&vendor) {
        Ok(e) => e,
        Err(e) => {
            println!("cargo:warning=tn-app: cannot read vendor dir {}: {e}", vendor.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let arch_dir = entry.path().join(sub);
        let Ok(files) = std::fs::read_dir(&arch_dir) else {
            continue; // this sidecar has no build for the current arch
        };
        for f in files.flatten() {
            let src = f.path();
            if !src.is_file() {
                continue;
            }
            let dst = exe_dir.join(f.file_name());
            if let Err(e) = copy_if_changed(&src, &dst) {
                println!("cargo:warning=tn-app: failed to copy {} -> {}: {e}", src.display(), dst.display());
            }
        }
    }
}

/// Compile the Tn icon into tn.exe as the default application icon resource.
#[cfg(windows)]
fn embed_app_icon() {
    let icon = "../tn-ui/assets/tn.ico";
    println!("cargo:rerun-if-changed={icon}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon);
    if let Err(e) = res.compile() {
        // Non-fatal: a missing icon is cosmetic. Surface it so it isn't silent.
        println!("cargo:warning=tn-app: failed to embed app icon resource: {e}");
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
