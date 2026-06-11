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
    /// 主毛色(blink 时盖住眼睛的颜色)。
    fur: u32,
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
}

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

/// 长空闲(无键入/无运行/无互动)超过此时长 → Sleep 打盹(设计.md `sleep`)。
const SLEEP_AFTER_MS: u64 = 90_000;
/// 双击逗弄的玩耍窗口(设计.md `play`:蹦跳 + 爱心)。
const PLAY_MS: u64 = 1400;
/// 探头入场窗口(现身/换品种/欢迎页切换时从岗台后冒出,规则「探头」)。
const PEEK_MS: u64 = 450;

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
    /// 最近一次「有事发生」(键入/运行/互动)的时刻;超 [`SLEEP_AFTER_MS`] → Sleep。
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
        } else if now.saturating_sub(self.idle_since_ms) > SLEEP_AFTER_MS {
            PetContext::Sleep // 趴下打盹 + zZ(设计.md `sleep`)
        } else {
            PetContext::Idle
        };
        // 任何非纯空闲状态都刷新活动时刻(醒着就不计入打盹倒计时)。
        if !matches!(self.ctx, PetContext::Idle | PetContext::Sleep) {
            self.idle_since_ms = now;
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
            // 眨眼:4–8s 随机间隔,130ms 一帧(规则)。
            if self.ctx == PetContext::Idle && now >= self.next_blink_ms {
                self.blink_until_ms = now + 160;
                self.next_blink_ms = now + 4000 + (now % 4000); // 4–8s 伪随机
            }
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
        );
        if new != old {
            cx.notify();
        }
    }

    /// workspace 每帧喂入:当前 tab 是否欢迎页(welcome_only 模式)。
    /// 形态切换(1×↔2×)时探头入场。
    pub(crate) fn set_on_welcome(&mut self, on: bool) {
        if self.on_welcome != on {
            self.peek();
        }
        self.on_welcome = on;
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
            self.peek(); // 从隐藏被唤出 = 探头入场
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
            self.peek(); // 现身 = 探头入场
        }
        self.menu_open = false;
        self.state.save();
        cx.notify();
    }

    // ── 互动 ────────────────────────────────────────────────────────────

    fn bark(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        if self.motion_on() {
            self.tilt_until_ms = now + 600; // 歪头杀
        }
        self.bubble = Some(("汪!".into(), now + 2000));
        cx.notify();
    }

    /// 双击逗弄 = 玩耍(设计.md `play`):蹦跳 + 双爱心 + 快速摇尾,1.4s 回真实
    /// 上下文。设置入口不再占双击(BUG发现 #6)— 右键菜单/命令面板/状态栏已可达。
    fn play(&mut self, cx: &mut Context<Self>) {
        let now = now_ms();
        self.play_until_ms = now + PLAY_MS;
        self.idle_since_ms = now;
        self.bubble = Some(("汪汪!".into(), now + 1600));
        cx.notify();
    }

    /// 探头入场(规则「探头」):现身/换品种/欢迎页切换时从岗台后冒出。
    fn peek(&mut self) {
        if self.motion_on() {
            self.peek_until_ms = now_ms() + PEEK_MS;
        }
    }

    fn refresh_random(&mut self, cx: &mut Context<Self>) {
        // 手动刷新走随机策略,不固定轮换(规则);不写 fixed_breed。
        self.breed = self.breed.random_other();
        self.menu_open = false;
        self.peek(); // 新狗探头入场
        self.bubble = Some((SharedString::from(self.breed.name_cn()), now_ms() + 2000));
        cx.notify();
    }

    fn pick_breed(&mut self, b: Breed, cx: &mut Context<Self>) {
        self.breed = b;
        self.state.fixed_breed = Some(b); // 显式选择 = 固定品种(入用户配置)
        self.state.save();
        self.peek(); // 新狗探头入场
        self.menu_open = false;
        cx.notify();
    }

    // ── 帧合成:状态变形 → quad 列表 ─────────────────────────────────────

    /// 当前帧的像素格(格坐标 + 颜色 + 子格偏移修正),供 canvas 直绘。
    /// 返回 (x_cell, y_cell, color, dx_px, dy_px, h_scale)。
    fn frame_cells(&self) -> Vec<(i32, i32, u32, f32, f32, f32)> {
        let sp = self.breed.sprite();
        let now = now_ms();
        let motion = self.motion_on();
        // 闭眼(眨眼/摸摸/打盹)统一为「快乐眯眼 ^^」:眼格压成下缘横缝,而不是
        // 盖毛色让眼睛凭空消失(BUG发现 #6:无眼很诡异)。reduced motion 下
        // 眨眼/摸摸不触发,但 Sleep 是姿态而非动画,仍闭眼。
        let squint = (motion && (self.blink_until_ms > now || self.ctx == PetContext::Hover))
            || self.ctx == PetContext::Sleep;
        let tilt = motion && self.tilt_until_ms > now;
        let mut out = Vec::with_capacity(96);

        // 全身偏移(像素):呼吸 / 蹦跳 / 玩耍 / 拎起 / 委屈下沉 / 打盹趴下。
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
            PetContext::Sleep => 3.0,  // 趴下贴地(设计.md `sleep`)
            _ => 0.0,
        };
        if motion && self.ctx == PetContext::Idle && self.breath {
            body_dy += 1.0; // 呼吸 1px 沉浮
        }
        // 探头入场:从岗台后冒出(450ms 上浮;规则「探头」)。
        if motion && self.peek_until_ms > now {
            let remain = (self.peek_until_ms - now) as f32 / PEEK_MS as f32;
            body_dy += remain * 6.0;
        }

        for (y, row) in sp.rows.iter().enumerate() {
            let y = y as i32;
            for (x, ch) in row.chars().enumerate() {
                let x = x as i32;
                let Some(color) = pixel_color(ch) else {
                    continue;
                };
                let mut dx = 0.0_f32;
                let mut dy = body_dy;
                let mut hs = 1.0_f32; // 高度比例(眯眼/委屈眼用)
                let is_eye = sp.eyes.contains(&(x, y));
                let is_ear = sp.ears.contains(&(x, y));
                let is_tail = sp.tail.contains(&(x, y));
                let is_leg = y == 8;

                // 快乐眯眼 ^^:眼格压成 35% 高、沉到格底的深色横缝(闭眼仍有
                // 眼线,不再「眼睛没了」)。
                if is_eye && squint && self.ctx != PetContext::Error {
                    hs = 0.35;
                    dy += 2.0;
                }
                // 委屈眼「- -」:眼格压成 40% 高的横条(规则)。
                if is_eye && self.ctx == PetContext::Error {
                    hs = 0.4;
                    dy += 2.0;
                }
                // Typing:立耳 +1 格(规则「耳朵立起 1px」);只对有立耳的犬。
                if is_ear && self.ctx == PetContext::Typing {
                    dy -= CELL;
                }
                // 耳朵下垂(error);打盹耳朵微塌。
                if is_ear && self.ctx == PetContext::Error {
                    dy += CELL * 0.6;
                }
                if is_ear && self.ctx == PetContext::Sleep {
                    dy += CELL * 0.4;
                }
                // 尾摆:idle 慢摆 1px,running 快摆 2px,玩耍 3px 最欢;打盹不摆。
                if is_tail && motion && self.ctx != PetContext::Sleep {
                    let amp = match self.ctx {
                        PetContext::Play => 3.0,
                        PetContext::Running => 2.0,
                        _ => 1.0,
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
                out.push((x, y, color, dx, dy, hs));
            }
        }
        // Success:头顶冒像素小心(ok 色 5×5,SHEET 05 `.updot`)。
        if self.ctx == PetContext::Success {
            out.push((9, -1, OK, 2.0, body_dy, 0.8));
        }
        // 玩耍:头顶双爱心(粉色,随相位交替闪)。
        if self.ctx == PetContext::Play {
            const HEART: u32 = 0xF08C98; // 像素爱心粉(宠物专属调色,非语义色)
            if !motion || self.phase {
                out.push((9, -1, HEART, 2.0, body_dy, 0.8));
            }
            if !motion || !self.phase {
                out.push((11, -2, HEART, 1.0, body_dy, 0.6));
            }
        }
        // 打盹:头顶 zZ(弱灰,随呼吸相位起伏)。
        if self.ctx == PetContext::Sleep {
            const ZZ: u32 = 0x69748E; // t2 弱文灰
            let lift = if motion && self.breath { -1.0 } else { 0.0 };
            out.push((11, 0, ZZ, 0.0, body_dy + lift, 0.5));
            out.push((12, -1, ZZ, 1.0, body_dy + lift * 1.5, 0.7));
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

        // ── 小狗本体(canvas 逐 quad 直绘;格距/偏移随形态 ×s) ──
        let sprite = canvas(
            |_b, _w, _cx| {},
            move |bounds, _state, window, _cx| {
                let cell = CELL * s;
                let ox = f32::from(bounds.origin.x) + SPRITE_X * s;
                let oy = f32::from(bounds.origin.y) + SPRITE_Y * s;
                for (x, y, color, dx, dy, hs) in &cells {
                    let h = cell * hs;
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(
                                px(ox + *x as f32 * cell + dx * s),
                                px(oy + *y as f32 * cell + dy * s + (cell - h) * 0.5),
                            ),
                            size: size(px(cell), px(h)),
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
