//! 像素宠物(特色③)— 终端陪伴小狗。
//!
//! 规则来源 docs/宠物/宠物系统规则.md;视觉规格 design/panels/05-pet-system.html。
//! 渲染:14×12 像素网格逐 quad 直绘(≤168 quad/帧,像素公民 — 与磷光网格同源,
//! 无 SVG tint 单色限制);栖位 = 状态栏上方 overlay,踩 1px 发丝「岗台」。
//!
//! 铁律(实现边界):
//!  - 不进 terminal grid、不写 buffer、不遮 prompt/光标/选区/浮层。
//!  - 上下文来自结构化事件(OSC 133 OutputStart/CommandFinished + 键入信号),
//!    不扫描输出文本;宠物只订阅,绝不反向影响 PTY/shell/agent。
//!  - 固定 100×84 容器,杜绝布局抖动;品种生命周期 = 终端进程生命周期。
//!  - 状态优先级 Drag > Play > Error/Success > Running > Typing > Hover >
//!    Sleep > Idle(Play=双击逗弄;Sleep=长空闲打盹,任何活动唤醒)。
//!  - 可关闭;reduced motion(`[editor] animations = "off"`)下全静帧。

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    canvas, div, fill, point, prelude::*, px, rgb, rgba, size, Bounds, Context, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, SharedString, Window,
};
use serde::{Deserialize, Serialize};
use tn_config::Loaded;

use crate::style::{
    col, ERR, H0, H1, H2, L0, L1, L2, L4, OK, PH, PH_DIM, R_CARD, R_CHIP, SCRIM, STATUSBAR_H, T0,
    T1, T2, T3,
};

// ═══════════════════════════ 终端上下文信号(进程级) ═══════════════════════
//
// 写入方:terminal_view(键入)与 io reader 线程(OSC 133 命令事件)。原子量、
// 无锁、无 GPUI 依赖 —— IO 线程也能写。宠物 tick 读取后推导姿态,绝不反向影响。

static LAST_KEY_MS: AtomicU64 = AtomicU64::new(0);
static RUN_COUNT: AtomicI64 = AtomicI64::new(0);
static RUN_START_MS: AtomicU64 = AtomicU64::new(0);
static LAST_EXIT_MS: AtomicU64 = AtomicU64::new(0);
/// 编码:0 = 无;1 = exit 0(success);2 = exit ≠0(error)。
static LAST_EXIT_KIND: AtomicU64 = AtomicU64::new(0);

// ── 规则 E 工作共情(结构化事件派生,零文本扫描)──
/// 连续 exit 0 计数(翻盘/三连胜判定)。
static SUCCESS_STREAK: AtomicU64 = AtomicU64::new(0);
/// 连续 exit ≠0 计数(连败陪伴判定 ≥3)。
static FAIL_STREAK: AtomicU64 = AtomicU64::new(0);
/// 一次性共情事件槽:0 无 / 1 三连胜 / 2 大功告成 / 3 翻盘 / 4 提交时刻。
/// pet tick 读后据此装点 Success 演出(双爱心/连跳/单爱心),并施加共享冷却。
static EMPATHY_KIND: AtomicU64 = AtomicU64::new(0);
static EMPATHY_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 本地时间(年/月/日/时)。每日见面、时段问候和羁绊档案都按**本地自然日**,
/// 故走 OS 本地时区(Windows: `GetLocalTime`),不用 UTC —— 否则 UTC+8 用户的
/// 「新的一天」会卡在早上 8 点。
struct LocalTime {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
}

impl LocalTime {
    /// `YYYY-MM-DD`(自然日键,用于 days_together / last_seen 比对)。
    fn date_key(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

#[cfg(windows)]
fn local_now() -> LocalTime {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    LocalTime {
        year: st.wYear,
        month: st.wMonth as u8,
        day: st.wDay as u8,
        hour: st.wHour as u8,
    }
}

#[cfg(not(windows))]
fn local_now() -> LocalTime {
    // 兜底(本项目仅 Windows 运行;此分支只为非 Windows 可编译/跑测试)。UTC。
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let hour = ((secs % 86_400) / 3600) as u8;
    // civil_from_days(Howard Hinnant):天数 → 年月日。
    let z = (secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    LocalTime {
        year: (y + i64::from(m <= 2)) as u16,
        month: m,
        day: d,
        hour,
    }
}

/// 自然日序号(Howard Hinnant days_from_civil)。用于「久别亲近」日期差。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD` → 自然日序号(失败 = None)。
fn date_to_days(key: &str) -> Option<i64> {
    let mut it = key.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    Some(days_from_civil(y, m, d))
}

/// 常用窝象限标签(规则 I 词表)。0 左下 / 1 右下 / 2 左上 / 3 右上。
fn perch_label(q: u8) -> &'static str {
    match q {
        0 => "左下角小窝",
        1 => "右下角小窝",
        2 => "左上角小窝",
        _ => "右上角小窝",
    }
}

/// 时段(规则 A 问候词表):清晨 5-9 / 白天 9-18 / 傍晚 18-23 / 深夜 23-5。
#[derive(Clone, Copy, PartialEq, Eq)]
enum DayPart {
    Dawn,
    Day,
    Dusk,
    Night,
}

impl DayPart {
    fn from_hour(h: u8) -> Self {
        match h {
            5..=8 => DayPart::Dawn,
            9..=17 => DayPart::Day,
            18..=22 => DayPart::Dusk,
            _ => DayPart::Night,
        }
    }
    fn now() -> Self {
        Self::from_hour(local_now().hour)
    }
    /// 见面问候(词表封闭,≤6 字铁律)。
    fn greeting(self) -> &'static str {
        match self {
            DayPart::Dawn => "早!",
            DayPart::Day => "来啦!",
            DayPart::Dusk => "晚上好",
            DayPart::Night => "夜深了…",
        }
    }
    fn is_night(self) -> bool {
        self == DayPart::Night
    }
}

/// 终端键入(KeyPress)→ Typing 上下文。从 terminal_view 的按键路径调用。
pub(crate) fn signal_typing() {
    LAST_KEY_MS.store(now_ms(), Ordering::Relaxed);
}

/// OSC 133 `C`(OutputStart):命令开始执行。
fn signal_command_start() {
    if RUN_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
        RUN_START_MS.store(now_ms(), Ordering::Relaxed);
    }
}

/// OSC 133 `D`(CommandFinished):命令结束 + 退出码。`cmd` 来自 OSC 633 `E`
/// 结构化命令行(只读字段,不扫描输出),`run_ms` 为本命令运行时长。
fn signal_command_end(exit: Option<i32>, cmd: &str, run_ms: u64) {
    signal_run_released();
    let now = now_ms();
    LAST_EXIT_MS.store(now, Ordering::Relaxed);
    LAST_EXIT_KIND.store(
        match exit {
            Some(0) => 1,
            Some(_) => 2,
            None => 0, // 无退出码 → 不演出
        },
        Ordering::Relaxed,
    );
    // ── 规则 E 共情分类(零文本扫描:仅 exit / 时长 / 结构化命令行首词)──
    match exit {
        Some(0) => {
            let prev_fail = FAIL_STREAK.swap(0, Ordering::Relaxed);
            let streak = SUCCESS_STREAK.fetch_add(1, Ordering::Relaxed) + 1;
            // 优先级:提交时刻 > 大功告成 > 翻盘 > 三连胜(更具体者优先)。
            let kind = if cmd_is_commit(cmd) {
                4
            } else if run_ms > 60_000 {
                2
            } else if prev_fail > 0 {
                3
            } else if streak >= 3 {
                1
            } else {
                0
            };
            if kind != 0 {
                EMPATHY_KIND.store(kind, Ordering::Relaxed);
                EMPATHY_MS.store(now, Ordering::Relaxed);
            }
        }
        Some(_) => {
            SUCCESS_STREAK.store(0, Ordering::Relaxed);
            FAIL_STREAK.fetch_add(1, Ordering::Relaxed);
        }
        None => {}
    }
}

/// 命令行首词是否 `git commit` / `git push`(规则 E 提交时刻;结构化字段,
/// 不扫描输出)。
fn cmd_is_commit(cmd: &str) -> bool {
    let mut it = cmd.split_whitespace();
    it.next() == Some("git") && matches!(it.next(), Some("commit") | Some("push"))
}

/// 只归还 Running 计数,不触发任何演出(会话中途关闭的欠账核销用)。
fn signal_run_released() {
    // 下限 0:错过 start 的孤儿 end 不把计数拖成负。
    let _ = RUN_COUNT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some((n - 1).max(0))
    });
}

/// 会话级 Running 守卫:每个 PTY reader 线程持有一个,记录本会话「开了还没
/// 关」的命令数;会话中途关闭(reader 退出)时把欠账还清。否则全局
/// [`RUN_COUNT`] 永久泄漏 —— 真机曾出现 NO SESSION 下宠物仍 RUNNING
/// (二轮差异总结 §8 状态泄漏)。
pub(crate) struct SessionRunGuard {
    /// 本会话「开了还没关」的命令数。
    open: u32,
    /// 最近一条结构化命令行(OSC 633 E;供共情提交判定)。
    last_cmd: String,
    /// 当前命令开始时刻(共情时长判定;命令在 pane 内顺序执行)。
    start_ms: u64,
}

impl SessionRunGuard {
    pub(crate) fn new() -> Self {
        Self {
            open: 0,
            last_cmd: String::new(),
            start_ms: 0,
        }
    }

    /// OSC 633 `E`:记录将要执行的命令行(只读结构化字段)。
    pub(crate) fn command_line(&mut self, cmd: &str) {
        self.last_cmd = cmd.to_string();
    }

    pub(crate) fn command_start(&mut self) {
        self.open += 1;
        self.start_ms = now_ms();
        signal_command_start();
    }

    pub(crate) fn command_end(&mut self, exit: Option<i32>) {
        self.open = self.open.saturating_sub(1);
        let run_ms = now_ms().saturating_sub(self.start_ms);
        signal_command_end(exit, &self.last_cmd, run_ms);
        self.last_cmd.clear();
    }
}

impl Drop for SessionRunGuard {
    fn drop(&mut self) {
        // 只清计数:不碰 LAST_EXIT_*,免得吞掉别的会话刚发生的 Success/Error 演出。
        for _ in 0..self.open {
            signal_run_released();
        }
    }
}

// ═══════════════════════════ 品种与像素资产 ═══════════════════════════════
//
// 像素图转写自 docs/宠物/原型/0X-*.svg(14×12 viewBox,每 rect = 1 格)。
// 字符 → 颜色:'.' 透明;其余见 [`pixel_color`]。

/// 七犬品种(D01–D07)。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Breed {
    Westie,
    Golden,
    Shepherd,
    Bichon,
    Maltese,
    ShihTzu,
    Poodle,
}

pub(crate) const ALL_BREEDS: [Breed; 7] = [
    Breed::Westie,
    Breed::Golden,
    Breed::Shepherd,
    Breed::Bichon,
    Breed::Maltese,
    Breed::ShihTzu,
    Breed::Poodle,
];

