//! 磷光 Phosphor shared style tokens + helpers — the single source of truth.
//!
//! Mirrors `design/phosphor.css :root` + the renderability contract
//! (docs/设计/磷光设计语言.md). One sentence: **precision instrument chassis
//! (structure = depth) + a single phosphor life color (only for live things)**.
//!
//! 契约(与原型强制一致,保证 GPUI 精准还原):
//!  1. 大面积只用不透明纯色 —— 海拔阶梯 L0..L4,杜绝大面渐变色带。
//!  2. 渐变只允许 ≤4px 条带 / ≤24px 小元素(tab 身份棒)。
//!  3. 边一律 1px;圆角 ≤10px。
//!  4. 投影只给「真浮层」(`shadow_float`);平铺板面零阴影 —— 深度 = 2px 接缝
//!     露出 L0 底盘 + 各自 1px 发丝边(规避平铺投影渗入邻板的坑)。
//!  5. 焦点 = 1px 磷光描边 + 四角角标(`focus_brackets`),永不 glow。
//!
//! 海拔/语义主色走主题(`config/themes/tn-dark.toml` 持有同一调色板);本文件
//! 提供主题 schema 不建模的**结构 token**(发丝线 alpha、文字阶梯、磷光衍生、
//! 语义 soft 底)与共享构件。
//!
//! `col`/`cola` accept either chrome colors (`tn_config::Color`) or terminal-cell
//! colors (`tn_core::Rgb`) via the [`Rgb8`] trait — both are just 8-bit RGB.

use gpui::{div, hsla, point, prelude::*, px, rgb, rgba, BoxShadow, Div, Rgba, Svg};

// ── 海拔阶梯(不透明;与 tn-dark.toml 同值,供不便走主题的场合直读) ──────────
pub(crate) const L0: u32 = 0x0B0E16; // 底盘 chassis:窗体背景、接缝露出色
pub(crate) const L1: u32 = 0x10141F; // 板面 plate:常驻 pane 基面
pub(crate) const L2: u32 = 0x151B29; // 抬升 raised:header、卡片、hover 行
pub(crate) const L3: u32 = 0x1B2334; // 浮板 sheet:浮层表面
pub(crate) const L4: u32 = 0x232C42; // 顶面 crest:浮层内选中、按下

// ── 发丝线(白,alpha 三档;0xRRGGBBAA) ────────────────────────────────────
pub(crate) const H0: u32 = 0xffffff0d; // ·05 平铺板面边 / 段间分隔
pub(crate) const H1: u32 = 0xffffff17; // ·09 卡片边 / 菜单分隔
pub(crate) const H2: u32 = 0xffffff29; // ·16 浮层边 / 滚动条 thumb

// ── 文字阶梯 ────────────────────────────────────────────────────────────────
pub(crate) const T0: u32 = 0xEAF0FB; // 主文
pub(crate) const T1: u32 = 0xA9B4CA; // 次文
pub(crate) const T2: u32 = 0x69748E; // 弱文
pub(crate) const T3: u32 = 0x3E4860; // 结构字符(刻度、占位)

// ── 磷光:唯一生命色(光标/运行/焦点/活动数据),永不发光 ──────────────────
pub(crate) const PH: u32 = 0x5BE7C4;
pub(crate) const PH_DIM: u32 = 0x5BE7C452; // ·32 焦点描边 / 角标臂以外的磷光线
pub(crate) const PH_SOFT: u32 = 0x5BE7C41f; // ·12 选区 / RUN 芯片底
pub(crate) const PH_INK: u32 = 0x06281F; // 磷光块上的反相墨色

// ── 语义状态(soft = ·12 芯片底;err-soft ·14) ─────────────────────────────
pub(crate) const OK: u32 = 0x8CD7A2;
pub(crate) const OK_SOFT: u32 = 0x8CD7A21f;
#[allow(dead_code)]
pub(crate) const WARN: u32 = 0xE5C07B;
#[allow(dead_code)]
pub(crate) const WARN_SOFT: u32 = 0xE5C07B1f;
pub(crate) const ERR: u32 = 0xE8707E;
pub(crate) const ERR_SOFT: u32 = 0xE8707E24;
pub(crate) const INFO: u32 = 0x82B4F0;
#[allow(dead_code)]
pub(crate) const INFO_SOFT: u32 = 0x82B4F01f;

