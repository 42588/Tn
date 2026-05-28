# mockup_replica — runnable gpui replica of `design/mockup.html`

`mockup_replica.rs` is a **self-contained gpui window** that reproduces the
`design/mockup.html` prototype (Tn Dark · Calm Glass) so the owner can run it on
a real machine and **visually compare it, side-by-side, against the mockup** —
validating that our CSS→gpui mapping (`docs/CSS_TO_GPUI.md`) produces the
intended look. It deliberately uses **no** `tn-ui` internals: every token
(colors, white-overlay alphas, radii), component value (`FontWeight(N.)`,
paddings/gaps/heights), gradient, rim, sheen, and shadow is **inlined from
`docs/CSS_TO_GPUI.md` §16** (the authoritative auto-generated spec), so the
replica tests the *documented values*, not the production widgets.

## Run

```powershell
cargo run -p tn-ui --example mockup_replica
```

(cargo isn't on PATH in this repo's shell — use the full path if needed:
`& "$env:USERPROFILE\.cargo\bin\cargo.exe" run -p tn-ui --example mockup_replica`)

The replica is wired as a **tn-ui example** (project rule: `gpui` may only be
linked in `tn-ui` / `tn-app`) via `crates/tn-ui/Cargo.toml`:

```toml
[[example]]
name = "mockup_replica"
path = "../../docs/test/mockup_replica.rs"
```

## Side-by-side comparison checklist

Open `design/mockup.html` in a browser (it's a fixed 1200×760 prototype) and
put the replica window beside it. Check:

- **Window chrome** — the `.win` card fills the OS window **edge-to-edge** (no
  desktop margin); the OS (Windows DWM) rounds the outer corners. Glass gradient
  body (`rgba(21,22,34,0.62)→rgba(15,16,25,0.72)`), 1px rim border (`white @ 6%`),
  faint 1px top sheen highlight (no glow).
- **Titlebar** — brand mark (accent→violet gradient rounded square + `Tn`),
  three tabs (**Claude** active / **pwsh** / **Codex**) each with its agent-color
  icon; the active tab shows a 2px **accent strip** along its top edge in the
  agent color (claude = `#F0916D`), plus the `~/proj/tn` mono badge; `+` new-tab
  and min/max/close controls (close hovers red).
- **Panel glass** — each `.pane` uses the `--g1` gradient
  `linear_gradient(180°, rgba(42,46,68,0.42) → rgba(26,28,44,0.52))`, a 1px rim
  border, a top specular highlight (upper 36% white→transparent), and a 1px
  sheen line at the very top. The **focused** agent pane has a warm claude rim
  (`claude @ 24%`) and a deeper shadow.
- **Explorer sidebar** — ~6+ tree rows with 16px-per-level indents, dir rows
  (accent folder icon, fg text, fw 540) vs file rows, the active `element.rs`
  row (claude-tinted file icon + sheen), and git tags **M** (yellow @ 15%) /
  **U** (green @ 15%), 15×15 rounded-5 chips, fw 800.
- **Agent pane** — header with avatar (claude @ 14% bg), `Claude Code` /
  `Sonnet 4.6`, the **usage ring** (32px, claude arc at 42% over a 10% white
  track) with `84K / 200K` + green `$0.31`, and a `· Thinking…` line; body with
  check/diamond/circle tool rows (cyan mono code spans) and a `.say` bubble.
- **Status bar** — segments `⎇ main · 3 sessions · ◆ ctx 42% · ◆ ctx 18% ·
  … · element.rs · Rust | UTF-8 | Tn Dark`, separated by faint 1px left
  dividers (`white @ 6%`), 30px tall, on a transparent→`black @ 20%` gradient.

### Color / type quick reference (must match)

| What | Value |
|------|-------|
| foreground `--fg` | `#C6D0F5` |
| dim `--fg-dim` | `#A6AFD4` |
| muted `--muted` | `#6E76A0` |
| faint `--faint` | `#474E72` |
| accent / violet | `#7AA2F7` / `#BB9AF7` |
| green / red / yellow / cyan | `#9ECE6A` / `#F7768E` / `#E0AF68` / `#7DCFFF` |
| claude / codex | `#F0916D` / `#73DACA` |
| rim / sheen / inset(g2) / hover(g3) | white @ 7% / 10% / 4% / 6% |
| radii | win 16 · pane 14 · card 11 |
| fonts | UI = Segoe UI · mono/code = Cascadia Code |

## Known simplifications (gpui 0.2.2 limits — see CSS_TO_GPUI.md §14)

- **No per-div blur** (`backdrop-filter`): the frosted look comes from the
  translucent panel gradients + rim + sheen, not real blur.
- **No desktop backdrop**: the mockup floats the card on a dark desktop lit by
  faint blue/teal **radial washes** — gpui 0.2.2 has no `radial-gradient`, so
  rather than render a flat near-black margin (which just reads as a black frame),
  the window fills edge-to-edge with the card itself (matches the real Tn app).
- **No noise / vignette / `mix-blend-mode`**: ignored.
- **No animation**: the `Thinking…` pulse dot, the spinning run diamond, and the
  prompt cursor blink are drawn **static** (gpui has no CSS transition; real
  motion needs a frame clock).
- **`box-shadow: inset`** is unsupported → every top highlight is an absolute
  1px sheen `div`.
- **Fractional `flex:`** weights (0.6 / 1.55 / 2.5 / 0.85 / 1.18) are applied by
  setting `flex_grow` directly (gpui's `flex_1()` hard-codes 1).