/// 一只狗的静态资产:像素行 + 动效结构点。
struct BreedSprite {
    /// 9 行 × 14 列(y0..y8;y9-11 为空,留给容器底部)。
    rows: [&'static str; 9],
    /// 眼睛格(blink/error 变形点)。
    eyes: [(i32, i32); 2],
    /// 立耳格(Typing 时 +1 格立起);垂耳犬为空。
    ears: &'static [(i32, i32)],
    /// 尾巴格(Idle 慢摆 / Running 快摆)。
    tail: &'static [(i32, i32)],
    /// 主毛色(闭眼衬底色 — SHEET 05-E 方案 A:衬底 + 下缘横缝)。
    fur: u32,
    /// 趴姿网格(SLEEP,SHEET 05-E 审核定稿):腿收起、肚皮贴地(底行 = 岗台)、
    /// 身长 +2 格、头压低 —— 姿态变形,不是位移;每品种保留识别点。
    lie_rows: [&'static str; 9],
    /// 趴姿眼睛格。
    lie_eyes: [(i32, i32); 2],
}

const WESTIE: BreedSprite = BreedSprite {
    rows: [
        "....W....W....",
        "...WPW..WPW...",
        "...WWWWWWWW...",
        "...WWWWWWWW...",
        "...WWKWWKWW...",
        "...WWWKKWWW...",
        "...WWWWWWWWW..",
        "...WWWWWWWW...",
        "....WW..WW....",
    ],
    eyes: [(5, 4), (8, 4)],
    ears: &[(4, 0), (9, 0)],
    tail: &[(11, 6)],
    fur: 0xF4F1E1,
    lie_rows: [
        "..............",
        "..............",
        "....W....W....",
        "...WPW..WPW...",
        "...WWWWWWWW...",
        "...WWKWWKWW...",
        "..WWWWKKWWWW..",
        ".WWWWWWWWWWWW.",
        ".WWWWWWWWWWWW.",
    ],
    lie_eyes: [(5, 5), (8, 5)],
};

const GOLDEN: BreedSprite = BreedSprite {
    rows: [
        "..............",
        "....GGGGGG....",
        "...GGGGGGGG...",
        "..DDGGGGGGDD..",
        "..DDGKGGKGDD..",
        "..DDGGKKGGDD..",
        "..DDGGGPGGDDGG",
        "..DDGGGGGGDD..",
        "....GG..GG....",
    ],
    eyes: [(5, 4), (8, 4)],
    ears: &[], // 垂耳
    tail: &[(12, 6), (13, 6)],
    fur: 0xF2C867,
    lie_rows: [
        "..............",
        "..............",
        "....GGGGGG....",
        "...GGGGGGGG...",
        "..DDGKGGKGDD..",
        "..DDGGKKGGDD..",
        "..DDGGGPGGDD..",
        ".GGGGGGGGGGGG.",
        ".GGGGGGGGGGGGG",
    ],
    lie_eyes: [(5, 4), (8, 4)],
};

const SHEPHERD: BreedSprite = BreedSprite {
    rows: [
        "...BB....BB...",
        "...BTB..BTB...",
        "..BBBBBBBBBB..",
        "..BTTBTTBTTB..",
        "..BTTKTTKTTB..",
        "..BBTTKKTTBB..",
        "..BBBTTTTBBBBB",
        "..BBBBBBBBBB..",
        "....TT..TT....",
    ],
    eyes: [(5, 4), (8, 4)],
    ears: &[(3, 0), (4, 0), (9, 0), (10, 0)],
    tail: &[(12, 6), (13, 6)],
    fur: 0x303338,
    // 立耳犬趴下仍竖耳(识别点),整体比垂耳犬高一行。
    lie_rows: [
        "..............",
        "...BB....BB...",
        "...BTB..BTB...",
        "..BBBBBBBBBB..",
        "..BTTKTTKTTB..",
        "..BBTTKKTTBB..",
        ".BBBBBBBBBBBB.",
        ".BBBBBBBBBBBBB",
        ".TTTTTTTTTTTT.",
    ],
    lie_eyes: [(5, 4), (8, 4)],
};

const BICHON: BreedSprite = BreedSprite {
    rows: [
        "....WWWWWW....",
        "...WWWWWWWW...",
        "..WWWWWWWWWW..",
        "..WWWKWWKWWW..",
        "..WWPWKKWPWW..",
        "..WWWWPPWWWWW.",
        "...WWWWWWWW...",
        "...WWWWWWWW...",
        "....WW..WW....",
    ],
    eyes: [(5, 3), (8, 3)],
    ears: &[], // 圆蓬,无立耳
    tail: &[(12, 5)],
    fur: 0xF4F1E1,
    // 棉花球趴下 = 压扁成椭圆。
    lie_rows: [
        "..............",
        "..............",
        "....WWWWWW....",
        "..WWWWWWWWWW..",
        "..WWWKWWKWWW..",
        "..WWPWKKWPWW..",
        ".WWWWWPPWWWWW.",
        ".WWWWWWWWWWWW.",
        ".WWWWWWWWWWWW.",
    ],
    lie_eyes: [(5, 4), (8, 4)],
};

const MALTESE: BreedSprite = BreedSprite {
    rows: [
        "......RR......",
        ".....RWWR.....",
        "....WWWWWW....",
        "...WWWWWWWW...",
        "..WWWKWWKWWW..",
        "..WWPWKKWPWW..",
        "..WWWWWWWWWWW.",
        "..WWWWWWWWWW..",
        "...WW....WW...",
    ],
    eyes: [(5, 4), (8, 4)],
    ears: &[],
    tail: &[(12, 6)],
    fur: 0xF4F1E1,
    // 蝴蝶结留头顶(识别点),长毛向右铺开。
    lie_rows: [
        "..............",
        "......RR......",
        ".....RWWR.....",
        "....WWWWWW....",
        "..WWWKWWKWWW..",
        "..WWPWKKWPWW..",
        ".WWWWWWWWWWWW.",
        ".WWWWWWWWWWWWW",
        ".WWWWWWWWWWWW.",
    ],
    lie_eyes: [(5, 4), (8, 4)],
};

const SHIH_TZU: BreedSprite = BreedSprite {
    rows: [
        "......AA......",
        ".....AAAA.....",
        "....WWWWWW....",
        "..CCWWWWWWCC..",
        "..CCWKWWKWCC..",
        "..CCPWKKWPCC..",
        "..CCWWWWWWCCWW",
        "..CCWWWWWWCC..",
        "....WW..WW....",
    ],
    eyes: [(5, 4), (8, 4)],
    ears: &[],
    tail: &[(12, 6), (13, 6)],
    fur: 0xF4F1E1,
    // 发饰留头顶,双色脸保留,尾巴平贴右侧。
    lie_rows: [
        "..............",
        "......AA......",
        ".....AAAA.....",
        "....WWWWWW....",
        "..CCWKWWKWCC..",
        "..CCPWKKWPCC..",
        ".CCWWWWWWWWCC.",
        ".CCWWWWWWWWCCW",
        ".WWWWWWWWWWWW.",
    ],
    lie_eyes: [(5, 4), (8, 4)],
};

const POODLE: BreedSprite = BreedSprite {
    rows: [
        "....NNNNNN....",
        "...NNNNNNNN...",
        "..NNNNNNNNNN..",
        "..UUNKNNKNUU..",
        "..UUPNKKNPUU..",
        "..UUNNNNNNUU..",
        "...UNNNNNNU...",
        "...UNNNNNNU...",
        "....NN..NN....",
    ],
    eyes: [(5, 3), (8, 3)],
    ears: &[],
    tail: &[],
    fur: 0x965F3E,
    // 卷毛球压扁,深棕围边保留。
    lie_rows: [
        "..............",
        "..............",
        "....NNNNNN....",
        "..NNNNNNNNNN..",
        "..UUNKNNKNUU..",
        "..UUPNKKNPUU..",
        ".UNNNNNNNNNNU.",
        ".NNNNNNNNNNNN.",
        ".NNNNNNNNNNNN.",
    ],
    lie_eyes: [(5, 4), (8, 4)],
};

/// 像素字符 → 颜色(SVG 原件的 fill 值)。
fn pixel_color(c: char) -> Option<u32> {
    match c {
        'W' => Some(0xF4F1E1), // 白毛
        'P' => Some(0xFFAAAB), // 粉(内耳/腮红/舌头)
        'K' => Some(0x323F49), // 深色(眼/鼻)
        'G' => Some(0xF2C867), // 金毛
        'D' => Some(0xDAA14A), // 金毛垂耳
        'B' => Some(0x303338), // 德牧黑
        'T' => Some(0xC4905E), // 德牧棕
        'R' => Some(0xE36B6B), // 蝴蝶结红
        'A' => Some(0x7EA0F0), // 西施发饰蓝
        'C' => Some(0xC28F6C), // 西施棕
        'N' => Some(0x965F3E), // 泰迪棕
        'U' => Some(0x613922), // 泰迪深棕
        _ => None,
    }
}

impl Breed {
    fn sprite(self) -> &'static BreedSprite {
        match self {
            Breed::Westie => &WESTIE,
            Breed::Golden => &GOLDEN,
            Breed::Shepherd => &SHEPHERD,
            Breed::Bichon => &BICHON,
            Breed::Maltese => &MALTESE,
            Breed::ShihTzu => &SHIH_TZU,
            Breed::Poodle => &POODLE,
        }
    }

    /// 状态栏席位名(等宽大写)。
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Breed::Westie => "WESTIE",
            Breed::Golden => "GOLDEN",
            Breed::Shepherd => "SHEPHERD",
            Breed::Bichon => "BICHON",
            Breed::Maltese => "MALTESE",
            Breed::ShihTzu => "SHIHTZU",
            Breed::Poodle => "POODLE",
        }
    }

    fn name_cn(self) -> &'static str {
        match self {
            Breed::Westie => "西高地",
            Breed::Golden => "金毛",
            Breed::Shepherd => "德牧",
            Breed::Bichon => "比熊",
            Breed::Maltese => "马尔济斯",
            Breed::ShihTzu => "西施",
            Breed::Poodle => "泰迪",
        }
    }

    fn random() -> Breed {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as usize)
            .unwrap_or(0);
        ALL_BREEDS[n % ALL_BREEDS.len()]
    }

    /// 性格参数(规则 D · SHEET 05 板 C/D)。表即实现 —— 只调参数与权重,
    /// 无品种私有代码路径。
    fn personality(self) -> &'static Personality {
        match self {
            Breed::Westie => &P_WESTIE,
            Breed::Golden => &P_GOLDEN,
            Breed::Shepherd => &P_SHEPHERD,
            Breed::Bichon => &P_BICHON,
            Breed::Maltese => &P_MALTESE,
            Breed::ShihTzu => &P_SHIHTZU,
            Breed::Poodle => &P_POODLE,
        }
    }
}

// ═══════════════════════════ 性格系统(规则 D) ════════════════════════════
//
// 参数化性格让「换一只狗」=「换一种陪伴感」。性格只调演出参数与权重,绝不做
// 数值差异(没有「更好的狗」)。常量表 = 实现,新增品种只加一行。

/// 一只狗的性格参数(规则 D SPEC 表逐列)。
struct Personality {
    /// 眨眼间隔区间(ms;Idle 下伪随机取值)。
    blink_min_ms: u64,
    blink_max_ms: u64,
    /// Idle 尾摆基础幅度(px);Running/Play 另有更大固定幅度。
    tail_amp: f32,
    /// 打盹阈值(ms):长空闲超过即趴下(西施最爱睡 45s,德牧站岗 180s)。
    sleep_after_ms: u64,
    /// 口癖气泡(规则 D);空串 = 不出声(德牧:以点头 1 格代替)。
    bark: &'static str,
    /// 六个 idle 微动作权重,顺序同 [`MICRO_ALL`]
    /// (① 抓痒 ② 舔爪 ③ 追尾 ④ 竖耳 ⑤ 望屏外 ⑥ 伸懒腰);
    /// 0 = 该品种不做(如垂耳犬的竖耳听声),避免触发不可见动作。
    micro_weights: [u8; 6],
    /// 每日见面入场四式权重,顺序同 [`ENTRANCE_ALL`]
    /// (探头 / 小跑 / 天降 / 它先到了;规则 A 按性格加权,连续两次不重样)。
    entrance_weights: [u8; 4],
}

// 数值取自规则.md / SHEET 05 板 C 的「性格参数表」。
const P_WESTIE: Personality = Personality {
    blink_min_ms: 4_000,
    blink_max_ms: 6_000,
    tail_amp: 1.0,
    sleep_after_ms: 120_000,
    bark: "汪!",
    micro_weights: [1, 1, 1, 3, 3, 1], // 竖耳听声 · 望屏外
    entrance_weights: [3, 1, 1, 1], // 探头(精神好奇,爱冒头)
};
const P_GOLDEN: Personality = Personality {
    blink_min_ms: 5_000,
    blink_max_ms: 8_000,
    tail_amp: 2.0,
    sleep_after_ms: 90_000,
    bark: "汪~",
    micro_weights: [1, 3, 1, 0, 1, 3], // 伸懒腰 · 舔爪(垂耳:不竖耳)
    entrance_weights: [1, 3, 1, 1], // 小跑(温和可靠,跑来迎你)
};
const P_SHEPHERD: Personality = Personality {
    blink_min_ms: 6_000,
    blink_max_ms: 9_000,
    tail_amp: 1.0,
    sleep_after_ms: 180_000,
    bark: "", // 不出声,点头 1 格
    micro_weights: [1, 1, 1, 4, 1, 1], // 竖耳听声(高频)
    entrance_weights: [2, 2, 1, 1], // 探头 / 小跑(站岗稳重)
};
const P_BICHON: Personality = Personality {
    blink_min_ms: 4_000,
    blink_max_ms: 7_000,
    tail_amp: 2.0,
    sleep_after_ms: 90_000,
    bark: "汪汪!",
    micro_weights: [1, 1, 4, 0, 1, 1], // 追尾转圈(高频)
    entrance_weights: [1, 1, 3, 1], // 天降(开心活泼,蹦出来)
};
const P_MALTESE: Personality = Personality {
    blink_min_ms: 5_000,
    blink_max_ms: 8_000,
    tail_amp: 1.0,
    sleep_after_ms: 60_000,
    bark: "…汪",
    micro_weights: [1, 3, 1, 0, 1, 3], // 舔爪 · 伸懒腰
    entrance_weights: [1, 1, 1, 3], // 它先到了(乖巧爱打盹)
};
const P_SHIHTZU: Personality = Personality {
    blink_min_ms: 6_000,
    blink_max_ms: 10_000,
    tail_amp: 1.0,
    sleep_after_ms: 45_000,
    bark: "呼…",
    micro_weights: [1, 2, 1, 0, 1, 4], // 伸懒腰(高频) · 舔爪
    entrance_weights: [1, 1, 1, 3], // 它先到了(慵懒最爱睡)
};
const P_POODLE: Personality = Personality {
    blink_min_ms: 4_000,
    blink_max_ms: 6_000,
    tail_amp: 3.0,
    sleep_after_ms: 120_000,
    bark: "汪!汪!",
    micro_weights: [3, 1, 3, 0, 1, 1], // 追尾转圈 · 抓痒
    entrance_weights: [1, 1, 3, 1], // 天降(俏皮,爱表演)
};

/// 活物引擎 idle 微动作池(规则 C)。顺序固定,与 [`Personality::micro_weights`]
/// 一一对应。全部由既有 dx/dy/镜像 变换实现,无新网格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Micro {
    Scratch,  // ① 抓痒:后腿格上抬快抖
    Lick,     // ② 舔爪:头低 1 格 + 前爪上抬
    Spin,     // ③ 追尾转圈:整身水平镜像(翻转即转身,零新网格)
    EarPerk,  // ④ 竖耳听声:双耳 +1 格保持
    LookAway, // ⑤ 望屏外:头部右移 1 格
    Stretch,  // ⑥ 伸懒腰:前低后高(头 +1px / 尾 −1px)
}

const MICRO_ALL: [Micro; 6] = [
    Micro::Scratch,
    Micro::Lick,
    Micro::Spin,
    Micro::EarPerk,
    Micro::LookAway,
    Micro::Stretch,
];

/// 每日见面入场池(规则 A · SHEET 05 板 F)。四式按性格加权随机,连续两次不
/// 重样;日常显隐只用轻量二式(探头 / 小跑)。全部由 body_dy/dx 时间线驱动,
/// 复用既有像素网格(它先到了 = 趴姿→起身),无新网格。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entrance {
    Peek,        // 式一:岗台线后升起(既有 peek 通道)
    Run,         // 式二:从右缘小跑入窝(gait + dx 滑入,到位刹车)
    Drop,        // 式三:从容器顶落下,落地三帧律回弹
    AlreadyHere, // 式四:现身即趴睡 → 察觉 → 哈欠 → 起身摇尾
}

const ENTRANCE_ALL: [Entrance; 4] =
    [Entrance::Peek, Entrance::Run, Entrance::Drop, Entrance::AlreadyHere];

impl Entrance {
    /// 入场单式总时长(ms):探头 500 / 小跑 600 / 天降 520 / 它先到 1200。
    fn duration_ms(self) -> u64 {
        match self {
            Entrance::Peek => PEEK_MS,
            Entrance::Run => 600,
            Entrance::Drop => 520,
            Entrance::AlreadyHere => 1200,
        }
    }
}

/// 岗台摆设(规则 G:布置不是经济系统 —— 无货币、无稀有度、无收集)。
/// 暖色非磷光,同一时间最多一个,可随时收起。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Toy {
    Ball,
    Bone,
    Blanket,
}

impl Toy {
    /// 记忆透明短标签(规则 I 词表)。
    fn label(self) -> &'static str {
        match self {
            Toy::Ball => "小球",
            Toy::Bone => "骨头",
            Toy::Blanket => "小毯子",
        }
    }
}

/// 用户主动亲密互动类型(规则 G/H:喂养 favorite_interaction 计数)。
#[derive(Clone, Copy)]
enum Affection {
    Pat,
    Feed,
    Play,
    Call,
}

// ═══════════════════════════ 上下文状态机 ════════════════════════════════

/// 终端上下文(优先级降序;见 docs/宠物/宠物系统规则.md + 小狗家族设计.md
/// 「上下文姿态扩展」)。Play = 双击逗弄;Sleep = 长空闲打盹(低于 Idle 之外
/// 的一切,任何活动即唤醒)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PetContext {
    Drag,
    Feed,
    Play,
    Error,
    Success,
    Running,
    Typing,
    Hover,
    Sleep,
    Idle,
}

impl PetContext {
    fn tag(self) -> &'static str {
        match self {
            PetContext::Drag => "DRAG",
            PetContext::Feed => "FEED",
            PetContext::Play => "PLAY",
            PetContext::Error => "ERROR",
            PetContext::Success => "SUCCESS",
            PetContext::Running => "RUNNING",
            PetContext::Typing => "TYPING",
            PetContext::Hover => "HOVER",
            PetContext::Sleep => "SLEEP",
            PetContext::Idle => "IDLE",
        }
    }
}

/// 双击逗弄的玩耍窗口(设计.md `play`:蹦跳 + 爱心)。
const PLAY_MS: u64 = 1400;
/// 探头入场窗口(SHEET 05-E:从岗台线后升起,全高裁切,~500ms 缓出)。
const PEEK_MS: u64 = 500;
/// idle 微动作单次时长(规则 C:做完 ≤1.5s 回 idle)。略加长到 1.8s,
/// 让动作有起-保持-收的余地、真的看得清(此前 1.5s 一闪而过)。
const MICRO_MS: u64 = 1800;
/// 微动作随机触发间隔下限 / 抖动跨度。原 20–60s 太稀、几乎遇不到;收紧到
/// 12–28s(仍受 ≤2 次/分钟预算约束,不会聒噪),让「活着」真的被看到。
const MICRO_GAP_MIN_MS: u64 = 12_000;
const MICRO_GAP_SPAN_MS: u64 = 16_000;
/// 首个微动作的提前量:启动后 ~7s 就来一个,让用户立刻感到它是活的。
const MICRO_FIRST_MS: u64 = 7_000;
/// 投喂演出总时长(规则 B 板 F:抛接 → 咀嚼 → 爱心 + 摆尾收尾)。
const FEED_MS: u64 = 2200;
/// 饼干像素色(暖棕,非磷光;SHEET 05 板 F #C99052)。
const BISCUIT: u32 = 0xC99052;

// ═══════════════════════════ 持久化(用户状态,不入项目配置) ═══════════════