// ── 浮层纯色压暗 scrim(无模糊,契约 7) ────────────────────────────────────
pub(crate) const SCRIM: u32 = 0x0507129e; // rgba(5,7,12,.62)

// ── 圆角(机加工:小而准) ──────────────────────────────────────────────────
pub(crate) const R_WINDOW: f32 = 10.0; // 整窗
pub(crate) const R_PANEL: f32 = 6.0; // 板面 / 浮层
pub(crate) const R_CARD: f32 = 4.0; // 卡片 / tab / 按钮
pub(crate) const R_CHIP: f32 = 3.0; // 芯片 / kbd / 树行

// ── 关键尺度(SHEET 01/02) ─────────────────────────────────────────────────
pub(crate) const TITLEBAR_H: f32 = 42.0;
pub(crate) const STATUSBAR_H: f32 = 26.0;
pub(crate) const PLATE_HEAD_H: f32 = 34.0;
pub(crate) const AGENT_HEAD_H: f32 = 38.0;
pub(crate) const SEAM: f32 = 2.0; // 平铺接缝:露出 L0 底盘

// ── 磷光字体系统(全部打包进 exe,见 lib.rs add_fonts) ─────────────────────
/// UI 正文无衬线(Inter):标签、正文、面板、菜单 —— 现代、极可读、几何中性。
pub(crate) const UI_SANS: &str = "Inter";
/// 展示字(Space Grotesk):词标 `TN_`、欢迎页 hero、大区块标题 —— 几何科技感。
pub(crate) const UI_DISPLAY: &str = "Space Grotesk";
/// 等宽(JetBrainsMono Nerd Font)默认族名,供需要内联 mono 字面量的场合;运行时
/// 实际等宽以 `config.font().family` 为准(用户可覆盖),故此常量当前仅作文档基准。
#[allow(dead_code)]
pub(crate) const UI_MONO: &str = "JetBrainsMono Nerd Font";
/// 中文回退族:通过 [`with_cjk`] / 根节点 `font_fallbacks` 串到主字族之后 —— 西文走
/// Inter/JBM/Space Grotesk,遇 CJK 字形落到此族。**必须是系统已装字体**:gpui 0.2.2
/// 的回退构建只在系统字体集里查族名(direct_write.rs:333),打包的内存字体当不了回退。
/// 用「微软雅黑 UI」(Win10/11 必装,匹配得上、无 ERROR 刷屏);装了更现代的 CJK
/// (如 MiSans / HarmonyOS Sans SC)可在此前置以自动升级。
pub(crate) const CJK_FALLBACK: &str = "Microsoft YaHei UI";

// ── 命名字号层级(磷光 type scale) ────────────────────────────────────────
// 单源字号,杜绝散落魔法数字;最小档从旧 10px 抬到 11px(密集小字更易读)。
/// 11 — 最小档:状态栏、芯片、kbd、次级标注。
pub(crate) const FS_MICRO: f32 = 11.0;
/// 12 — 说明档:footer 提示、小标签、元信息。
pub(crate) const FS_CAPTION: f32 = 12.0;
/// 13 — 正文档:菜单项、列表、Markdown 正文。
pub(crate) const FS_BODY: f32 = 13.0;
/// 14 — 强调档:卡片标题、tab、按钮。
pub(crate) const FS_LABEL: f32 = 14.0;
/// 18 — 标题档:区块标题、面板大标题(canonical 档位,待逐步采用)。
#[allow(dead_code)]
pub(crate) const FS_TITLE: f32 = 18.0;
/// 26 — hero 档:欢迎页词标。
pub(crate) const FS_HERO: f32 = 26.0;

