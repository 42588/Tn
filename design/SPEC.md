# Tn 界面规格单（SPEC）

> **本文件由 `TN_GEN_SPEC=1 cargo test -p tn-ui spec_gen` 生成**,取自 `design/mockup.html`
> + `tn-dark.toml` + `style.rs`。**勿手改**——改原件后重跑。实现 gpui 界面时**照抄数值、别看图估**;
> 网页↔代码的颜色一致性另由 `style::token_drift` 测试守卫。翻译查 [CSS_TO_GPUI.md](CSS_TO_GPUI.md)。

## 1. 设计令牌（单一真源）

> 颜色定义在 `tn-dark.toml`、白叠加/圆角定义在 `style.rs`;mockup 的同名变量是**受守卫的副本**。

| mockup `--var` | 值 | gpui 写法 | 定义处 |
|---|---|---|---|
| `--fg` | `#C6D0F5` | `col(ui.foreground)` | tn-dark.toml |
| `--muted` | `#6E76A0` | `col(ui.muted)` | tn-dark.toml |
| `--accent` | `#7AA2F7` | `col(ui.accent)` | tn-dark.toml |
| `--violet` | `#BB9AF7` | `col(ui.accent_alt)` | tn-dark.toml |
| `--green` | `#9ECE6A` | `col(t.ansi.green)` | tn-dark.toml |
| `--red` | `#F7768E` | `col(t.ansi.red)` | tn-dark.toml |
| `--yellow` | `#E0AF68` | `col(t.ansi.yellow)` | tn-dark.toml |
| `--cyan` | `#7DCFFF` | `col(t.ansi.cyan)` | tn-dark.toml |
| `--claude` | `#F0916D` | `col(t.agents.claude)` | tn-dark.toml |
| `--codex` | `#73DACA` | `col(t.agents.codex)` | tn-dark.toml |
| `--rim` | `0xffffff12（白 @ 7%）` | `rgba(RIM)` | style.rs |
| `--sheen` | `0xffffff1a（白 @ 10%）` | `rgba(SHEEN)` | style.rs |
| `--g2` | `0xffffff0a（白 @ 4%）` | `rgba(INSET)` | style.rs |
| `--g3` | `0xffffff0f（白 @ 6%）` | `rgba(HOVER)` | style.rs |
| `（状态栏分隔）` | `0xffffff0f（白 @ 6%）` | `rgba(DIVIDER)` | style.rs |
| `--r-win` | `16px` | `rounded(px(R_WINDOW))` | style.rs |
| `--r-pane` | `14px` | `rounded(px(R_PANEL))` | style.rs |
| `--r-card` | `11px` | `rounded(px(R_CARD))` | style.rs |

## 2. 组件规格（mockup 逐类精确值,`var()` 已解析）

**`.work`**
- `gap`: 11px
- `padding`: 5px 12px 11px

**`.pane`**
- `background`: linear-gradient(180deg, rgba(42,46,68,0.42), rgba(26,28,44,0.52))
- `border-radius`: 14px
- `min-width`: 0
- `border`: 1px solid rgba(255,255,255,0.07)

**`.phead`**
- `height`: 36px
- `gap`: 9px
- `padding`: 0 13px
- `font-size`: 11.5px
- `font-weight`: 560
- `color`: #6E76A0

**`.cwd`**
- `color`: #A6AFD4

**`.chip`**
- `font-size`: 10.5px
- `font-weight`: 560
- `padding`: 2px 9px
- `border-radius`: 999px
- `color`: #A6AFD4
- `background`: rgba(255,255,255,0.06)

**`.tnode`**
- `gap`: 7px
- `height`: 26px
- `padding`: 0 10px
- `border-radius`: 8px
- `color`: #A6AFD4

**`.tag`**
- `font-size`: 9px
- `font-weight`: 800
- `width`: 15px
- `height`: 15px
- `border-radius`: 5px

**`.agenthead`**
- `gap`: 11px
- `padding`: 10px 14px
- `background`: linear-gradient(180deg, rgba(240,145,109,0.07), transparent 72%)

**`.who`**
- `gap`: 1px

**`.nm`**
- `font-size`: 13px
- `font-weight`: 680
- `color`: #C6D0F5

**`.model`**
- `font-size`: 11px
- `color`: #6E76A0
- `font-weight`: 520

**`.usage`**
- `gap`: 11px
- `padding`: 4px 5px 4px 12px
- `border-radius`: 999px
- `background`: rgba(255,255,255,0.04)

**`.tok`**
- `font-size`: 11px
- `font-weight`: 640
- `color`: #A6AFD4

**`.cost`**
- `font-size`: 10.5px
- `font-weight`: 640
- `color`: #9ECE6A

**`.ring`**
- `width`: 32px
- `height`: 32px

**`.lbl`**
- `font-size`: 9px
- `font-weight`: 760
- `color`: #C6D0F5

**`.agentbody`**
- `padding`: 12px 15px
- `font-size`: 12.5px

**`.tool`**
- `gap`: 9px

**`.say`**
- `padding`: 11px 13px
- `border-radius`: 11px
- `color`: #C6D0F5
- `background`: linear-gradient(180deg, rgba(255,255,255,0.05), rgba(255,255,255,0.018))

**`.body`**
- `padding`: 11px 15px
- `font-size`: 12.5px

**`.status`**
- `height`: 30px
- `padding`: 0 6px
- `font-size`: 11px
- `font-weight`: 510
- `color`: #6E76A0
- `background`: linear-gradient(180deg, transparent, rgba(0,0,0,0.2))

**`.seg2`**
- `gap`: 6px
- `padding`: 0 13px
- `height`: 18px

**`.tab`**
- `gap`: 7px
- `height`: 34px
- `padding`: 0 14px
- `border-radius`: 11px 11px 0 0
- `font-size`: 12.5px
- `font-weight`: 520
- `color`: #6E76A0

**`.newtab`**
- `color`: #6E76A0
- `width`: 29px
- `height`: 29px
- `border-radius`: 9px

**`.wctl`**
- `gap`: 2px
- `color`: #6E76A0