/// `pet_state.json`(同 ssh_recents/layout 的 `%APPDATA%\Tn` 模式)。
#[derive(Clone, Serialize, Deserialize)]
struct PetState {
    /// 主宠品种(规则 0:领养时定;`None` = 尚未领养,初始化时随机占位)。
    #[serde(default)]
    fixed_breed: Option<Breed>,
    /// 是否已完成领养仪式(规则 0)。`false` → 首次启用时弹领养卡。
    #[serde(default)]
    adopted: bool,
    /// 主宠名字(规则 0;`None` = 用品种名)。≤8 显示字。
    #[serde(default)]
    name: Option<String>,
    /// 羁绊档案(规则 F;仅此三项,永不扩成数值系统)。
    /// 首见自然日 `YYYY-MM-DD`。
    #[serde(default)]
    first_met: Option<String>,
    /// 见面自然日计数(在一起第 N 天)。
    #[serde(default)]
    days_together: u32,
    /// 最近一次见面的本地自然日(每日见面 / days_together 递增判定)。
    #[serde(default)]
    last_seen_date: Option<String>,
    /// 累计喂过的小饼干(规则 F)。
    #[serde(default)]
    treats_fed: u32,
    /// 今日已投喂的自然日(规则 B:每日 1 块,== 今天则今日已喂)。
    #[serde(default)]
    last_treat_date: Option<String>,
    // ── 互动记忆(规则 H:偏好不是画像;小型衰减计数,长期上限固定)──
    /// 摸头 / 投喂 / 逗弄 / 叫名 的衰减计数(favorite_interaction)。
    #[serde(default)]
    fav_pat: u32,
    #[serde(default)]
    fav_feed: u32,
    #[serde(default)]
    fav_play: u32,
    #[serde(default)]
    fav_call: u32,
    /// 常用窝(favorite_perch):屏幕四象限粗分,只记区域不记精确历史。
    /// 0 = 左下 / 1 = 右下 / 2 = 左上 / 3 = 右上。
    #[serde(default)]
    favorite_perch: Option<u8>,
    /// 当前摆设(toy_choice;同一时间最多一个,可随时收起)。
    #[serde(default)]
    toy: Option<Toy>,
    /// 深夜首见 / 深夜投喂的低精度计数(quiet_hours_seen)。
    #[serde(default)]
    quiet_hours_seen: u32,
    /// 最近一次亲密互动的本地日期(last_affection_at;判定久别亲近)。
    #[serde(default)]
    last_affection_at: Option<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    visible: bool,
    #[serde(default)]
    welcome_only: bool,
    /// 栖位:距右 / 距底(px)。
    #[serde(default = "default_right")]
    right: f32,
    #[serde(default = "default_bottom")]
    bottom: f32,
}

fn default_true() -> bool {
    true
}
fn default_right() -> f32 {
    30.0
}
fn default_bottom() -> f32 {
    STATUSBAR_H + 8.0
}

impl Default for PetState {
    fn default() -> Self {
        Self {
            fixed_breed: None,
            adopted: false,
            name: None,
            first_met: None,
            days_together: 0,
            last_seen_date: None,
            treats_fed: 0,
            last_treat_date: None,
            fav_pat: 0,
            fav_feed: 0,
            fav_play: 0,
            fav_call: 0,
            favorite_perch: None,
            toy: None,
            quiet_hours_seen: 0,
            last_affection_at: None,
            enabled: true,
            visible: true,
            welcome_only: false,
            right: default_right(),
            bottom: default_bottom(),
        }
    }
}

impl PetState {
    fn path() -> Option<PathBuf> {
        tn_config::config_dir().map(|d| d.join("pet_state.json"))
    }
    fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }
}

// ═══════════════════════════ 视图 ═══════════════════════════════════════

/// 状态栏「法定席位」数据(SHEET 05 板 A:`WESTIE · IDLE`)。
pub(crate) struct PetSegment {
    pub label: String,
    pub live: bool,
}

/// 拖拽中:起始鼠标位 + 起始栖位。
struct PetDrag {
    start_mouse: (f32, f32),
    start_pos: (f32, f32), // (right, bottom)
    moved: bool,
    /// 长按已触发摸头(规则 G):松手不再当单击「汪」。
    patted: bool,
}

/// 像素小狗 overlay。挂在 workspace root 上(inset 0 的穿透容器,只有小狗
/// 本体与菜单有命中区)。
pub struct PetView {
    cfg: Arc<Loaded>,
    state: PetState,
    breed: Breed,
    ctx: PetContext,
    /// 动画相位:每 tick 翻转(小跑步态 / 尾摆)。
    phase: bool,
    /// 呼吸相位(慢,~2s)。
    breath: bool,
    /// 眨眼窗口终点(ms);0 = 不在眨眼。
    blink_until_ms: u64,
    next_blink_ms: u64,
    /// 歪头(click)窗口终点。
    tilt_until_ms: u64,
    /// 玩耍(双击逗弄)窗口终点(设计.md `play`)。
    play_until_ms: u64,
    /// 探头入场窗口终点(现身/换品种/欢迎切换;规则「探头」)。
    peek_until_ms: u64,
    /// 点头(德牧单击代替「汪」;规则 D「不出声,点头 1 格」)窗口终点。
    nod_until_ms: u64,
    /// 活物引擎(规则 C):当前 idle 微动作 + 其窗口终点。
    micro: Option<Micro>,
    micro_until_ms: u64,
    /// 上一个微动作(避免连续两次同一动作;规则 C)。
    last_micro: Option<Micro>,
    /// 下一次微动作的最早触发时刻(20–60s 随机节拍)。
    next_micro_ms: u64,
    /// 最近两次微动作时刻(打扰预算:≤2 次/分钟)。
    micro_times: [u64; 2],
    /// 性格/微动作/眨眼用的伪随机种子(无外部 rng 依赖)。
    rng: u64,
    /// 最近一次「有事发生」(键入/运行/互动)的时刻;超品种打盹阈值 → Sleep。
    idle_since_ms: u64,
    /// 气泡:文本 + 消散时刻(2s 规则)。
    bubble: Option<(SharedString, u64)>,
    hover: bool,
    drag: Option<PetDrag>,
    menu_open: bool,
    /// 当前 tab 是否欢迎页(welcome_only 模式用;由 workspace 每帧喂入)。
    on_welcome: bool,
    // ── 每日见面(规则 A)──
    /// 当前入场演出 + 起始时刻(None = 已落定);entrance 期间走专用时间线。
    entrance: Option<Entrance>,
    entrance_start_ms: u64,
    /// 上一次入场式(连续两次不重样)。
    last_entrance: Option<Entrance>,
    /// 状态栏「第 N 天」短显终点(里程碑/每日见面落定后 5s)。
    day_badge_until_ms: u64,
    /// 哈欠窗口终点(规则 A 深夜问候 / 规则 B 深夜投喂;张口 600 + 半闭 250)。
    yawn_until_ms: u64,
    // ── 小饼干(规则 B)──
    /// 投喂演出窗口起点 / 终点(抛接→咀嚼→爱心);0 = 不在投喂。
    feed_start_ms: u64,
    feed_until_ms: u64,
    /// 本次投喂是否深夜彩蛋(吃完接哈欠 → 主动趴下)。
    feed_night: bool,
    // ── 主动互动(规则 G)──
    /// 摸头(长按)窗口终点:头低眯眼 + 尾摆 + 爱心。
    pat_until_ms: u64,
    /// 叫名字演出窗口终点 + 上次叫名时刻(10s 冷却)。
    call_until_ms: u64,
    last_call_ms: u64,
    /// 换窝安顿窗口终点(落地回弹 + 嗅地)。
    settle_until_ms: u64,
    /// 久别亲近的额外爱心窗口终点(规则 G:常规演出 + 多一颗心)。
    extra_heart_until_ms: u64,
    /// 玩具子菜单展开;重置互动记忆二次确认浮层。
    toy_menu_open: bool,
    confirm_reset: bool,
    // ── 工作共情(规则 E)──
    /// 最近若干次主动庆祝时刻(共享冷却 ≤2 次/10 分钟)。
    celebrate_times: [u64; 2],
    /// 连败陪伴(规则 E:≥3 连败 → 就近趴下陪着,不演委屈)。
    companion_lie: bool,
    /// 连败陪伴「呜…」是否已出过(同一段连败只一次)。
    moaned: bool,
    /// 上次读到的共情事件时刻(去重,避免同一事件反复演出)。
    last_empathy_ms: u64,
    /// 当前生效的共情装点类型(0 无 / 1 三连胜 / 2 大功告成 / 3 翻盘 / 4 提交)。
    empathy_kind: u64,
    // ── 领养与命名(规则 0)──
    /// 领养卡是否打开(首次启用宠物;一次性)。
    adopt_open: bool,
    /// 领养卡内暂选品种(未落定)。
    adopt_breed: Breed,
    /// 改名 / 领养命名输入缓冲(None = 不在改名)。
    name_editing: Option<String>,
    /// 文本输入焦点(领养卡 / 改名;track_focus + on_key_down 累积)。
    focus: gpui::FocusHandle,
    /// 下一帧需要抓取焦点(浮层刚打开)。
    grab_focus: bool,
}