/// 给一个主字族构造带中文回退的 [`gpui::Font`]。西文用 `family`,CJK 字形自动落到
/// 思源黑体 SC(DirectWrite 把该回退排在系统回退之前)。用于终端/编辑器/Markdown
/// 这些直接构造整 `Font` 的场合;普通 Div 文本由根节点继承 `font_fallbacks`,只需
/// `.font_family(...)` 即可。
pub(crate) fn with_cjk(family: &str) -> gpui::Font {
    let mut f = gpui::font(gpui::SharedString::from(family.to_owned()));
    f.fallbacks = Some(gpui::FontFallbacks::from_fonts(vec![CJK_FALLBACK.to_string()]));
    f
}

/// 8-bit RGB, implemented by both the config/theme color and the terminal-cell
/// color so [`col`]/[`cola`] work with either.
pub(crate) trait Rgb8 {
    fn channels(&self) -> (u8, u8, u8);
}
impl Rgb8 for tn_config::Color {
    fn channels(&self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }
}
impl Rgb8 for tn_core::Rgb {
    fn channels(&self) -> (u8, u8, u8) {
        (self.r, self.g, self.b)
    }
}

/// Opaque GPUI color.
pub(crate) fn col(c: impl Rgb8) -> Rgba {
    let (r, g, b) = c.channels();
    rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32)
}

/// Color with explicit alpha — 磷光体系里 alpha 只用于发丝线、soft 芯片底和
/// 身份色衍生(小面积),大面积一律不透明(契约 1)。
pub(crate) fn cola(c: impl Rgb8, a: f32) -> Rgba {
    let (r, g, b) = c.channels();
    Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

/// A soft, contained drop shadow. 磷光体系中只有真浮层可用(契约 4);
/// 平铺板面零阴影。
pub(crate) fn soft_shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: hsla(0., 0., 0., alpha),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur),
        spread_radius: px(spread),
    }
}

/// Attach a shadow stack to a div. The outer wrapper must have explicit size
/// and the parent must NOT clip overflow, otherwise the shadows are cropped.
pub(crate) fn shadowed(d: Div, shadows: Vec<BoxShadow>) -> Div {
    d.shadow(shadows)
}

/// 浮层投影(phosphor.css `--shadow-float`):
/// `0 2px 8px -2px rgba(0,0,0,.55), 0 28px 72px -28px rgba(0,0,0,.92)`。
/// 全系统唯一允许的投影 —— App Menu、命令面板、QuickLook、幽灵终端共用。
pub(crate) fn shadow_float() -> Vec<BoxShadow> {
    vec![
        soft_shadow(2.0, 8.0, -2.0, 0.55),
        soft_shadow(28.0, 72.0, -28.0, 0.92),
    ]
}

/// 四角磷光角标(取景器):焦点的全部表达,零 glow(契约 5)。
/// 臂长 11 × 厚 2,贴在宿主 1px 边框的外缘(-1px)。宿主需 `relative` 且不裁剪
/// 溢出;平铺板面间有 2px 接缝,1px 的外伸不会侵入邻板。
pub(crate) fn focus_brackets() -> Vec<Div> {
    let bar = || div().absolute().bg(rgb(PH));
    vec![
        // 左上
        bar().top(px(-1.)).left(px(-1.)).w(px(11.)).h(px(2.)),
        bar().top(px(-1.)).left(px(-1.)).w(px(2.)).h(px(11.)),
        // 右上
        bar().top(px(-1.)).right(px(-1.)).w(px(11.)).h(px(2.)),
        bar().top(px(-1.)).right(px(-1.)).w(px(2.)).h(px(11.)),
        // 左下
        bar().bottom(px(-1.)).left(px(-1.)).w(px(11.)).h(px(2.)),
        bar().bottom(px(-1.)).left(px(-1.)).w(px(2.)).h(px(11.)),
        // 右下
        bar().bottom(px(-1.)).right(px(-1.)).w(px(11.)).h(px(2.)),
        bar().bottom(px(-1.)).right(px(-1.)).w(px(2.)).h(px(11.)),
    ]
}

