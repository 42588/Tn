# Tn

A Windows-first, GPU-accelerated terminal for **vibe coding** — built in Rust for
performance and aesthetics, with first-class hosting of AI coding CLIs
(Claude Code, Codex), a Warp-style block UI, and WSL + remote-Linux (SSH) support.

> **Status: M0 complete.** A live interactive PowerShell runs in a GPUI window
> (DirectX 11 + DirectWrite) with keyboard input and window-fit resize.
> See [docs/BLUEPRINT.md](docs/BLUEPRINT.md) for the full technical manual and roadmap.

## Stack

| Layer | Choice |
|---|---|
| UI / render | [GPUI](https://gpui.rs) (Zed's framework) — DirectX 11 + DirectWrite on Windows |
| Terminal engine | [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) (VT parser + grid) |
| PTY / remote | [`portable-pty`](https://crates.io/crates/portable-pty) (ConPTY + WSL) + [`russh`](https://crates.io/crates/russh) (SSH) |
| Language | Rust 2021 (stable, MSVC) |
| License | GPL-3.0-or-later |

## Quick start

```powershell
# Requires: Rust (stable-x86_64-pc-windows-msvc) + VS C++ build tools.
cargo run -p tn-app          # opens the terminal window
```

Headless checks (no window needed):

```powershell
cargo test -p tn-core        # engine unit tests
cargo run  -p tn-cli         # ConPTY smoke test (spawn shell, render grid to stdout)
$env:TN_AUTOQUIT="1"; cargo run -p tn-app   # GUI self-test: render grid, then quit
```

## Workspace

```
crates/tn-core    terminal engine (alacritty wrapper)      — headless
crates/tn-pty     PTY backends (ConPTY; WSL/SSH planned)   — headless
crates/tn-config  config + theming                          — stub (M1)
crates/tn-ui      GPUI front-end (the only gpui-linked lib)
crates/tn-app     the `tn` binary
crates/tn-cli     headless debug/smoke harness
```

See [docs/BLUEPRINT.md](docs/BLUEPRINT.md) for architecture, data flow, design
decisions, the milestone roadmap, and the development guide.