impl PetView {
    pub fn new(cx: &mut Context<Self>, cfg: Arc<Loaded>) -> Self {
        let state = PetState::load();
        // 品种在终端初始化时一次性决定:固定优先,否则随机(规则)。
        let breed = state.fixed_breed.unwrap_or_else(Breed::random);
        let now = now_ms();
        let view = Self {
            cfg,
            state,
            breed,
            ctx: PetContext::Idle,
            phase: false,
            breath: false,
            blink_until_ms: 0,
            next_blink_ms: now + 4000,
            tilt_until_ms: 0,
            play_until_ms: 0,
            peek_until_ms: now + PEEK_MS, // 初次现身也探头
            nod_until_ms: 0,
            micro: None,
            micro_until_ms: 0,
            last_micro: None,
            next_micro_ms: now + MICRO_FIRST_MS,
            micro_times: [0, 0],
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(1)
                | 1,
            idle_since_ms: now,
            bubble: None,
            hover: false,
            drag: None,
            menu_open: false,
            on_welcome: false,
            entrance: None,
            entrance_start_ms: 0,
            last_entrance: None,
            day_badge_until_ms: 0,
            yawn_until_ms: 0,
            feed_start_ms: 0,
            feed_until_ms: 0,
            feed_night: false,
            pat_until_ms: 0,
            call_until_ms: 0,
            last_call_ms: 0,
            settle_until_ms: 0,
            extra_heart_until_ms: 0,
            toy_menu_open: false,
            confirm_reset: false,
            celebrate_times: [0, 0],
            companion_lie: false,
            moaned: false,
            last_empathy_ms: 0,
            empathy_kind: 0,
            adopt_open: false,
            adopt_breed: breed,
            name_editing: None,
            focus: cx.focus_handle(),
            grab_focus: false,
        };
        let mut view = view;
        // 首次启用宠物 → 一次性领养卡(规则 0)。老用户(adopted=false 但已有
        // fixed_breed/历史)同样走领养卡:预选当前品种 + 默认名,一键领养。
        if view.state.enabled && view.state.visible && !view.state.adopted {
            view.adopt_open = true;
            view.adopt_breed = breed;
            view.name_editing = Some(breed.name_cn().to_string()); // 默认 = 品种名
            view.grab_focus = true;
            Self::spawn_card_focus_driver(cx); // 重绘心跳:把焦点稳稳落到领养卡
        }
        // 动画/上下文 tick:240ms。变化才 notify(空闲时零重绘)。
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this, cx| loop {
            exec.timer(Duration::from_millis(240)).await;
            let alive = this.update(cx, |pet, cx| pet.tick(cx)).is_ok();
            if !alive {
                break;
            }
        })
        .detach();
        // 初次现身:已领养老用户走每日见面入场;未领养则等领养落定后再入场。
        if view.motion_on() {
            if view.state.adopted {
                view.begin_daily_meeting(cx);
            }
            Self::spawn_peek_driver(cx);
        } else {
            view.peek_until_ms = 0;
        }
        view
    }

    /// reduced motion:`[editor] animations = "off"` → 全静帧(可访问性规则)。
    fn motion_on(&self) -> bool {
        self.cfg.config.editor.animations != tn_config::EditorAnimations::Off
    }

    /// 当前可见帧的指纹:任何影响绘制的状态变化都改变它(空闲时恒定 → 零重绘)。
    /// 用单 u64 折叠所有窗口/相位,避免超过元组 PartialEq 的 12 元上限。
    fn frame_fingerprint(&self, now: u64) -> u64 {
        let b = |x: bool| x as u64;
        let mut h = self.ctx as u64;
        h = h * 2 + b(self.phase);
        h = h * 2 + b(self.breath);
        h = h * 2 + b(self.blink_until_ms > now);
        h = h * 2 + b(self.bubble.is_some());
        h = h * 2 + b(self.tilt_until_ms > now);
        h = h * 2 + b(self.peek_until_ms > now);
        h = h * 8 + self.micro.map(|m| m as u64 + 1).unwrap_or(0);
        h = h * 2 + b(self.nod_until_ms > now);
        h = h * 8 + self.entrance.map(|e| e as u64 + 1).unwrap_or(0);
        h = h * 2 + b(self.yawn_until_ms > now);
        h = h * 8 + self.empathy_kind;
        h = h * 2 + b(self.day_badge_until_ms > now);
        h = h * 2 + b(self.companion_lie);
        h = h * 2 + b(self.feed_until_ms > now);
        h = h * 2 + b(self.pat_until_ms > now);
        h = h * 2 + b(self.call_until_ms > now);
        h = h * 2 + b(self.settle_until_ms > now);
        h = h * 2 + b(self.extra_heart_until_ms > now);
        h
    }

    /// 共情共享冷却(规则 E:主动庆祝 ≤2 次/10 分钟,超出静默 —— 惊喜稀释自保护)。
    fn celebrate_budget_ok(&self, now: u64) -> bool {
        self.celebrate_times
            .iter()
            .filter(|t| now.saturating_sub(**t) < 600_000)
            .count()
            < 2
    }

    /// 工作共情(规则 E):消费一次性事件槽,按类型装点演出;受共享冷却限制。
    /// 1 三连胜(双爱心)/ 2 大功告成(连跳+双爱心+口癖)/ 3 翻盘(Success+「!」)
    /// / 4 提交时刻(小庆祝+单爱心)。永不评判:连败陪伴不走这里(只趴下)。
    fn poll_empathy(&mut self, now: u64, _cx: &mut Context<Self>) {
        let ek = EMPATHY_KIND.load(Ordering::Relaxed);
        if ek == 0 {
            return;
        }
        let em = EMPATHY_MS.load(Ordering::Relaxed);
        EMPATHY_KIND.store(0, Ordering::Relaxed); // 一次性消费
        if em == self.last_empathy_ms {
            return;
        }
        self.last_empathy_ms = em;
        if !self.celebrate_budget_ok(now) {
            return; // 超额静默
        }
        self.celebrate_times = [self.celebrate_times[1], now];
        self.empathy_kind = ek;
        self.idle_since_ms = now;
        let bark = self.breed.personality().bark;
        let motion = self.motion_on();
        match ek {
            // 三连胜:Success 跳升级双爱心(复用 Play 的双爱心蹦跳,不冒泡)。
            1 if motion => {
                self.play_until_ms = now + PLAY_MS;
            }
            2 => {
                // 大功告成:连跳两次 + 双爱心 + 口癖。
                if motion {
                    self.play_until_ms = now + PLAY_MS;
                }
                if !bark.is_empty() {
                    self.bubble = Some((bark.into(), now + 1600));
                }
            }
            3 => {
                // 翻盘:普通 Success(exit 0 已触发)+ 气泡「!」(它也松了口气)。
                self.bubble = Some(("!".into(), now + 2000));
            }
            // 4 提交时刻:单爱心由 frame_cells 据 empathy_kind 在 Success 窗口渲染。
            _ => {}
        }
    }

    /// 每 tick:从进程级信号推导上下文 + 推进动画相位;有变化才重绘。
    fn tick(&mut self, cx: &mut Context<Self>) {
        if !self.state.enabled || !self.state.visible {
            return;
        }
        let now = now_ms();
        let old = self.frame_fingerprint(now);

        // 工作共情事件轮询(规则 E):庆祝/翻盘/提交;施加共享冷却。
        self.poll_empathy(now, cx);

        // ── 上下文推导(优先级 Drag > Play > Error/Success > Running > Typing >
        //    Hover > Sleep > Idle;Sleep = 长空闲打盹,任何活动即唤醒)──
        let exit_kind = LAST_EXIT_KIND.load(Ordering::Relaxed);
        let exit_age = now.saturating_sub(LAST_EXIT_MS.load(Ordering::Relaxed));
        // 连败陪伴(规则 E):≥3 连败 → 就近趴下陪着,不演委屈;下个成功自然起身。
        self.companion_lie = FAIL_STREAK.load(Ordering::Relaxed) >= 3;
        if self.companion_lie {
            if !self.moaned {
                self.bubble = Some(("呜…".into(), now + 2000)); // 仅一次(规则 E)
                self.moaned = true;
            }
        } else {
            self.moaned = false;
        }
        let running = RUN_COUNT.load(Ordering::Relaxed) > 0
            && now.saturating_sub(RUN_START_MS.load(Ordering::Relaxed)) > 1000; // >1s 才算(规则)
        let typing = now.saturating_sub(LAST_KEY_MS.load(Ordering::Relaxed)) < 1200;
        self.ctx = if self.drag.as_ref().is_some_and(|d| d.moved) {
            PetContext::Drag // 仅真正拖动才拎起;静止长按留给摸头(规则 G)
        } else if self.pat_until_ms > now {
            PetContext::Idle // 摸头是 Idle 上的姿态修饰(头低眯眼),不抢上下文
        } else if self.feed_until_ms > now {
            PetContext::Feed // 投喂:抛接 + 咀嚼 + 爱心(规则 B)
        } else if self.play_until_ms > now {
            PetContext::Play // 双击逗弄:蹦跳 + 爱心(设计.md `play`)
        } else if exit_kind == 2 && exit_age < 3000 && !self.companion_lie {
            PetContext::Error // 委屈 3s 复原(规则);连败陪伴时不演委屈
        } else if exit_kind == 1 && exit_age < 900 {
            PetContext::Success // 一次性蹦跳后回真实上下文
        } else if running {
            PetContext::Running
        } else if typing {
            PetContext::Typing
        } else if self.hover {
            PetContext::Hover
        } else if self.companion_lie {
            PetContext::Idle // 趴下由 companion_lie 渲染(无 zZ),状态机仍算清醒
        } else if now.saturating_sub(self.idle_since_ms)
            > self.breed.personality().sleep_after_ms
        {
            PetContext::Sleep // 趴下打盹 + zZ(设计.md `sleep`;阈值按品种 — 规则 D)
        } else {
            PetContext::Idle
        };
        // 共情装点只在 Success/Play 演出窗口内有效,过后清除。
        if !matches!(self.ctx, PetContext::Success | PetContext::Play) {
            self.empathy_kind = 0;
        }
        // 任何非纯空闲状态都刷新活动时刻(醒着就不计入打盹倒计时)。
        if !matches!(self.ctx, PetContext::Idle | PetContext::Sleep) {
            self.idle_since_ms = now;
        }
        // 专注保护(规则 C):离开 Idle 立即弃帧 —— 微动作不在 Typing/Running/
        // Hover/Sleep 期间残留。
        if self.ctx != PetContext::Idle {
            self.micro = None;
        }

        if self.motion_on() {
            // 步态/尾摆相位:Running/Play 每 tick 翻;Idle 慢摆(隔 3 tick)。
            if matches!(self.ctx, PetContext::Running | PetContext::Play) {
                self.phase = !self.phase;
            } else if now % 1440 < 240 {
                self.phase = !self.phase;
            }
            // 呼吸:~2s 沉浮(Sleep 复用为 zZ 喘息相位)。
            self.breath = (now / 1920) % 2 == 0;
            // 眨眼:间隔按品种(规则 D 眨眼间隔列),160ms 一帧。
            if self.ctx == PetContext::Idle && now >= self.next_blink_ms {
                self.blink_until_ms = now + 160;
                let p = self.breed.personality();
                let span = (p.blink_max_ms - p.blink_min_ms).max(1);
                self.next_blink_ms = now + p.blink_min_ms + self.next_rand() % span;
            }
            // 活物引擎(规则 C):Idle 下 20–60s 随机触发一个微动作。
            // 专注保护:只在纯 Idle、无探头、无气泡时;打扰预算 ≤2 次/分钟。
            if self.ctx == PetContext::Idle
                && self.micro.is_none()
                && self.peek_until_ms <= now
                && self.bubble.is_none()
                && now >= self.next_micro_ms
            {
                if self.micro_budget_ok(now) {
                    if let Some(m) = self.pick_micro() {
                        self.micro = Some(m);
                        self.micro_until_ms = now + MICRO_MS;
                        self.last_micro = Some(m);
                        self.micro_times = [self.micro_times[1], now];
                        Self::spawn_micro_driver(cx);
                    }
                }
                // 无论是否成功触发,都排下一拍(避免每 tick 重试)。
                self.next_micro_ms = now + MICRO_GAP_MIN_MS + self.next_rand() % MICRO_GAP_SPAN_MS;
            }
        }
        // 微动作到点收尾,回 idle(driver 也会清,这里兜底)。
        if self.micro.is_some() && now >= self.micro_until_ms {
            self.micro = None;
        }
        // 气泡 2s 消散(规则)。
        if let Some((_, until)) = &self.bubble {
            if now >= *until {
                self.bubble = None;
            }
        }

        let new = self.frame_fingerprint(now);
        if new != old {
            cx.notify();
        }
    }

    /// workspace 每帧喂入:当前 tab 是否欢迎页(welcome_only 模式)。
    /// 形态切换(1×↔2×)时探头入场。
    pub(crate) fn set_on_welcome(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.on_welcome != on {
            self.on_welcome = on;
            self.peek(cx);
        }
    }

    /// 状态栏席位:`Some(("WESTIE · IDLE", live))`;关闭系统时显示 PET · OFF
    /// (席位常驻,保证随时可点回来 — 可访问性规则:关闭不丢功能)。
    pub(crate) fn status_segment(&self) -> Option<PetSegment> {
        if !self.state.enabled {
            return Some(PetSegment {
                label: "PET · OFF".into(),
                live: false,
            });
        }
        if !self.state.visible {
            return Some(PetSegment {
                label: format!("{} · 隐", self.display_name()),
                live: false,
            });
        }
        // 每日见面落定后 5s 短显「第 N 天」(规则 A),随后回常态读数。
        if self.day_badge_until_ms > now_ms() {
            return Some(PetSegment {
                label: format!("{} · 第 {} 天", self.display_name(), self.state.days_together),
                live: true,
            });
        }
        // 名字替代品种名上屏(规则 0:「豆豆 · IDLE」)。
        Some(PetSegment {
            label: format!("{} · {}", self.display_name(), self.ctx.tag()),
            live: !matches!(
                self.ctx,
                PetContext::Idle | PetContext::Hover | PetContext::Sleep
            ),
        })
    }

    /// 主宠显示名(规则 0:名字优先,缺省用品种名;≤8 显示字由输入侧保证)。
    fn display_name(&self) -> String {
        self.state
            .name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.breed.name_cn().to_string())
    }

    /// 命令面板「宠物设置」入口:确保宠物可见并打开设置菜单(键盘可达性规则;
    /// 与右键菜单同一菜单 — 双击已让位给玩耍互动)。
    pub(crate) fn open_settings(&mut self, cx: &mut Context<Self>) {
        if !self.state.enabled || !self.state.visible {
            self.peek(cx); // 从隐藏被唤出 = 探头入场
        }
        self.state.enabled = true;
        self.state.visible = true;
        self.menu_open = true;
        cx.notify();
    }

    /// 状态栏席位点击:显隐开关(关闭系统状态下点击 = 重新启用)。
    pub(crate) fn toggle_visible(&mut self, cx: &mut Context<Self>) {
        if !self.state.enabled {
            self.state.enabled = true;
            self.state.visible = true;
        } else {
            self.state.visible = !self.state.visible;
        }
        if self.state.visible {
            // 现身 = 每日见面入场(新的一天走重型演出,否则轻量探头/小跑)。
            self.begin_daily_meeting(cx);
        }
        self.menu_open = false;
        self.state.save();
        cx.notify();
    }

    // ── 每日见面 · 羁绊档案(规则 A / F)──────────────────────────────────

    /// 里程碑词(规则 F:第 7/30/100/365 天;其余 None)。
    fn milestone_word(days: u32) -> Option<&'static str> {
        match days {
            7 => Some("第 7 天!"),
            30 => Some("第 30 天!"),
            100 => Some("100 天啦!"),
            365 => Some("一周年!"),
            _ => None,
        }
    }

    /// 每日见面(规则 A):当日首次现身 → 档案递增 + 入场池 + 时段问候 + 第N天。
    /// 同一自然日只递增一次;reduced motion 只出问候气泡(静音路径)。
    fn begin_daily_meeting(&mut self, cx: &mut Context<Self>) {
        let today = local_now().date_key();
        let new_day = self.state.last_seen_date.as_deref() != Some(today.as_str());
        if new_day {
            // 羁绊档案(规则 F:仅 first_met / days_together,永不扩成数值系统)。
            if self.state.first_met.is_none() {
                self.state.first_met = Some(today.clone());
            }
            self.state.days_together = self.state.days_together.saturating_add(1);
            self.state.last_seen_date = Some(today);
            self.state.save();
        }
        if !self.motion_on() {
            self.greet(new_day); // 静音路径:只出问候气泡
            return;
        }
        // 入场式:新的一天用四式池(性格加权、不重样);日常显隐用轻量二式。
        let ent = self.pick_entrance(new_day);
        self.start_entrance(ent, cx);
        self.greet(new_day);
    }

    /// 问候气泡 + 第N天短显 + 深夜哈欠(规则 A 时段词表 / 规则 F 里程碑)。
    fn greet(&mut self, new_day: bool) {
        let now = now_ms();
        let part = DayPart::now();
        let text: SharedString = match new_day.then(|| Self::milestone_word(self.state.days_together)).flatten() {
            Some(m) => m.into(),
            None => part.greeting().into(),
        };
        self.bubble = Some((text, now + 2000));
        if new_day {
            self.day_badge_until_ms = now + 5000;
            // 深夜首见低精度计数(规则 H quiet_hours_seen)。
            if part.is_night() {
                self.state.quiet_hours_seen = self.state.quiet_hours_seen.saturating_add(1);
                self.state.save();
            }
        }
        // 深夜问候补一个哈欠帧(规则 A:一个哈欠就是全部提醒,不说教)。
        if part.is_night() && self.motion_on() {
            self.yawn_until_ms = now + 850; // 张口 600 + 半闭 250
        }
    }

    /// 今日是否还有小饼干(规则 B:每自然日 1 块,不囤积、不补偿)。
    fn can_feed_today(&self) -> bool {
        self.state.last_treat_date.as_deref() != Some(local_now().date_key().as_str())
    }

    /// 投喂(规则 B):每日一块。抛接 → 咀嚼 → 爱心;深夜彩蛋吃完接哈欠趴下。
    /// 无数值、不囤积;忘了就忘了,明天照常有。
    fn feed_treat(&mut self, cx: &mut Context<Self>) {
        self.menu_open = false;
        if !self.can_feed_today() {
            return; // 今天吃过啦
        }
        let now = now_ms();
        // 档案 + 当日消耗(规则 F / B)。
        self.state.treats_fed = self.state.treats_fed.saturating_add(1);
        self.state.last_treat_date = Some(local_now().date_key());
        let night = DayPart::now().is_night();
        if night {
            self.state.quiet_hours_seen = self.state.quiet_hours_seen.saturating_add(1); // 规则 H
        }
        self.state.save();
        self.idle_since_ms = now;
        // 投喂也是亲密互动(规则 H favorite_interaction;久别亲近多一颗心)。
        let miss = self.note_affection(Affection::Feed);
        if !self.motion_on() {
            // reduced motion:无演出,只记账(可访问性)。
            cx.notify();
            return;
        }
        self.maybe_extra_heart(miss);
        self.feed_start_ms = now;
        self.feed_until_ms = now + FEED_MS;
        self.feed_night = night;
        // 抛接 / 咀嚼需快于 240ms 主 tick → 复用 30ms 入场驱动节拍。
        Self::spawn_feed_driver(cx);
        cx.notify();
    }

    /// 投喂动画驱动:30ms 重绘直到收尾;深夜彩蛋在收尾时接哈欠 + 主动趴下。
    fn spawn_feed_driver(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
            let alive = this
                .update(cx, |pet, cx| {
                    let now = now_ms();
                    if pet.feed_until_ms != 0 && now >= pet.feed_until_ms {
                        pet.feed_until_ms = 0;
                        // 深夜彩蛋(规则 B):吃完哈欠 → 主动趴下(它陪你,但它困了)。
                        if pet.feed_night {
                            pet.yawn_until_ms = now + 850;
                            pet.idle_since_ms =
                                now.saturating_sub(pet.breed.personality().sleep_after_ms + 1);
                        }
                    }
                    cx.notify();
                    pet.feed_until_ms != 0
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    /// 记一次亲密互动(规则 H favorite_interaction:小型衰减计数,长期上限固定)。
    /// 返回是否「久别亲近」(距上次亲密 ≥3 自然日;规则 G)。
    fn note_affection(&mut self, kind: Affection) -> bool {
        const CAP: u32 = 40;
        let today = local_now().date_key();
        let miss = self
            .state
            .last_affection_at
            .as_deref()
            .and_then(date_to_days)
            .zip(date_to_days(&today))
            .map(|(prev, now)| now - prev >= 3)
            .unwrap_or(false);
        // 其余 −1(地板 0),选中 +6(净 +5,封顶):recency-weighted 偏好。
        self.state.fav_pat = self.state.fav_pat.saturating_sub(1);
        self.state.fav_feed = self.state.fav_feed.saturating_sub(1);
        self.state.fav_play = self.state.fav_play.saturating_sub(1);
        self.state.fav_call = self.state.fav_call.saturating_sub(1);
        let slot = match kind {
            Affection::Pat => &mut self.state.fav_pat,
            Affection::Feed => &mut self.state.fav_feed,
            Affection::Play => &mut self.state.fav_play,
            Affection::Call => &mut self.state.fav_call,
        };
        *slot = (*slot + 6).min(CAP);
        self.state.last_affection_at = Some(today);
        self.state.save();
        miss
    }

    /// 记忆透明短标签(规则 I:最多 3 个;无数据 → 调用方显示「还在认识你」)。
    fn memory_labels(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = Vec::new();
        let favs = [
            (self.state.fav_pat, "爱摸头"),
            (self.state.fav_feed, "爱饼干"),
            (self.state.fav_play, "爱逗弄"),
            (self.state.fav_call, "爱叫名"),
        ];
        if let Some((_, l)) = favs.iter().filter(|(c, _)| *c >= 6).max_by_key(|(c, _)| *c) {
            v.push(l);
        }
        if let Some(t) = self.state.toy {
            v.push(t.label());
        }
        if let Some(q) = self.state.favorite_perch {
            v.push(perch_label(q));
        }
        v.truncate(3);
        v
    }

    /// 久别亲近额外爱心(规则 G:常规演出后多一颗心,不出愧疚文案)。
    fn maybe_extra_heart(&mut self, miss: bool) {
        if miss && self.motion_on() {
            self.extra_heart_until_ms = now_ms() + 1300;
        }
    }

    /// 摸头(规则 G:长按 350ms 触发)。头低眯眼 + 尾摆 + 单爱心;2s 内重复
    /// 只延长开心、不叠气泡(词表:无字)。
    fn pat(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let extend = self.pat_until_ms > now;
        self.pat_until_ms = now + 1200;
        self.idle_since_ms = now;
        if !extend {
            let miss = self.note_affection(Affection::Pat);
            self.maybe_extra_heart(miss);
        }
        cx.notify();
    }

    /// 叫名字(规则 G:菜单触发,10s 冷却)。竖耳抬头 → 小跑回应(无长句气泡)。
    fn call_name(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        self.menu_open = false;
        if now.saturating_sub(self.last_call_ms) < 10_000 {
            cx.notify();
            return; // 10s 冷却,避免刷成噪音
        }
        self.last_call_ms = now;
        self.call_until_ms = now + 900;
        self.idle_since_ms = now;
        let miss = self.note_affection(Affection::Call);
        self.maybe_extra_heart(miss);
        cx.notify();
    }

    /// 换窝安顿(规则 G:拖拽松手且移动超阈值)。落地回弹 + 嗅地;记常用窝象限。
    fn settle(&mut self, quadrant: u8, cx: &mut Context<Self>) {
        let now = now_ms();
        self.settle_until_ms = now + 700;
        self.state.favorite_perch = Some(quadrant); // favorite_perch(规则 H,只记区域)
        self.idle_since_ms = now;
        self.state.save();
        cx.notify();
    }

    /// 摆个玩具 / 收起(规则 G:布置不是经济系统;同一时间最多一个)。
    fn set_toy(&mut self, toy: Option<Toy>, cx: &mut Context<Self>) {
        self.state.toy = toy; // toy_choice(规则 H)
        self.state.save();
        self.toy_menu_open = false;
        self.menu_open = false;
        cx.notify();
    }

    /// 重置互动记忆(规则 I:二次确认后清五项偏好;不清名字/品种/档案/窝坐标/开关)。
    fn reset_memory(&mut self, cx: &mut Context<Self>) {
        self.state.fav_pat = 0;
        self.state.fav_feed = 0;
        self.state.fav_play = 0;
        self.state.fav_call = 0;
        self.state.favorite_perch = None;
        self.state.toy = None;
        self.state.quiet_hours_seen = 0;
        self.state.last_affection_at = None;
        self.state.save();
        self.confirm_reset = false;
        self.menu_open = false;
        cx.notify();
    }

    /// 入场式选取(规则 A:性格加权、连续两次不重样)。`full_pool=false` 时
    /// 只在轻量二式(探头 / 小跑)中选,用于日常显隐。
    fn pick_entrance(&mut self, full_pool: bool) -> Entrance {
        let base = self.breed.personality().entrance_weights;
        let mut weights = if full_pool {
            base
        } else {
            [base[0].max(1), base[1].max(1), 0, 0] // 轻量二式
        };
        if let Some(last) = self.last_entrance {
            let i = last as usize;
            let others: u32 = weights
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, w)| *w as u32)
                .sum();
            if others > 0 {
                weights[i] = 0;
            }
        }
        let total: u32 = weights.iter().map(|w| *w as u32).sum();
        if total == 0 {
            return Entrance::Peek;
        }
        let mut r = (self.next_rand() % total as u64) as u32;
        for (i, w) in weights.iter().enumerate() {
            let w = *w as u32;
            if r < w {
                return ENTRANCE_ALL[i];
            }
            r -= w;
        }
        Entrance::Peek
    }

    /// 启动入场演出(探头复用既有线下裁切通道;其余走 30ms 入场驱动)。
    fn start_entrance(&mut self, ent: Entrance, cx: &mut Context<Self>) {
        let now = now_ms();
        self.entrance = Some(ent);
        self.entrance_start_ms = now;
        self.last_entrance = Some(ent);
        self.idle_since_ms = now;
        if ent == Entrance::Peek {
            self.peek_until_ms = now + PEEK_MS;
            Self::spawn_peek_driver(cx);
        }
        Self::spawn_entrance_driver(cx);
    }

    /// 入场动画驱动:30ms 重绘直到该式时长结束,然后落定(自停)。
    fn spawn_entrance_driver(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
            let alive = this
                .update(cx, |pet, cx| {
                    if let Some(ent) = pet.entrance {
                        if now_ms().saturating_sub(pet.entrance_start_ms) >= ent.duration_ms() {
                            pet.entrance = None; // 落定
                        }
                    }
                    cx.notify();
                    pet.entrance.is_some()
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    // ── 互动 ────────────────────────────────────────────────────────────

    fn bark(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        let p = self.breed.personality();
        if p.bark.is_empty() {
            // 德牧:不出声,点头 1 格代替(规则 D)。
            if self.motion_on() {
                self.nod_until_ms = now + 500;
            }
        } else {
            if self.motion_on() {
                self.tilt_until_ms = now + 600; // 歪头杀
            }
            self.bubble = Some((p.bark.into(), now + 2000));
        }
        cx.notify();
    }

    /// 双击逗弄 = 玩耍(设计.md `play`):蹦跳 + 双爱心 + 快速摇尾,1.4s 回真实
    /// 上下文。设置入口不再占双击(BUG发现 #6)— 右键菜单/命令面板/状态栏已可达。
    fn play(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        self.play_until_ms = now + PLAY_MS;
        self.idle_since_ms = now;
        // 逗弄是亲密互动(规则 H favorite_interaction;久别亲近多一颗心)。
        let miss = self.note_affection(Affection::Play);
        self.maybe_extra_heart(miss);
        // Play 反应走品种口癖(规则 D);德牧无声则只演出不冒泡。
        let bark = self.breed.personality().bark;
        self.bubble = (!bark.is_empty()).then(|| (bark.into(), now + 1600));
        cx.notify();
    }

    /// 探头入场(SHEET 05-E 审核定稿):现身/换品种/欢迎切换时,从岗台线后
    /// 升起(线下裁切);500ms 内 30ms 一帧的专用驱动(240ms 主 tick 太粗)。
    /// reduced motion → 直切落定,无动画。
    fn peek(&mut self, cx: &mut Context<Self>) {
        if !self.motion_on() {
            return;
        }
        self.peek_until_ms = now_ms() + PEEK_MS;
        Self::spawn_peek_driver(cx);
    }

    /// 探头动画驱动:30ms 重绘直到窗口结束(自停;多个重叠无害)。
    fn spawn_peek_driver(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
            let alive = this
                .update(cx, |pet, cx| {
                    cx.notify();
                    pet.peek_until_ms > now_ms()
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    /// 伪随机(LCG;眨眼间隔 / 微动作选取用,无外部依赖)。
    fn next_rand(&mut self) -> u64 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.rng >> 33
    }

    /// 打扰预算(规则 C / 节律预算):微动作 ≤2 次/分钟。
    fn micro_budget_ok(&self, now: u64) -> bool {
        self.micro_times
            .iter()
            .filter(|t| now.saturating_sub(**t) < 60_000)
            .count()
            < 2
    }

    /// 按品种性格加权选一个微动作,排除上一个(规则 C:同一动作不连续两次)。
    fn pick_micro(&mut self) -> Option<Micro> {
        let mut weights = self.breed.personality().micro_weights;
        // 排除上次:仅当排除后仍有可选项(避免唯一动作被永久禁掉)。
        if let Some(last) = self.last_micro {
            let i = last as usize;
            let others: u32 = weights
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, w)| *w as u32)
                .sum();
            if others > 0 {
                weights[i] = 0;
            }
        }
        let total: u32 = weights.iter().map(|w| *w as u32).sum();
        if total == 0 {
            return None;
        }
        let mut r = (self.next_rand() % total as u64) as u32;
        for (i, w) in weights.iter().enumerate() {
            let w = *w as u32;
            if r < w {
                return Some(MICRO_ALL[i]);
            }
            r -= w;
        }
        None
    }

    /// 微动作动画驱动:80ms 重绘(抓痒抖动 / 追尾镜像需快于 240ms 主 tick),
    /// 到点清帧自停(reduced motion 不会进到这里)。
    fn spawn_micro_driver(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let alive = this
                .update(cx, |pet, cx| {
                    if pet.micro_until_ms <= now_ms() {
                        pet.micro = None;
                    }
                    cx.notify();
                    pet.micro.is_some()
                })
                .unwrap_or(false);
            if !alive {
                break;
            }
        })
        .detach();
    }

    /// 换个形象(规则 0):品种架直选,只改外观 —— 名字、档案、窝位全部延续,
    /// 零失去焦虑、无送别弹窗。
    fn pick_breed(&mut self, b: Breed, cx: &mut Context<Self>) {
        self.breed = b;
        self.state.fixed_breed = Some(b); // 显式选择 = 固定品种(入用户配置)
        self.state.save();
        self.peek(cx); // 新狗探头入场
        self.menu_open = false;
        cx.notify();
    }

    /// 改名(规则 0:随时可改,档案不动 —— 改名是亲昵,不是重置)。
    fn begin_rename(&mut self, cx: &mut Context<Self>) {
        self.name_editing = Some(self.display_name());
        self.menu_open = false;
        self.grab_focus = true;
        Self::spawn_card_focus_driver(cx);
        cx.notify();
    }

    /// 文本输入卡(领养 / 改名)打开期间的重绘心跳:卡片本身是静态的、不会触发
    /// fingerprint 变化 → 不重绘 → render 里的「每帧重夺焦点」就跑不到。这里在卡片
    /// 打开期间 120ms notify 一次,保证 render 反复执行把焦点稳稳落到输入卡;卡片
    /// 关闭即自停。(reduced motion 也要能改名,故不受 motion 开关影响。)
    fn spawn_card_focus_driver(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let open = this
                .update(cx, |pet, cx| {
                    let open = pet.adopt_open || pet.name_editing.is_some();
                    if open {
                        cx.notify();
                    }
                    open
                })
                .unwrap_or(false);
            if !open {
                break;
            }
        })
        .detach();
    }

    /// 提交名字(领养命名 / 改名共用):≤8 显示字,空 = 用品种名。
    fn commit_name(&mut self, raw: &str, cx: &mut Context<Self>) {
        let name: String = raw.trim().chars().take(8).collect();
        self.state.name = (!name.is_empty()).then_some(name);
        self.state.save();
        cx.notify();
    }

    /// 完成领养(规则 0):落定品种 + 名字,写档案首见日,关卡并入场。
    fn adopt(&mut self, cx: &mut Context<Self>) {
        self.breed = self.adopt_breed;
        self.state.fixed_breed = Some(self.adopt_breed);
        if let Some(raw) = self.name_editing.take() {
            let name: String = raw.trim().chars().take(8).collect();
            self.state.name = (!name.is_empty()).then_some(name);
        }
        self.state.adopted = true;
        self.adopt_open = false;
        self.state.save();
        // 领养即第一次见面(规则 A / F:first_met / 第 1 天)。
        if self.motion_on() {
            self.begin_daily_meeting(cx);
        }
        cx.notify();
    }

    /// 领养卡内选品种:名字字段若仍是默认(空 / 等于旧品种名)则跟随更新。
    fn adopt_pick(&mut self, b: Breed, cx: &mut Context<Self>) {
        let was_default = self
            .name_editing
            .as_deref()
            .map(|s| {
                let t = s.trim();
                t.is_empty() || t == self.adopt_breed.name_cn()
            })
            .unwrap_or(true);
        self.adopt_breed = b;
        if was_default {
            self.name_editing = Some(b.name_cn().to_string());
        }
        cx.notify();
    }

    /// 跳过领养(规则 0:Esc → 缘分狗 + 默认名)。
    fn skip_adopt(&mut self, cx: &mut Context<Self>) {
        self.adopt_breed = Breed::random();
        self.name_editing = None;
        self.adopt(cx);
    }

    /// 领养卡 / 改名输入键(沿命令面板同款:打字累积、Backspace 删、Enter 确认、
    /// Esc 取消;Tab 在领养卡里换下一只品种)。CJK 走 key_char。
    fn on_name_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let m = &ev.keystroke.modifiers;
        let key = ev.keystroke.key.as_str();
        match key {
            "escape" => {
                if self.adopt_open {
                    self.skip_adopt(cx);
                } else {
                    self.name_editing = None;
                    cx.notify();
                }
            }
            "enter" => {
                if self.adopt_open {
                    self.adopt(cx);
                } else if let Some(raw) = self.name_editing.take() {
                    self.commit_name(&raw, cx);
                }
            }
            "tab" if self.adopt_open => {
                let i = ALL_BREEDS
                    .iter()
                    .position(|b| *b == self.adopt_breed)
                    .unwrap_or(0);
                let next = ALL_BREEDS[(i + 1) % ALL_BREEDS.len()];
                self.adopt_pick(next, cx);
            }
            "backspace" => {
                if let Some(buf) = self.name_editing.as_mut() {
                    buf.pop();
                    cx.notify();
                }
            }
            "space" if !m.control && !m.alt => {
                if let Some(buf) = self.name_editing.as_mut() {
                    buf.push(' ');
                    cx.notify();
                }
            }
            k if k.chars().count() == 1 && !m.control && !m.alt => {
                let ch = ev
                    .keystroke
                    .key_char
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(k);
                if let Some(buf) = self.name_editing.as_mut() {
                    // ≤8 显示字(超出忽略;命令面板同款即时反馈)。
                    if buf.chars().count() < 8 {
                        buf.push_str(ch);
                        cx.notify();
                    }
                }
            }
            _ => {}
        }
    }

    /// 输入栏(领养卡 / 改名共用):「名字 <buf>▌」+ ≤8 字提示。
    fn name_field(&self) -> impl IntoElement {
        let buf = self.name_editing.clone().unwrap_or_default();
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.))
            .mt(px(12.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(R_CARD))
            .bg(rgb(L0))
            .border_1()
            .border_color(rgba(H1))
            .font_family(SharedString::from(self.cfg.font().family.clone()))
            .child(div().text_size(px(11.)).text_color(rgb(T2)).child("名字"))
            .child(div().text_size(px(13.)).text_color(rgb(T0)).child(SharedString::from(buf)))
            .child(div().text_size(px(13.)).text_color(rgb(PH)).child("▌"))
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(rgb(T3))
                    .child("中文可用 · ≤8 字"),
            )
    }

    /// 浮层按钮(主/次):主 = 磷光描边强调。
    fn pill(
        label: &'static str,
        primary: bool,
        cb: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .px(px(16.))
            .py(px(5.))
            .rounded(px(R_CHIP))
            .text_size(px(11.))
            .border_1()
            .border_color(rgba(if primary { PH_DIM } else { H1 }))
            .text_color(rgb(if primary { T0 } else { T1 }))
            .bg(rgb(if primary { L2 } else { L1 }))
            .hover(|s| s.bg(rgb(L4)).text_color(rgb(T0)))
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |p, _e, _w, cx| {
                    cx.stop_propagation();
                    cb(p, cx);
                }),
            )
    }

    /// 领养卡(规则 0:浮层家族 L3 + h2 边 + 投影 + 纯色 scrim)。
    fn render_adopt_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let family = SharedString::from(self.cfg.font().family.clone());
        let mut grid = div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(6.))
            .mt(px(10.));
        for b in ALL_BREEDS {
            let on = b == self.adopt_breed;
            grid = grid.child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(3.))
                    .w(px(78.))
                    .py(px(8.))
                    .rounded(px(R_CARD))
                    .border_1()
                    .border_color(rgba(if on { PH_DIM } else { H0 }))
                    .bg(rgb(if on { L2 } else { L1 }))
                    .when(!on, |d| d.hover(|s| s.bg(rgb(L2))))
                    .text_size(px(11.))
                    .text_color(rgb(if on { T0 } else { T1 }))
                    .child(SharedString::from(b.name_cn()))
                    .child(
                        div()
                            .text_size(px(8.))
                            .text_color(rgb(if on { PH } else { T3 }))
                            .child(SharedString::from(b.tag())),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |p, _e, _w, cx| {
                            cx.stop_propagation();
                            p.adopt_pick(b, cx);
                        }),
                    ),
            );
        }
        let card = div()
            .w(px(560.))
            .p(px(16.))
            .rounded(px(crate::style::R_PANEL))
            .border_1()
            .border_color(rgba(H2))
            .bg(col(self.cfg.theme.ui.palette_bg))
            .shadow(crate::style::shadow_float())
            .font_family(family)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(rgb(PH)).child("⌂"))
                    .child(div().text_size(px(13.)).text_color(rgb(T0)).child("领养你的搭档"))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(PH))
                            .child("初次见面"),
                    ),
            )
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(11.))
                    .text_color(rgb(T2))
                    .child("挑一只,或交给缘分 —— 它会一直陪你写码。"),
            )
            .child(grid)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .mt(px(10.))
                    .child(Self::pill(
                        "🎲 交给缘分",
                        false,
                        |p, cx| {
                            let b = Breed::random();
                            p.adopt_pick(b, cx);
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(T3))
                            .child("随机只此一次 — 日常不再有随机入口"),
                    ),
            )
            .child(self.name_field())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .mt(px(14.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(T3))
                            .child("Esc 跳过(缘分狗 + 默认名)"),
                    )
                    .child(div().flex_1())
                    .child(Self::pill("领养", true, |p, cx| p.adopt(cx), cx)),
            );
        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(SCRIM))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|p, ev: &KeyDownEvent, _w, cx| p.on_name_key(ev, cx)))
            .on_scroll_wheel(
                cx.listener(|_p, _e: &gpui::ScrollWheelEvent, _w, cx| cx.stop_propagation()),
            )
            .child(card)
    }

    /// 改名浮层(规则 0:小件,只换名字)。
    fn render_rename_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let card = div()
            .w(px(360.))
            .p(px(16.))
            .rounded(px(crate::style::R_PANEL))
            .border_1()
            .border_color(rgba(H2))
            .bg(col(self.cfg.theme.ui.palette_bg))
            .shadow(crate::style::shadow_float())
            .font_family(SharedString::from(self.cfg.font().family.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .child(div().text_color(rgb(T2)).child("✎"))
                    .child(div().text_size(px(12.)).text_color(rgb(T0)).child("给它改个名字")),
            )
            .child(self.name_field())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .mt(px(14.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(T3))
                            .child("Enter 确定 · Esc 取消"),
                    )
                    .child(div().flex_1())
                    .child(Self::pill(
                        "确定",
                        true,
                        |p, cx| {
                            if let Some(raw) = p.name_editing.take() {
                                p.commit_name(&raw, cx);
                            }
                        },
                        cx,
                    )),
            );
        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(SCRIM))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|p, ev: &KeyDownEvent, _w, cx| p.on_name_key(ev, cx)))
            .on_scroll_wheel(
                cx.listener(|_p, _e: &gpui::ScrollWheelEvent, _w, cx| cx.stop_propagation()),
            )
            .child(card)
    }

    /// 重置互动记忆二次确认(规则 I:清五项偏好,不清名字/品种/档案/窝/开关)。
    fn render_reset_card(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let card = div()
            .w(px(340.))
            .p(px(16.))
            .rounded(px(crate::style::R_PANEL))
            .border_1()
            .border_color(rgba(H2))
            .bg(col(self.cfg.theme.ui.palette_bg))
            .shadow(crate::style::shadow_float())
            .font_family(SharedString::from(self.cfg.font().family.clone()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_p, _e, _w, cx| cx.stop_propagation()),
            )
            .child(div().text_size(px(12.)).text_color(rgb(T0)).child("重置互动记忆?"))
            .child(
                div()
                    .mt(px(8.))
                    .text_size(px(11.))
                    .text_color(rgb(T2))
                    .child("清空它记得的偏好(摸头 / 玩具 / 小窝等)。名字、品种、在一起的天数都不动。"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .mt(px(14.))
                    .child(div().flex_1())
                    .child(Self::pill(
                        "取消",
                        false,
                        |p, cx| {
                            p.confirm_reset = false;
                            cx.notify();
                        },
                        cx,
                    ))
                    .child(Self::pill("清空", true, |p, cx| p.reset_memory(cx), cx)),
            );
        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.))
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(SCRIM))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|p, _e, _w, cx| {
                    p.confirm_reset = false; // 点 scrim = 取消
                    cx.notify();
                }),
            )
            .on_scroll_wheel(
                cx.listener(|_p, _e: &gpui::ScrollWheelEvent, _w, cx| cx.stop_propagation()),
            )
            .child(card)
    }

    // ── 帧合成:状态变形 → quad 列表 ─────────────────────────────────────

    /// 探头是否进行中(painter 据此在岗台线下裁切)。
    fn peeking(&self) -> bool {
        self.peek_until_ms > now_ms()
    }

    /// 当前帧的像素格(格坐标 + 颜色 + 子格修正),供 canvas 直绘。
    /// 返回 (x_cell, y_cell, color, dx_px, dy_px, w_scale, h_scale)。
    fn frame_cells(&self) -> Vec<(i32, i32, u32, f32, f32, f32, f32)> {
        let sp = self.breed.sprite();
        let now = now_ms();
        let motion = self.motion_on();
        let sleeping = self.ctx == PetContext::Sleep;
        // 入场时间线(规则 A 入场池):它先到了 = 现身即趴睡 → 哈欠 → 起身。
        let ent_el = now.saturating_sub(self.entrance_start_ms);
        let already = self.entrance == Some(Entrance::AlreadyHere);
        let already_dur = Entrance::AlreadyHere.duration_ms();
        let already_lie = already && ent_el < already_dur * 55 / 100;
        let already_yawn =
            already && ent_el >= already_dur * 55 / 100 && ent_el < already_dur * 78 / 100;
        // 趴下姿态来源:打盹(zZ)/ 连败陪伴(无 zZ)/ 它先到了的趴睡段。
        let lie = sleeping || self.companion_lie || already_lie;
        // 投喂时间线(规则 B 板 F:抛接 300 → 起跳 ~180 → 咀嚼 420 → 爱心收尾)。
        let feeding = self.ctx == PetContext::Feed;
        let feed_ef = now.saturating_sub(self.feed_start_ms);
        // 闭眼 = SHEET 05-E **方案 A(审核定稿)**:毛色衬底铺满整格 + 眼色 2px
        // 横缝贴下缘 —— 不再让眼格露出透明洞(上一版"没生效"的根因)。
        // reduced motion 下眨眼/摸摸不触发,但趴姿是姿态而非动画,仍闭眼。
        // 摸头眯眼是姿态(reduced motion 仍切眯眼终帧;规则 G)。
        let squint = (motion && (self.blink_until_ms > now || self.ctx == PetContext::Hover))
            || lie
            || self.pat_until_ms > now;
        let tilt = motion && self.tilt_until_ms > now;
        // 趴姿网格(姿态变形,SHEET 05-E):腿收起、肚皮贴岗台、头压低。
        let (rows, eyes): (&[&'static str; 9], &[(i32, i32); 2]) = if lie {
            (&sp.lie_rows, &sp.lie_eyes)
        } else {
            (&sp.rows, &sp.eyes)
        };
        let mut out = Vec::with_capacity(110);

        // 全身偏移(像素):呼吸 / 蹦跳 / 玩耍 / 拎起 / 委屈下沉。
        let mut body_dy = match self.ctx {
            PetContext::Success => -4.0, // 上跳 2 设计像素(≈4 物理 px)
            // 玩耍:逐 tick 蹦跳(高低交替,比 success 单跳更欢)。
            PetContext::Play => {
                if motion && self.phase {
                    -5.0
                } else {
                    -1.0
                }
            }
            PetContext::Drag => -10.0, // 拎起悬空
            PetContext::Error => 2.0,  // 垂头丧气
            // 投喂接物三帧律:仰头 → 预备蹲 → 起跳(ease-out)→ 落地回弹。
            PetContext::Feed => {
                if !motion || feed_ef < 300 {
                    0.0
                } else if feed_ef < 340 {
                    1.5
                } else if feed_ef < 440 {
                    let q = (feed_ef - 340) as f32 / 100.0;
                    -4.0 * (1.0 - (1.0 - q).powi(2))
                } else if feed_ef < 480 {
                    1.0
                } else {
                    0.0
                }
            }
            _ => 0.0,
        };
        if motion && self.ctx == PetContext::Idle && !lie && self.breath {
            body_dy += 1.0; // 呼吸 1px 沉浮
        }
        // 奔跑起伏(gallop bob):整身随步态相位上下颠 ~2px,比单纯换脚更有冲劲。
        if motion && self.ctx == PetContext::Running {
            body_dy += if self.phase { -1.5 } else { 0.5 };
        }
        let mut body_dx = 0.0_f32;
        // 探头入场(SHEET 05-E 审核定稿):从岗台线**后面**升起 —— 全高(9 格)
        // 下沉起步、缓出上浮,线下部分由 painter 裁切;不是可见状态下的位移。
        if self.peek_until_ms > now {
            let p = 1.0 - (self.peek_until_ms - now) as f32 / PEEK_MS as f32;
            let ease = 1.0 - (1.0 - p).powi(3);
            body_dy += (1.0 - ease) * 9.0 * CELL;
        }
        // 入场池其余两式(规则 A):小跑从右缘滑入、天降从顶落下接回弹。
        // (探头走上面 peek 通道;它先到了走 lie/yawn 姿态段。)
        match self.entrance {
            Some(Entrance::Run) => {
                let p = (ent_el as f32 / 600.0).min(1.0);
                let ease = 1.0 - (1.0 - p).powi(3);
                body_dx += (1.0 - ease) * 44.0; // 起步在右 44px → 滑入到位
            }
            Some(Entrance::Drop) => {
                let dur = Entrance::Drop.duration_ms() as f32;
                let p = (ent_el as f32 / dur).min(1.0);
                if p < 0.78 {
                    let q = p / 0.78;
                    let ease = 1.0 - (1.0 - q).powi(3);
                    body_dy += (1.0 - ease) * -60.0; // 从高处落下
                } else {
                    let q = (p - 0.78) / 0.22; // 落地三帧律:压 1.5px → 回正
                    body_dy += 1.5 * (1.0 - q);
                }
            }
            _ => {}
        }
        // 换窝安顿落地回弹(规则 G:松手 → 弹一下 → 嗅地 → 坐好)。
        if motion && self.settle_until_ms > now {
            let sf = 700u64.saturating_sub(self.settle_until_ms - now);
            if sf < 160 {
                body_dy += 2.0 * (1.0 - sf as f32 / 160.0);
            }
        }
        // 叫名字小跑(规则 G:朝用户方向迈 1 格再回窝)。
        if motion && self.call_until_ms > now {
            let cf = 900u64.saturating_sub(self.call_until_ms - now);
            let off = if cf < 300 {
                cf as f32 / 300.0 * CELL
            } else if cf < 600 {
                CELL
            } else {
                (1.0 - (cf - 600) as f32 / 300.0) * CELL
            };
            body_dx -= off; // 朝左(用户)
        }
        // 趴姿呼吸:背部隆起 1px —— 上半身(行 ≤6)上移,行 7 拉高 1px 补缝,
        // 肚皮行(8)贴岗台不动(审核稿吸气帧,无裂缝)。
        let inhale = lie && motion && self.breath;

        // 追尾转圈(Spin)= 绕立轴旋转的 2D 读法:整身宽度按 cos 收放(侧脸最窄)、
        // cos<0 即转到背面 → 水平镜像;同时身体画个小圈(sin 横摆)。比单纯镜像翻面
        // 真实得多 —— 看上去是在原地转圈,而不是「左右横跳」。
        let (spin_ws, spin_flip, spin_sway) = if motion && self.micro == Some(Micro::Spin) {
            let a = (now % 720) as f32 / 720.0 * std::f32::consts::TAU; // 0.72s 一圈
            let c = a.cos();
            (c.abs().max(0.18), c < 0.0, a.sin() * 3.0)
        } else {
            (1.0, false, 0.0)
        };
        body_dx += spin_sway;

        for (y, row) in rows.iter().enumerate() {
            let y = y as i32;
            for (x, ch) in row.chars().enumerate() {
                let x = x as i32;
                let Some(color) = pixel_color(ch) else {
                    continue;
                };
                let mut dx = body_dx;
                let mut dy = body_dy;
                let mut ws = 1.0_f32;
                let mut hs = 1.0_f32;
                let is_eye = eyes.contains(&(x, y));
                let is_ear = !lie && sp.ears.contains(&(x, y));
                let is_tail = !lie && sp.tail.contains(&(x, y));
                let is_leg = !lie && y == 8;
                // 转圈:列向中心列(x=7)收拢 + 单元格变窄 → 整身绕中轴压扁(旋转读法)。
                if spin_ws < 1.0 {
                    ws = spin_ws;
                    dx += (7.0 - x as f32) * CELL * (1.0 - spin_ws);
                }

                if inhale {
                    if y <= 6 {
                        dy -= 1.0;
                    } else if y == 7 {
                        dy -= 1.0;
                        hs = (CELL + 1.0) / CELL; // 拉高补缝
                    }
                }

                // 点头(德牧单击:头部行下沉半格 0.5s — 规则 D「不出声,点头 1 格」)。
                if motion && self.nod_until_ms > now && y <= 5 {
                    dy += CELL * 0.5;
                }
                // 投喂仰头(规则 B 帧 1:狗仰头盯着抛来的饼干)。
                if feeding && feed_ef < 300 && y <= 5 {
                    dy -= 2.0;
                }
                // 摸头(规则 G):头低 1 格(眯眼已在 squint 处理)。
                if motion && self.pat_until_ms > now && y <= 5 {
                    dy += CELL;
                }
                // 叫名字(规则 G):竖耳 + 抬头朝向用户。
                if motion && self.call_until_ms > now {
                    if is_ear {
                        dy -= CELL;
                    } else if y <= 5 {
                        dy -= 1.0;
                    }
                }
                // 换窝安顿嗅地(规则 G:回弹后低头嗅嗅地面)。
                if motion && self.settle_until_ms > now {
                    let sf = 700u64.saturating_sub(self.settle_until_ms - now);
                    if sf >= 200 && y <= 5 {
                        dy += CELL * 0.7;
                    }
                }

                // 活物引擎(规则 C):idle 微动作,全部用既有 dx/dy 变换(追尾的
                // 水平镜像在 out 构建后统一处理)。sub = 快速子相位(抖动/交替)。
                if let Some(m) = self.micro {
                    let sub = (now / 100) % 2 == 0;
                    match m {
                        // ① 抓痒:后腿(右侧腿格)大幅上抬 + 快速抖动;头随之微歪向后腿。
                        Micro::Scratch => {
                            if is_leg && x >= 7 {
                                dy -= 5.0;
                                dx += if sub { 2.0 } else { -2.0 };
                            }
                            if y <= 3 && x >= 7 {
                                dy += 1.0; // 头偏向被挠的一侧
                            }
                        }
                        // ② 舔爪:头低 + 前爪(左侧腿格)抬到嘴边并随子相位「舔」。
                        Micro::Lick => {
                            if y <= 5 {
                                dy += CELL * 0.8;
                            }
                            if is_leg && x < 7 {
                                dy -= 4.0 + if sub { 1.5 } else { 0.0 };
                            }
                        }
                        // ④ 竖耳听声:双耳 +1 格 + 头略抬(垂耳犬权重为 0,不会进到这里)。
                        Micro::EarPerk => {
                            if is_ear {
                                dy -= CELL;
                            } else if y <= 2 {
                                dy -= 1.0;
                            }
                        }
                        // ⑤ 望屏外:头部右移 1 格 + 略抬,像被窗外什么吸引。
                        Micro::LookAway if y <= 5 => {
                            dx += CELL;
                            if y <= 2 {
                                dy -= 1.5;
                            }
                        }
                        // ⑥ 伸懒腰(作揖 play bow):前身压低 + 前爪前伸,后身/尾抬高。
                        Micro::Stretch => {
                            if y <= 3 {
                                dy += 2.5; // 头胸贴地
                            }
                            if is_leg && x < 7 {
                                dx -= 3.0; // 前爪前伸
                            }
                            if is_tail {
                                dy -= 2.5; // 翘臀
                            } else if y >= 6 && x >= 8 {
                                dy -= 1.0; // 后背微抬
                            }
                        }
                        _ => {}
                    }
                }

                // 闭眼(方案 A):先铺毛色衬底整格,再叠眼色 2px 下缘横缝。
                if is_eye && squint && self.ctx != PetContext::Error {
                    out.push((x, y, sp.fur, dx, dy, 1.0, 1.0)); // 衬底
                    out.push((x, y, color, dx, dy + 2.0, 1.0, 2.0 / CELL)); // 下缘缝
                    continue;
                }
                // 委屈眼「- -」:同样衬底,眼色 2px 横条居中(规则;区分于眯眼)。
                if is_eye && self.ctx == PetContext::Error {
                    out.push((x, y, sp.fur, dx, dy, 1.0, 1.0));
                    out.push((x, y, color, dx, dy, 1.0, 2.0 / CELL));
                    continue;
                }
                // Typing:立耳 +1 格(规则「耳朵立起 1px」);只对有立耳的犬。
                if is_ear && self.ctx == PetContext::Typing {
                    dy -= CELL;
                }
                // 耳朵下垂(error)。
                if is_ear && self.ctx == PetContext::Error {
                    dy += CELL * 0.6;
                }
                // 奔跑耳朵扇动:随步态相位上下扑 ~2px(有立耳的犬更明显)。
                if is_ear && self.ctx == PetContext::Running && motion {
                    dy += if self.phase { -2.0 } else { 1.0 };
                }
                // 尾摆:running 快摆 2px,玩耍 3px 最欢;其余按品种基础幅度
                // (规则 D 尾摆幅度列:1/2/3px);趴姿不摆。
                if is_tail && motion {
                    let amp = match self.ctx {
                        PetContext::Play => 3.0,
                        PetContext::Feed if feed_ef >= 480 => 3.0, // 吃到了,快摆收尾
                        PetContext::Running => 2.0,
                        _ => self.breed.personality().tail_amp,
                    };
                    dy += if self.phase { -amp } else { amp };
                }
                // 小跑步态:脚掌前后交替 + 抬腿(规则)。drag 时腿下垂。
                if is_leg {
                    if self.ctx == PetContext::Running && motion {
                        let front = x < 7;
                        dx += if front == self.phase { 3.0 } else { -3.0 }; // 更大步幅
                        if front == self.phase {
                            dy -= 2.0; // 迈出的那对脚抬起,奔跑感更足
                        }
                    }
                    if self.ctx == PetContext::Drag {
                        dy += 3.0; // 悬空腿下垂
                    }
                }
                // 歪头杀:头部行(y ≤ 5)整体错位重绘(规则「头部像素错位」)。
                if tilt && y <= 5 {
                    dx += 3.0;
                    dy += 1.5;
                }
                out.push((x, y, color, dx, dy, ws, hs));
            }
        }
        const HEART: u32 = 0xF08C98; // 像素爱心粉(宠物专属调色,非语义色)
        // Success:头顶冒像素小心;提交时刻(共情 4)改冒单爱心(规则 E)。
        if self.ctx == PetContext::Success {
            let c = if self.empathy_kind == 4 { HEART } else { OK };
            out.push((9, -1, c, 2.0 + body_dx, body_dy, 0.8, 0.8));
        }
        // 玩耍(定稿):头顶双爱心 #F08C98,随相位交替闪(reduced motion 双亮静帧)。
        if self.ctx == PetContext::Play {
            if !motion || self.phase {
                out.push((9, -1, HEART, 2.0 + body_dx, body_dy, 0.8, 0.8));
            }
            if !motion || !self.phase {
                out.push((11, -2, HEART, 1.0 + body_dx, body_dy, 0.65, 0.65));
            }
        }
        // 投喂(规则 B 板 F:抛接 → 咀嚼 → 爱心)。
        if feeding && motion {
            let my = (eyes[0].1 + 1).min(7);
            if feed_ef < 440 {
                // 饼干从右上(菜单方向)抛物线落向嘴边。
                let p = (feed_ef as f32 / 440.0).min(1.0);
                let (sx, sy) = (13.0_f32, -3.0_f32);
                let (mx, myf) = (7.0_f32, my as f32);
                let xf = sx + (mx - sx) * p;
                let yf = sy + (myf - sy) * p - 4.0 * p * (1.0 - p); // 上凸抛物线
                let xc = xf.floor() as i32;
                let yc = yf.floor() as i32;
                out.push((
                    xc,
                    yc,
                    BISCUIT,
                    (xf - xc as f32) * CELL + body_dx,
                    (yf - yc as f32) * CELL + body_dy,
                    0.7,
                    0.7,
                ));
            } else if feed_ef < 900 {
                // 咀嚼:左右腮交替鼓起 1 格(毛色凸出),每拍 ~210ms。
                let cxc = if (feed_ef / 210) % 2 == 0 { 4 } else { 9 };
                out.push((cxc, my, sp.fur, body_dx, body_dy, 0.7, 0.7));
            } else {
                // 收尾:冒一颗爱心(尾巴快摆已在 tail 分支)。
                out.push((9, -1, HEART, 2.0 + body_dx, body_dy, 0.8, 0.8));
            }
        }
        // 摸头(规则 G):头顶单爱心(词表无字,爱心就是回应)。
        if motion && self.pat_until_ms > now {
            out.push((9, -1, HEART, 2.0 + body_dx, body_dy, 0.8, 0.8));
        }
        // 久别亲近(规则 G):常规演出之外多一颗心(不出愧疚文案)。
        if motion && self.extra_heart_until_ms > now {
            out.push((11, -2, HEART, 1.0 + body_dx, body_dy, 0.7, 0.7));
        }
        // 打盹:头顶 zZ(t2 弱灰小方);连败陪伴/它先到了的趴睡不出 zZ。
        if sleeping {
            const ZZ: u32 = 0x69748E; // t2 弱文灰
            let lift = if motion && self.breath { -2.0 } else { 0.0 };
            out.push((11, 3, ZZ, 0.0, 2.0 + lift, 0.5, 0.5));
            out.push((12, 1, ZZ, 3.0, 4.0 + lift * 1.5, 0.65, 0.65));
        }
        // 哈欠(规则 A 深夜问候 / 它先到了察觉段):鼻下张口 → 半闭,接闭眼方案 A。
        let yawn_open = if already_yawn {
            Some(0.7)
        } else if motion && self.yawn_until_ms > now {
            // 总 850ms:张口 600(剩余 >250)→ 半闭 250(剩余 ≤250)。
            Some(if self.yawn_until_ms - now > 250 { 0.7 } else { 0.3 })
        } else {
            None
        };
        if let Some(o) = yawn_open {
            let my = (eyes[0].1 + 1).min(7); // 鼻下一行
            let dy = body_dy + if lie { 0.0 } else { 1.0 };
            out.push((6, my, 0x323F49, body_dx, dy, 0.8, o));
            out.push((7, my, 0x323F49, body_dx + CELL * 0.2, dy, 0.8, o));
        }
        // ③ 追尾转圈(规则 C):转到背面(cos<0)即整身水平镜像 —— 与上面的宽度
        // 收放一起把翻面演成「转过去」,零新网格。镜像 x 与 dx 同时取反。
        if spin_flip {
            for c in out.iter_mut() {
                c.0 = 13 - c.0;
                c.3 = -c.3;
            }
        }
        // 岗台摆设(规则 G:暖色非磷光)。**逗弄(Play)时小狗真的跟玩具互动** ——
        // 球被拍得蹦跳翻滚、骨头被叼到嘴边随身一起跳;平时则安静摆在岗台。
        let playing = motion && self.ctx == PetContext::Play;
        match self.state.toy {
            Some(Toy::Ball) => {
                if playing {
                    // 球被拍起来:与小狗蹦跳反相地上下弹 + 左右滚一点。
                    let by = if self.phase { -9.0 } else { 0.0 };
                    let bx = if self.phase { -3.0 } else { 1.5 };
                    out.push((12, 8, 0xE36B6B, bx, by, 0.9, 0.9));
                } else {
                    out.push((12, 8, 0xE36B6B, 0.0, -1.0, 0.9, 0.9));
                }
            }
            Some(Toy::Bone) => {
                if playing {
                    // 叼着骨头:横在嘴边、随蹦跳(body_dy)一起上下。
                    let my = (eyes[0].1 + 2).min(8);
                    out.push((5, my, 0xEDE6D6, 2.0 + body_dx, body_dy, 0.45, 0.45));
                    out.push((6, my, 0xEDE6D6, body_dx, body_dy, 1.4, 0.45));
                    out.push((8, my, 0xEDE6D6, -1.0 + body_dx, body_dy, 0.45, 0.45));
                } else {
                    out.push((11, 8, 0xEDE6D6, 1.0, 1.0, 0.45, 0.45));
                    out.push((12, 8, 0xEDE6D6, 0.0, 1.0, 0.9, 0.45));
                    out.push((12, 8, 0xEDE6D6, 4.0, 1.0, 0.45, 0.45));
                }
            }
            Some(Toy::Blanket) => {
                for x in 1..5 {
                    out.push((x, 8, 0x6A7A95, 0.0, 3.0, 1.0, 0.5));
                }
            }
            None => {}
        }
        out
    }
}