/// 平铺板面(phosphor.css `.plate` / `.plate.focus`)。
///
/// `inner` 须自带不透明基面(L1 / `ui.surface_1`)+ `rounded(R_PANEL - 1.)` +
/// `overflow_hidden`。焦点 = 边升级为磷光 ·32% + 四角角标;非焦点 = 1px h0。
/// 永远零投影 —— 深度由 2px 接缝(外层 `gap`)露出 L0 表达。
pub(crate) fn plate(inner: Div, focused: bool) -> Div {
    let mut outer = div()
        .size_full()
        .relative()
        .rounded(px(R_PANEL))
        .border_1()
        .border_color(if focused { rgba(PH_DIM) } else { rgba(H0) })
        .child(inner);
    if focused {
        outer = outer.children(focus_brackets());
    }
    outer
}

/// 真浮层(phosphor.css `.float`):L3 浮板 + 1px h2 边 + 浮层投影。
/// `inner` 须自带不透明 L3 基面(`ui.palette_bg`)+ `rounded(R_PANEL - 1.)` +
/// `overflow_hidden`。父级不得裁剪溢出,否则投影被切。
pub(crate) fn float_panel(inner: Div) -> Div {
    shadowed(
        div()
            .size_full()
            .rounded(px(R_PANEL))
            .border_1()
            .border_color(rgba(H2))
            .child(inner),
        shadow_float(),
    )
}

/// 按钮(phosphor.css `.btn`):L2 + 1px h1 + r4 · sans 12 · t1;hover = L4 + t0。
/// 调用方自行挂 on_mouse_down。
pub(crate) fn btn(label: impl Into<gpui::SharedString>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(14.))
        .py(px(5.))
        .rounded(px(R_CARD))
        .border_1()
        .border_color(rgba(H1))
        .bg(rgb(L2))
        .text_size(px(FS_CAPTION))
        .text_color(rgb(T1))
        .hover(|s| s.bg(rgb(L4)).text_color(rgb(T0)))
        .child(label.into())
}

/// 主按钮(`.btn.primary`):磷光填充 + 反相墨字 600。
pub(crate) fn btn_primary(label: impl Into<gpui::SharedString>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(14.))
        .py(px(5.))
        .rounded(px(R_CARD))
        .border_1()
        .border_color(rgb(PH))
        .bg(rgb(PH))
        .text_size(px(FS_CAPTION))
        .font_weight(gpui::FontWeight(600.))
        .text_color(rgb(PH_INK))
        .child(label.into())
}

/// 危险按钮(`.btn.danger`):err-soft 底 + err 字。
pub(crate) fn btn_danger(label: impl Into<gpui::SharedString>) -> Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.))
        .px(px(14.))
        .py(px(5.))
        .rounded(px(R_CARD))
        .border_1()
        .border_color(rgba(0xE8707E59)) // err ·35
        .bg(rgba(ERR_SOFT))
        .text_size(px(FS_CAPTION))
        .text_color(rgb(ERR))
        .child(label.into())
}

/// 键帽(`.kbd`):mono 10 · t1 · L2 底 + h1 边(底边 2px)· r3。
pub(crate) fn kbd(label: impl Into<gpui::SharedString>, mono: gpui::SharedString) -> Div {
    div()
        .font_family(mono)
        .text_size(px(FS_MICRO))
        .text_color(rgb(T1))
        .px(px(6.))
        .py(px(1.))
        .rounded(px(R_CHIP))
        .bg(rgb(L2))
        .border_1()
        .border_b(px(2.))
        .border_color(rgba(H1))
        .child(label.into())
}

/// A line icon, sized square and tinted `color`. (gpui paints an SVG only when
/// a text color is set, so the tint is always explicit — see `assets.rs`.)
pub(crate) fn icon(name: &str, size: f32, color: impl Rgb8) -> Svg {
    gpui::svg()
        .path(crate::assets::icon_path(name))
        .w(px(size))
        .h(px(size))
        .flex_none()
        .text_color(col(color))
}
