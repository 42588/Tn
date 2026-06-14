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
    canvas, div, fill, point, prelude::*, px, rgb, rgba, size, Bounds, Context, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, SharedString, Window,
};
use serde::{Deserialize, Serialize};
use tn_config::Loaded;

use crate::style::{
    col, ERR, H1, H2, L4, OK, PH, PH_DIM, R_CARD, R_CHIP, STATUSBAR_H, T0, T1, T2, T3,
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

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// OSC 133 `D`(CommandFinished):命令结束 + 退出码。
fn signal_command_end(exit: Option<i32>) {
    signal_run_released();
    LAST_EXIT_MS.store(now_ms(), Ordering::Relaxed);
    LAST_EXIT_KIND.store(
        match exit {
            Some(0) => 1,
            Some(_) => 2,
            None => 0, // 无退出码 → 不演出
        },
        Ordering::Relaxed,
    );
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
pub(crate) struct SessionRunGuard(u32);

impl SessionRunGuard {
    pub(crate) fn new() -> Self {
        Self(0)
    }

    pub(crate) fn command_start(&mut self) {
        self.0 += 1;
        signal_command_start();
    }

    pub(crate) fn command_end(&mut self, exit: Option<i32>) {
        self.0 = self.0.saturating_sub(1);
        signal_command_end(exit);
    }
}

impl Drop for SessionRunGuard {
    fn drop(&mut self) {
        // 只清计数:不碰 LAST_EXIT_*,免得吞掉别的会话刚发生的 Success/Error 演出。
        for _ in 0..self.0 {
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

    /// 随机但不与当前重复(手动刷新时换一只的体感更好;单池随机仍是规则允许的)。
    fn random_other(self) -> Breed {
        let mut b = Breed::random();
        if b == self {
            let i = ALL_BREEDS.iter().position(|x| *x == self).unwrap_or(0);
            b = ALL_BREEDS[(i + 1) % ALL_BREEDS.len()];
        }
        b
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
}

// 数值取自规则.md / SHEET 05 板 C 的「性格参数表」。
const P_WESTIE: Personality = Personality {
    blink_min_ms: 4_000,
    blink_max_ms: 6_000,
    tail_amp: 1.0,
    sleep_after_ms: 120_000,
    bark: "汪!",
    micro_weights: [1, 1, 1, 3, 3, 1], // 竖耳听声 · 望屏外
};
const P_GOLDEN: Personality = Personality {
    blink_min_ms: 5_000,
    blink_max_ms: 8_000,
    tail_amp: 2.0,
    sleep_after_ms: 90_000,
    bark: "汪~",
    micro_weights: [1, 3, 1, 0, 1, 3], // 伸懒腰 · 舔爪(垂耳:不竖耳)
};
const P_SHEPHERD: Personality = Personality {
    blink_min_ms: 6_000,
    blink_max_ms: 9_000,
    tail_amp: 1.0,
    sleep_after_ms: 180_000,
    bark: "", // 不出声,点头 1 格
    micro_weights: [1, 1, 1, 4, 1, 1], // 竖耳听声(高频)
};
const P_BICHON: Personality = Personality {
    blink_min_ms: 4_000,
    blink_max_ms: 7_000,
    tail_amp: 2.0,
    sleep_after_ms: 90_000,
    bark: "汪汪!",
    micro_weights: [1, 1, 4, 0, 1, 1], // 追尾转圈(高频)
};
const P_MALTESE: Personality = Personality {
    blink_min_ms: 5_000,
    blink_max_ms: 8_000,
    tail_amp: 1.0,
    sleep_after_ms: 60_000,
    bark: "…汪",
    micro_weights: [1, 3, 1, 0, 1, 3], // 舔爪 · 伸懒腰
};
const P_SHIHTZU: Personality = Personality {
    blink_min_ms: 6_000,
    blink_max_ms: 10_000,
    tail_amp: 1.0,
    sleep_after_ms: 45_000,
    bark: "呼…",
    micro_weights: [1, 2, 1, 0, 1, 4], // 伸懒腰(高频) · 舔爪
};
const P_POODLE: Personality = Personality {
    blink_min_ms: 4_000,
    blink_max_ms: 6_000,
    tail_amp: 3.0,
    sleep_after_ms: 120_000,
    bark: "汪!汪!",
    micro_weights: [3, 1, 3, 0, 1, 1], // 追尾转圈 · 抓痒
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

// ═══════════════════════════ 上下文状态机 ════════════════════════════════

/// 终端上下文(优先级降序;见 docs/宠物/宠物系统规则.md + 小狗家族设计.md
/// 「上下文姿态扩展」)。Play = 双击逗弄;Sleep = 长空闲打盹(低于 Idle 之外
/// 的一切,任何活动即唤醒)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PetContext {
    Drag,
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
/// idle 微动作单次时长(规则 C:做完 ≤1.5s 回 idle)。
const MICRO_MS: u64 = 1500;
/// 微动作随机触发间隔下限 / 抖动跨度(规则 C:20–60s 随机一个)。
const MICRO_GAP_MIN_MS: u64 = 20_000;
const MICRO_GAP_SPAN_MS: u64 = 40_000;

// ═══════════════════════════ 持久化(用户状态,不入项目配置) ═══════════════

/// `pet_state.json`(同 ssh_recents/layout 的 `%APPDATA%\Tn` 模式)。
#[derive(Clone, Serialize, Deserialize)]
struct PetState {
    /// 用户固定的品种;`None` = 每次初始化随机。
    #[serde(default)]
    fixed_breed: Option<Breed>,
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
            next_micro_ms: now + MICRO_GAP_MIN_MS,
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
        };
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
        // 初次现身的探头入场(reduced motion → 直接落定)。
        let mut view = view;
        if view.motion_on() {
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

    /// 每 tick:从进程级信号推导上下文 + 推进动画相位;有变化才重绘。
    fn tick(&mut self, cx: &mut Context<Self>) {
        if !self.state.enabled || !self.state.visible {
            return;
        }
        let now = now_ms();
        let old = (
            self.ctx,
            self.phase,
            self.breath,
            self.blink_until_ms > now,
            self.bubble.is_some(),
            self.tilt_until_ms > now,
            self.peek_until_ms > now,
            self.micro,
            self.nod_until_ms > now,
        );

        // ── 上下文推导(优先级 Drag > Play > Error/Success > Running > Typing >
        //    Hover > Sleep > Idle;Sleep = 长空闲打盹,任何活动即唤醒)──
        let exit_kind = LAST_EXIT_KIND.load(Ordering::Relaxed);
        let exit_age = now.saturating_sub(LAST_EXIT_MS.load(Ordering::Relaxed));
        let running = RUN_COUNT.load(Ordering::Relaxed) > 0
            && now.saturating_sub(RUN_START_MS.load(Ordering::Relaxed)) > 1000; // >1s 才算(规则)
        let typing = now.saturating_sub(LAST_KEY_MS.load(Ordering::Relaxed)) < 1200;
        self.ctx = if self.drag.is_some() {
            PetContext::Drag
        } else if self.play_until_ms > now {
            PetContext::Play // 双击逗弄:蹦跳 + 爱心(设计.md `play`)
        } else if exit_kind == 2 && exit_age < 3000 {
            PetContext::Error // 委屈 3s 复原(规则)
        } else if exit_kind == 1 && exit_age < 900 {
            PetContext::Success // 一次性蹦跳后回真实上下文
        } else if running {
            PetContext::Running
        } else if typing {
            PetContext::Typing
        } else if self.hover {
            PetContext::Hover
        } else if now.saturating_sub(self.idle_since_ms)
            > self.breed.personality().sleep_after_ms
        {
            PetContext::Sleep // 趴下打盹 + zZ(设计.md `sleep`;阈值按品种 — 规则 D)
        } else {
            PetContext::Idle
        };
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

        let new = (
            self.ctx,
            self.phase,
            self.breath,
            self.blink_until_ms > now,
            self.bubble.is_some(),
            self.tilt_until_ms > now,
            self.peek_until_ms > now,
            self.micro,
            self.nod_until_ms > now,
        );
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
                label: format!("{} · 隐", self.breed.tag()),
                live: false,
            });
        }
        Some(PetSegment {
            label: format!("{} · {}", self.breed.tag(), self.ctx.tag()),
            live: !matches!(
                self.ctx,
                PetContext::Idle | PetContext::Hover | PetContext::Sleep
            ),
        })
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
            self.peek(cx); // 现身 = 探头入场
        }
        self.menu_open = false;
        self.state.save();
        cx.notify();
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

    fn refresh_random(&mut self, cx: &mut Context<Self>) {
        // 手动刷新走随机策略,不固定轮换(规则);不写 fixed_breed。
        self.breed = self.breed.random_other();
        self.menu_open = false;
        self.peek(cx); // 新狗探头入场
        self.bubble = Some((SharedString::from(self.breed.name_cn()), now_ms() + 2000));
        cx.notify();
    }

    fn pick_breed(&mut self, b: Breed, cx: &mut Context<Self>) {
        self.breed = b;
        self.state.fixed_breed = Some(b); // 显式选择 = 固定品种(入用户配置)
        self.state.save();
        self.peek(cx); // 新狗探头入场
        self.menu_open = false;
        cx.notify();
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
        // 闭眼 = SHEET 05-E **方案 A(审核定稿)**:毛色衬底铺满整格 + 眼色 2px
        // 横缝贴下缘 —— 不再让眼格露出透明洞(上一版"没生效"的根因)。
        // reduced motion 下眨眼/摸摸不触发,但 Sleep 是姿态而非动画,仍闭眼。
        let squint =
            (motion && (self.blink_until_ms > now || self.ctx == PetContext::Hover)) || sleeping;
        let tilt = motion && self.tilt_until_ms > now;
        // 打盹用趴姿网格(姿态变形,SHEET 05-E):腿收起、肚皮贴岗台、头压低。
        let (rows, eyes): (&[&'static str; 9], &[(i32, i32); 2]) = if sleeping {
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
            _ => 0.0,
        };
        if motion && self.ctx == PetContext::Idle && self.breath {
            body_dy += 1.0; // 呼吸 1px 沉浮
        }
        // 探头入场(SHEET 05-E 审核定稿):从岗台线**后面**升起 —— 全高(9 格)
        // 下沉起步、缓出上浮,线下部分由 painter 裁切;不是可见状态下的位移。
        if self.peek_until_ms > now {
            let p = 1.0 - (self.peek_until_ms - now) as f32 / PEEK_MS as f32;
            let ease = 1.0 - (1.0 - p).powi(3);
            body_dy += (1.0 - ease) * 9.0 * CELL;
        }
        // 趴姿呼吸:背部隆起 1px —— 上半身(行 ≤6)上移,行 7 拉高 1px 补缝,
        // 肚皮行(8)贴岗台不动(审核稿吸气帧,无裂缝)。
        let inhale = sleeping && motion && self.breath;

        for (y, row) in rows.iter().enumerate() {
            let y = y as i32;
            for (x, ch) in row.chars().enumerate() {
                let x = x as i32;
                let Some(color) = pixel_color(ch) else {
                    continue;
                };
                let mut dx = 0.0_f32;
                let mut dy = body_dy;
                let ws = 1.0_f32;
                let mut hs = 1.0_f32;
                let is_eye = eyes.contains(&(x, y));
                let is_ear = !sleeping && sp.ears.contains(&(x, y));
                let is_tail = !sleeping && sp.tail.contains(&(x, y));
                let is_leg = !sleeping && y == 8;

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

                // 活物引擎(规则 C):idle 微动作,全部用既有 dx/dy 变换(追尾的
                // 水平镜像在 out 构建后统一处理)。sub = 快速子相位(抖动/交替)。
                if let Some(m) = self.micro {
                    let sub = (now / 110) % 2 == 0;
                    match m {
                        // ① 抓痒:后腿(右侧腿格)上抬并快速抖动。
                        Micro::Scratch if is_leg && x >= 7 => {
                            dy -= 3.0;
                            dx += if sub { 1.0 } else { -1.0 };
                        }
                        // ② 舔爪:头低 1 格 + 前爪(左侧腿格)上抬。
                        Micro::Lick => {
                            if y <= 5 {
                                dy += CELL;
                            }
                            if is_leg && x < 7 {
                                dy -= 2.0;
                            }
                        }
                        // ④ 竖耳听声:双耳 +1 格(垂耳犬权重为 0,不会进到这里)。
                        Micro::EarPerk if is_ear => dy -= CELL,
                        // ⑤ 望屏外:头部右移 1 格。
                        Micro::LookAway if y <= 5 => dx += CELL,
                        // ⑥ 伸懒腰:前低后高 —— 头顶行 +1px 下压,尾巴 −1px 上扬。
                        Micro::Stretch => {
                            if y <= 2 {
                                dy += 1.0;
                            }
                            if is_tail {
                                dy -= 1.0;
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
                // 尾摆:running 快摆 2px,玩耍 3px 最欢;其余按品种基础幅度
                // (规则 D 尾摆幅度列:1/2/3px);趴姿不摆。
                if is_tail && motion {
                    let amp = match self.ctx {
                        PetContext::Play => 3.0,
                        PetContext::Running => 2.0,
                        _ => self.breed.personality().tail_amp,
                    };
                    dy += if self.phase { -amp } else { amp };
                }
                // 小跑步态:脚掌前后交替(规则)。drag 时腿下垂。
                if is_leg {
                    if self.ctx == PetContext::Running && motion {
                        dx += if (x < 7) == self.phase { 2.0 } else { -2.0 };
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
        // Success:头顶冒像素小心(ok 色 ~5×5,SHEET 05 `.updot`)。
        if self.ctx == PetContext::Success {
            out.push((9, -1, OK, 2.0, body_dy, 0.8, 0.8));
        }
        // 玩耍(定稿):头顶双爱心 #F08C98,随相位交替闪(reduced motion 双亮静帧)。
        if self.ctx == PetContext::Play {
            const HEART: u32 = 0xF08C98; // 像素爱心粉(宠物专属调色,非语义色)
            if !motion || self.phase {
                out.push((9, -1, HEART, 2.0, body_dy, 0.8, 0.8));
            }
            if !motion || !self.phase {
                out.push((11, -2, HEART, 1.0, body_dy, 0.65, 0.65));
            }
        }
        // 打盹:头顶 zZ(t2 弱灰小方,3px/4px,随呼吸相位上浮 — 审核稿)。
        if sleeping {
            const ZZ: u32 = 0x69748E; // t2 弱文灰
            let lift = if motion && self.breath { -2.0 } else { 0.0 };
            out.push((11, 3, ZZ, 0.0, 2.0 + lift, 0.5, 0.5));
            out.push((12, 1, ZZ, 3.0, 4.0 + lift * 1.5, 0.65, 0.65));
        }
        // ③ 追尾转圈(规则 C):整身水平镜像 —— 翻转即「转身」,零新网格。
        // 用慢子相位在 1.5s 内翻 ~2 次;镜像 x 与 dx 同时取反。
        if motion && self.micro == Some(Micro::Spin) && (now / 360) % 2 == 1 {
            for c in out.iter_mut() {
                c.0 = 13 - c.0;
                c.3 = -c.3;
            }
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
                    self.breed.tag(),
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
                    });
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
            let mut menu = div()
                .absolute()
                .right(px(right + box_w - 10.))
                .bottom(px(bottom + 6.))
                .w(px(190.))
                .p(px(5.))
                .rounded(px(crate::style::R_PANEL))
                .border_1()
                .border_color(rgba(H2))
                .bg(col(self.cfg.theme.ui.palette_bg)) // L3 浮板
                .shadow(crate::style::shadow_float())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_p, _e, _w, cx| cx.stop_propagation()),
                )
                .child(mi(
                    "随机刷新",
                    "↻",
                    PH,
                    Box::new(|p, cx| p.refresh_random(cx)),
                    cx,
                ))
                .child(sep());
            // 品种架内联(七犬直选;当前 = 磷光标)。
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
            menu
        });

        // 拖拽中:根容器接管 move/up(离开本体也能继续拖);否则根保持穿透。
        root.child(pet_box)
            .when_some(menu, |d, m| d.child(m))
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
                        cx.listener(|pet, _ev: &MouseUpEvent, _w, cx| {
                            if let Some(drag) = pet.drag.take() {
                                if drag.moved {
                                    pet.state.save(); // 落点入用户配置(规则)
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