/// 单格物理边长(px):14 格 × 6 = 84 宽,12 格 × 6 = 72 高(SHEET 05 SPEC)。
const CELL: f32 = 6.0;
/// 容器:固定 100×84(狗 84×72 + 岗台 + 余量),恒定占位杜绝布局抖动。
const BOX_W: f32 = 100.0;
const BOX_H: f32 = 84.0;
/// 雪碧图绘制原点(容器内):水平居中,底部留 10px 给岗台。
const SPRITE_X: f32 = (BOX_W - 14.0 * CELL) / 2.0;
const SPRITE_Y: f32 = BOX_H - 10.0 - 9.0 * CELL; // 9 行内容贴岗台

// 欢迎页 2× 形态不再是静帧贴图:同一只 PetView 以 on_welcome ×2 渲染
// (活状态机 + 完整交互),见 Render 实现(二轮差异总结 §8)。

impl Render for PetView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 穿透根:无监听、无背景 → 不挡终端;只有小狗本体/菜单有命中区。
        let root = div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .right(px(0.))
            .bottom(px(0.));
        let hidden = !self.state.enabled
            || !self.state.visible
            || (self.state.welcome_only && !self.on_welcome);
        if hidden {
            return root;
        }

        // 欢迎页 = 同一只宠物的 **2× 形态**(SHEET 07 `.wperch`):全局只有这一只,
        // hover/单击/拖拽/右键/气泡与 1× 完全同源 —— 此前欢迎页是 sprite_block
        // 静态贴图、零交互、标签硬编码 IDLE(二轮差异总结 §8 的主缺口)。
        let s = if self.on_welcome { 2.0 } else { 1.0 };
        let box_w = BOX_W * s;
        let box_h = BOX_H * s;

        let vw = f32::from(window.viewport_size().width);
        let vh = f32::from(window.viewport_size().height);
        // 栖位钳制在窗内(拖拽换窝后窗口缩小也不丢狗)。
        let right = self.state.right.clamp(2.0, (vw - box_w - 2.0).max(2.0));
        let bottom = self.state.bottom.clamp(
            STATUSBAR_H + 2.0,
            (vh - box_h - 44.0).max(STATUSBAR_H + 2.0),
        );

        let cells = self.frame_cells();
        let dragging = self.drag.is_some();
        // 探头进行中:岗台线以下裁切(SHEET 05-E「从岗台线后升起」)。
        let peek_clip = self.peeking();

        // ── 小狗本体(canvas 逐 quad 直绘;格距/偏移随形态 ×s) ──
        let sprite = canvas(
            |_b, _w, _cx| {},
            move |bounds, _state, window, _cx| {
                let cell = CELL * s;
                let ox = f32::from(bounds.origin.x) + SPRITE_X * s;
                let oy = f32::from(bounds.origin.y) + SPRITE_Y * s;
                // 岗台线(雪碧图底缘):探头时线下不画。
                let shelf = oy + 9.0 * cell;
                for (x, y, color, dx, dy, ws, hs) in &cells {
                    let w = cell * ws;
                    let mut h = cell * hs;
                    let qx = ox + *x as f32 * cell + dx * s + (cell - w) * 0.5;
                    let qy = oy + *y as f32 * cell + dy * s + (cell - h) * 0.5;
                    if peek_clip {
                        if qy >= shelf {
                            continue;
                        }
                        h = h.min(shelf - qy);
                    }
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(px(qx), px(qy)),
                            size: size(px(w), px(h)),
                        },
                        rgb(*color),
                    ));
                }
            },
        )
        .absolute()
        .top(px(0.))
        .left(px(0.))
        .right(px(0.))
        .bottom(px(0.));


        // ── 岗台:1px h1 发丝,左 28px 磷光点睛(SHEET 05 SPEC) ──
        let shelf = div()
            .absolute()
            // 欢迎页 2× 形态:岗台 180 宽居中(SHEET 07 `.wperch` shelf 180);
            // 1× 沿用满箱宽。
            .left(px(if self.on_welcome {
                (box_w - 180.0).max(0.0) / 2.0
            } else {
                0.0
            }))
            .right(px(if self.on_welcome {
                (box_w - 180.0).max(0.0) / 2.0
            } else {
                0.0
            }))
            .bottom(px(8. * s))
            .h(px(1.))
            .bg(rgba(H1))
            .when(!dragging, |d| {
                d.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(0.))
                        .w(px(28.))
                        .h(px(1.))
                        .bg(rgba(PH_DIM)),
                )
            });

        // 欢迎页 2× 标签:活的「品种 · 上下文」读数(mono 9 t3),与状态栏席位
        // 同源 —— 取代硬编码「IDLE(欢迎页 2× 形态)」(图纸注记曾被当文案上屏,
        // 且与状态栏 RUNNING 撞车;差异总结 §8)。
        let welcome_label = self.on_welcome.then(|| {
            div()
                .absolute()
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .flex()
                .justify_center()
                .font_family(SharedString::from(self.cfg.font().family.clone()))
                .text_size(px(9.))
                .text_color(rgb(T3))
                .child(SharedString::from(format!(
                    "{} · {}",
                    self.display_name(),
                    self.ctx.tag()
                )))
        });

        // ── 气泡(L4 + h2 + r4 mono 11;2s 消散) ──
        let bubble = self.bubble.as_ref().map(|(text, _)| {
            div()
                .absolute()
                .top(px(-18.))
                .right(px(-2.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(R_CARD))
                .bg(rgb(L4))
                .border_1()
                .border_color(rgba(H2))
                .font_family(SharedString::from(self.cfg.font().family.clone()))
                .text_size(px(11.))
                .text_color(rgb(T0))
                .child(text.clone())
        });

        // ── 本体容器:固定 100×84(欢迎页 ×2),互动命中区 ──
        let pet_box = div()
            .id("pet")
            .absolute()
            .right(px(right))
            .bottom(px(bottom))
            .w(px(box_w))
            .h(px(box_h))
            .child(sprite)
            .child(shelf)
            .when_some(welcome_label, |d, l| d.child(l))
            .when_some(bubble, |d, b| d.child(b))
            .cursor_pointer()
            .on_hover(cx.listener(|pet, hovered: &bool, _w, cx| {
                if pet.hover != *hovered {
                    pet.hover = *hovered;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|pet, ev: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    // 双击 = 逗弄玩耍(蹦跳+爱心,设计.md `play`;BUG发现 #6:
                    // 设置不再占双击 — 右键/命令面板/状态栏席位可达)。
                    if ev.click_count >= 2 {
                        pet.drag = None;
                        pet.play(cx);
                        return;
                    }
                    pet.menu_open = false;
                    pet.drag = Some(PetDrag {
                        start_mouse: (f32::from(ev.position.x), f32::from(ev.position.y)),
                        start_pos: (pet.state.right, pet.state.bottom),
                        moved: false,
                        patted: false,
                    });
                    // 长按 350ms 未移动 → 摸头(规则 G);移动则转为拖拽换窝。
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(350))
                            .await;
                        let _ = this.update(cx, |pet, cx| {
                            let do_pat = pet
                                .drag
                                .as_ref()
                                .is_some_and(|d| !d.moved && !d.patted);
                            if do_pat {
                                if let Some(d) = pet.drag.as_mut() {
                                    d.patted = true;
                                }
                                pet.pat(cx);
                            }
                        });
                    })
                    .detach();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|pet, _ev: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    pet.menu_open = !pet.menu_open;
                    cx.notify();
                }),
            );

        // ── 右键菜单(浮层家族:L3 这里用 L4 区分小件;SHEET 05 板 D) ──
        let menu = self.menu_open.then(|| {
            let mi = |label: &'static str,
                      glyph: &'static str,
                      glyph_color: u32,
                      cb: Box<dyn Fn(&mut Self, &mut Context<Self>)>,
                      cx: &mut Context<Self>| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(9.))
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(px(R_CHIP))
                    .text_size(px(11.))
                    .text_color(rgb(T1))
                    .hover(|s| s.bg(rgb(L4)).text_color(rgb(T0)))
                    .child(div().text_color(rgb(glyph_color)).child(glyph))
                    .child(label)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |pet, _e, _w, cx| {
                            cx.stop_propagation();
                            cb(pet, cx);
                        }),
                    )
            };
            let sep = || div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(rgba(H1));
            let grp = |label: &'static str| {
                div()
                    .px(px(10.))
                    .pt(px(4.))
                    .pb(px(2.))
                    .text_size(px(9.))
                    .text_color(rgb(T3))
                    .child(label)
            };
            // 档案只读行(规则 F:名字 · 品种 · 在一起第 N 天;平时不展示、不提醒)。
            let day_n = self.state.days_together.max(1);
            // 居中悬浮面板(用户定夺:不再吊在宠物角落,改与领养/改名卡同款 ——
            // 全屏 scrim + 居中卡片;过高在卡内滚动,永不被边栏/视口裁剪)。
            let menu_max_h = (vh - 80.0).max(200.0);
            let mut menu = div()
                .id("pet-menu") // overflow_y_scroll 需 stateful 元素(带 id)
                .w(px(280.))
                .max_h(px(menu_max_h))
                .overflow_y_scroll()
                .p(px(8.))
                .rounded(px(crate::style::R_PANEL))
                .border_1()
                .border_color(rgba(H2))
                .bg(col(self.cfg.theme.ui.palette_bg)) // L3 浮板
                .shadow(crate::style::shadow_float())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_p, _e, _w, cx| cx.stop_propagation()),
                )
                .on_scroll_wheel(
                    cx.listener(|_p, _e: &gpui::ScrollWheelEvent, _w, cx| cx.stop_propagation()),
                )
                // 身份页眉(规则 F 档案):名字 · 品种 · 第 N 天 —— 大字名 + 弱灰副行。
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .px(px(8.))
                        .pt(px(2.))
                        .pb(px(6.))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .child(div().text_color(rgb(PH)).text_size(px(11.)).child("⌂"))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .text_color(rgb(T0))
                                        .child(SharedString::from(self.display_name())),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(rgb(T3))
                                .child(SharedString::from(format!(
                                    "{} · 在一起第 {} 天",
                                    self.breed.name_cn(),
                                    day_n,
                                ))),
                        ),
                )
                .child(sep())
                // 给小饼干(规则 B 首组):今日还有则可点,喂过置灰「今天吃过啦」。
                .child({
                    let can = self.can_feed_today();
                    let mut row = div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(9.))
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(R_CHIP))
                        .text_size(px(11.))
                        .text_color(rgb(if can { T1 } else { T3 }))
                        .child(div().text_color(rgb(if can { BISCUIT } else { T3 })).child("●"))
                        .child(if can { "给小饼干" } else { "今天吃过啦" });
                    if can {
                        row = row
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(rgb(PH))
                                    .child("今日还有 1 块"),
                            )
                            .hover(|s| s.bg(rgb(L4)).text_color(rgb(T0)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|p, _e, _w, cx| {
                                    cx.stop_propagation();
                                    p.feed_treat(cx);
                                }),
                            );
                    }
                    row
                })
                // 记忆透明(规则 I):「它记得:…」最多 3 标签;无数据 → 还在认识你。
                .child({
                    let labels = self.memory_labels();
                    let text = if labels.is_empty() {
                        "还在认识你".to_string()
                    } else {
                        format!("它记得:{}", labels.join(" · "))
                    };
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(9.))
                        .h(px(22.))
                        .px(px(10.))
                        .text_size(px(10.))
                        .text_color(rgb(T2))
                        .child(div().text_color(rgb(PH)).child("✦"))
                        .child(SharedString::from(text))
                })
                .child(sep())
                .child(mi(
                    "叫名字",
                    "♪",
                    T2,
                    Box::new(|p, cx| p.call_name(cx)),
                    cx,
                ))
                .child(grp("摆个玩具"));
            // 玩具四选(规则 G:球 / 骨头 / 毯子 / 收起;无货币、无稀有度、无收集)。
            {
                let opts: [(Option<Toy>, &'static str); 4] = [
                    (Some(Toy::Ball), "球"),
                    (Some(Toy::Bone), "骨头"),
                    (Some(Toy::Blanket), "毯子"),
                    (None, "收起"),
                ];
                let mut row = div()
                    .flex()
                    .flex_row()
                    .gap(px(5.))
                    .px(px(10.))
                    .pb(px(4.));
                for (toy, label) in opts {
                    let on = self.state.toy == toy;
                    row = row.child(
                        div()
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(24.))
                            .rounded(px(R_CHIP))
                            .text_size(px(10.))
                            .border_1()
                            .border_color(rgba(if on { PH_DIM } else { H1 }))
                            .text_color(rgb(if on { T0 } else { T1 }))
                            .when(on, |d| d.bg(rgb(L4)))
                            .hover(|s| s.bg(rgb(L4)).text_color(rgb(T0)))
                            .child(label)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |p, _e, _w, cx| {
                                    cx.stop_propagation();
                                    p.set_toy(toy, cx);
                                }),
                            ),
                    );
                }
                menu = menu.child(row);
            }
            menu = menu
                .child(sep())
                .child(mi(
                    "改名…",
                    "✎",
                    T2,
                    Box::new(|p, cx| p.begin_rename(cx)),
                    cx,
                ))
                .child(sep())
                .child(grp("换个形象"));
            // 品种架内联(七犬直选;当前 = 磷光标)。换个形象只改外观,名字与
            // 档案延续(规则 0);「随机刷新」已废除(持续性老虎机瓦解身份感)。
            for b in ALL_BREEDS {
                let on = b == self.breed;
                menu = menu.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(9.))
                        .h(px(26.))
                        .px(px(10.))
                        .rounded(px(R_CHIP))
                        .text_size(px(11.))
                        .text_color(if on { rgb(T0) } else { rgb(T1) })
                        .when(on, |d| d.bg(rgb(L4)))
                        .hover(|s| s.bg(rgb(L4)).text_color(rgb(T0)))
                        .child(
                            div()
                                .text_color(if on { rgb(PH) } else { rgb(T2) })
                                .child(if on { "●" } else { "○" }),
                        )
                        .child(SharedString::from(b.name_cn()))
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(rgb(T2))
                                .child(SharedString::from(b.tag())),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |pet, _e, _w, cx| {
                                cx.stop_propagation();
                                pet.pick_breed(b, cx);
                            }),
                        ),
                );
            }
            menu = menu
                .child(sep())
                .child(mi(
                    "重置互动记忆…",
                    "↺",
                    ERR,
                    Box::new(|p, cx| {
                        p.confirm_reset = true; // 二次确认(规则 I)
                        p.menu_open = false;
                        cx.notify();
                    }),
                    cx,
                ))
                .child(sep())
                .child(mi(
                    if self.state.welcome_only {
                        "全局显示"
                    } else {
                        "仅欢迎页显示"
                    },
                    "◌",
                    T2,
                    Box::new(|p, cx| {
                        p.state.welcome_only = !p.state.welcome_only;
                        p.state.save();
                        p.menu_open = false;
                        cx.notify();
                    }),
                    cx,
                ))
                .child(mi(
                    "隐藏",
                    "−",
                    T2,
                    Box::new(|p, cx| {
                        p.state.visible = false;
                        p.state.save();
                        p.menu_open = false;
                        cx.notify();
                    }),
                    cx,
                ))
                .child(mi(
                    "关闭宠物系统",
                    "⏻",
                    ERR,
                    Box::new(|p, cx| {
                        p.state.enabled = false;
                        p.state.save();
                        p.menu_open = false;
                        cx.notify();
                    }),
                    cx,
                ));
            // 全屏 scrim + 居中:点空白处或滚轮关闭(卡片本体已 stop_propagation)。
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(SCRIM))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|p, _e, _w, cx| {
                        p.menu_open = false;
                        cx.notify();
                    }),
                )
                .child(menu)
        });

        // 文本输入卡(领养 / 改名)打开期间 **每帧重夺焦点**,而非一次性 grab ——
        // 一次性 grab 偶尔抢不到第一帧、或被宿主重新泊焦抢走,导致「打不出字」
        // (与 workspace 浮层同款修法:focus() 幂等,已聚焦即早退,不会循环)。
        let text_card_open = self.adopt_open || self.name_editing.is_some();
        if text_card_open && (self.grab_focus || !self.focus.is_focused(window)) {
            window.focus(&self.focus);
        }
        self.grab_focus = false;

        // ── 领养卡(规则 0:首次启用宠物的一次性领养仪式;浮层家族)──
        let adopt_card = self.adopt_open.then(|| self.render_adopt_card(cx));
        // ── 改名浮层(规则 0:随时可改,档案不动)──
        let rename_card = (!self.adopt_open && self.name_editing.is_some())
            .then(|| self.render_rename_card(cx));
        // ── 重置互动记忆二次确认(规则 I)──
        let reset_card = self.confirm_reset.then(|| self.render_reset_card(cx));

        // 拖拽中:根容器接管 move/up(离开本体也能继续拖);否则根保持穿透。
        root.child(pet_box)
            .when_some(menu, |d, m| d.child(m))
            .when_some(reset_card, |d, c| d.child(c))
            .when_some(adopt_card, |d, c| d.child(c))
            .when_some(rename_card, |d, c| d.child(c))
            .when(dragging, |d| {
                d.occlude()
                    .on_mouse_move(cx.listener(move |pet, ev: &MouseMoveEvent, _w, cx| {
                        if let Some(drag) = pet.drag.as_mut() {
                            let mx = f32::from(ev.position.x);
                            let my = f32::from(ev.position.y);
                            let dx = mx - drag.start_mouse.0;
                            let dy = my - drag.start_mouse.1;
                            if dx.abs() + dy.abs() > 4.0 {
                                drag.moved = true;
                            }
                            // 鼠标右移 → right 减小;下移 → bottom 减小。
                            // 形态尺寸现算(欢迎页 2×),拖拽中切 tab 也不越界。
                            let s = if pet.on_welcome { 2.0 } else { 1.0 };
                            pet.state.right =
                                (drag.start_pos.0 - dx).clamp(2.0, (vw - BOX_W * s - 2.0).max(2.0));
                            pet.state.bottom = (drag.start_pos.1 - dy).clamp(
                                STATUSBAR_H + 2.0,
                                (vh - BOX_H * s - 44.0).max(STATUSBAR_H + 2.0),
                            );
                            cx.notify();
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |pet, _ev: &MouseUpEvent, _w, cx| {
                            if let Some(drag) = pet.drag.take() {
                                if drag.moved {
                                    // 换窝安顿(规则 G):记常用窝象限 + 落地回弹演出。
                                    let s = if pet.on_welcome { 2.0 } else { 1.0 };
                                    let cxl = vw - pet.state.right - BOX_W * s * 0.5;
                                    let cyt = vh - pet.state.bottom - BOX_H * s * 0.5;
                                    let q = match (cxl < vw * 0.5, cyt < vh * 0.5) {
                                        (true, false) => 0,  // 左下
                                        (false, false) => 1, // 右下
                                        (true, true) => 2,   // 左上
                                        (false, true) => 3,  // 右上
                                    };
                                    pet.settle(q, cx); // settle 内含 state.save()
                                } else if drag.patted {
                                    // 摸头已在长按时处理,松手不再当单击。
                                } else {
                                    pet.bark(cx); // 原地松手 = 单击互动
                                }
                            }
                            cx.notify();
                        }),
                    )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 站姿/趴姿网格的结构守卫:14 列 × 9 行,眼睛格必须落在深色 K 像素上
    /// (SHEET 05-E:闭眼 = 毛色衬底 + 眼色横缝,坐标错位会画飞)。
    #[test]
    fn sprites_are_well_formed() {
        for b in ALL_BREEDS {
            let sp = b.sprite();
            for (i, row) in sp.rows.iter().chain(sp.lie_rows.iter()).enumerate() {
                assert_eq!(
                    row.chars().count(),
                    14,
                    "{:?} row {i} must be 14 cols, got {:?}",
                    b,
                    row
                );
            }
            for (label, rows, eyes) in [
                ("stand", &sp.rows, &sp.eyes),
                ("lie", &sp.lie_rows, &sp.lie_eyes),
            ] {
                for (ex, ey) in eyes.iter() {
                    let ch = rows[*ey as usize].chars().nth(*ex as usize).unwrap();
                    assert_eq!(ch, 'K', "{:?} {label} eye at ({ex},{ey}) must be K", b);
                }
            }
        }
    }

    /// 性格表(规则 D)逐犬完整且自洽:眨眼区间有效、打盹阈值在审核范围内、
    /// 微动作权重非空。表即实现 —— 守卫常量表不被改飞。
    #[test]
    fn personality_table_is_sane() {
        for b in ALL_BREEDS {
            let p = b.personality();
            assert!(
                p.blink_min_ms < p.blink_max_ms,
                "{:?} blink interval must be a valid range",
                b
            );
            assert!(
                (45_000..=180_000).contains(&p.sleep_after_ms),
                "{:?} sleep threshold {} out of審核 range",
                b,
                p.sleep_after_ms
            );
            assert!(
                p.micro_weights.iter().any(|w| *w > 0),
                "{:?} must have at least one idle micro-action",
                b
            );
        }
    }

    /// 每日见面入场池(规则 A):每犬入场权重非空,保证总能选出一式。
    #[test]
    fn entrance_weights_nonempty() {
        for b in ALL_BREEDS {
            assert!(
                b.personality().entrance_weights.iter().any(|w| *w > 0),
                "{:?} must have at least one entrance style",
                b
            );
        }
    }

    /// 时段问候(规则 A 词表)按本地小时分段。
    #[test]
    fn day_part_buckets() {
        assert_eq!(DayPart::from_hour(7).greeting(), "早!");
        assert_eq!(DayPart::from_hour(12).greeting(), "来啦!");
        assert_eq!(DayPart::from_hour(20).greeting(), "晚上好");
        assert_eq!(DayPart::from_hour(2).greeting(), "夜深了…");
        assert!(DayPart::from_hour(23).is_night());
        assert!(!DayPart::from_hour(9).is_night());
    }

    /// 里程碑词(规则 F):只在 7/30/100/365 触发,其余无。
    #[test]
    fn milestones_only_on_anniversaries() {
        assert_eq!(PetView::milestone_word(7), Some("第 7 天!"));
        assert_eq!(PetView::milestone_word(365), Some("一周年!"));
        assert_eq!(PetView::milestone_word(8), None);
        assert_eq!(PetView::milestone_word(1), None);
    }

    /// 久别亲近(规则 G):自然日差按 days_from_civil 计算,跨月正确。
    #[test]
    fn date_diff_counts_natural_days() {
        let a = date_to_days("2026-06-10").unwrap();
        let b = date_to_days("2026-06-13").unwrap();
        assert_eq!(b - a, 3); // ≥3 → 久别
        assert_eq!(
            date_to_days("2026-03-01").unwrap() - date_to_days("2026-02-28").unwrap(),
            1
        );
        assert!(date_to_days("not-a-date").is_none());
    }

    /// 常用窝象限标签(规则 I 词表)覆盖四角。
    #[test]
    fn perch_labels_cover_quadrants() {
        assert_eq!(perch_label(0), "左下角小窝");
        assert_eq!(perch_label(1), "右下角小窝");
        assert_eq!(perch_label(2), "左上角小窝");
        assert_eq!(perch_label(3), "右上角小窝");
    }

    /// 提交时刻(规则 E):仅 `git commit` / `git push` 开头(结构化首词)。
    #[test]
    fn commit_detection_is_first_two_tokens() {
        assert!(cmd_is_commit("git commit -m \"x\""));
        assert!(cmd_is_commit("git push origin main"));
        assert!(!cmd_is_commit("git status"));
        assert!(!cmd_is_commit("cargo test"));
        assert!(!cmd_is_commit("echo git commit"));
    }

    /// 垂耳犬不得对「竖耳听声」赋权(规则 C:0 = 不做该动作),否则会触发
    /// 不可见微动作(无耳格可抬)。EarPerk = [`MICRO_ALL`] 第 4 项(索引 3)。
    #[test]
    fn earless_breeds_dont_perk_ears() {
        assert_eq!(MICRO_ALL[3], Micro::EarPerk);
        for b in ALL_BREEDS {
            if b.sprite().ears.is_empty() {
                assert_eq!(
                    b.personality().micro_weights[3],
                    0,
                    "{:?} has no ears yet weights EarPerk",
                    b
                );
            }
        }
    }

    /// 趴姿肚皮必须贴岗台(底行非空)且头压低(首两行留白 ≥1)——「趴下」的
    /// 姿态变形约束(审核稿:身高 9 → ≤8 行)。
    #[test]
    fn lie_pose_hugs_the_shelf() {
        for b in ALL_BREEDS {
            let sp = b.sprite();
            assert!(
                sp.lie_rows[8].chars().any(|c| c != '.'),
                "{:?} lie pose belly row must touch the shelf",
                b
            );
            assert!(
                sp.lie_rows[0].chars().all(|c| c == '.'),
                "{:?} lie pose must drop the head (row 0 empty)",
                b
            );
        }
    }
}
