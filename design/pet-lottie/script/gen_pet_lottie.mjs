// Tn 宠物 · 像素小狗矢量化重做(换渲染方向:Lottie 路径)
// ------------------------------------------------------------------
// 与原型一致:14列×9行 前视像素金毛(grid 取自 crates/tn-ui/src/pet.rs GOLDEN)。
// 像素身份不变(方块格子、磷光配色),只平移/缩放不旋转(与原型动画词汇一致)。
// 一条时间轴演完全套姿态,每段打 marker 供集成层 seek。
//
// 同一套运动,按 config 出两份:
//   ① player  —— 512×512 + 背景/岗台 + 槽,浏览器预览/审稿
//   ② runtime —— 100×84(pet.rs box 原生尺度,CELL=6,透明无背景),内嵌进 tn-ui
//                由纯 Rust mini 播放器逐帧栅格化喂 RenderImage(终端实跑)
// 运行:node script/gen_pet_lottie.mjs
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

const FR = 60;
const COLS = 14, ROWS = 9;

// ---- 调色板(像素字符 → 颜色,移植自 pet.rs pixel_color;运动对全部品种通用)----
const hex = (h) => { const n = parseInt(h, 16); return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255, 1]; };
const COL = {
  heart: hex("F08C98"), zz: hex("69748E"), accent: hex("5BE7C4"),
  bg: hex("0E1422"), shelf: hex("2A3550"), bubble: hex("232C42"), biscuit: hex("C99052"),
  shadow: hex("04060B"), dust: hex("9BA4B4"), // 接地阴影(近黑冷,暗终端上才有对比)/ 像素扬尘(冷灰)
};
const COLOR_OF = {
  W: hex("F4F1E1"), P: hex("FFAAAB"), K: hex("323F49"), G: hex("F2C867"), D: hex("DAA14A"),
  B: hex("303338"), T: hex("C4905E"), R: hex("E36B6B"), A: hex("7EA0F0"), C: hex("C28F6C"),
  N: hex("965F3E"), U: hex("613922"),
};
const SID = { G: "furColor", D: "furDarkColor" }; // 仅金毛挂槽(player 可调);其余品种内联色

// ---- 7 个品种静态像素(站姿网格直接抄 pet.rs;睡觉=站姿改姿态,不另起造型)----------
// 每品种:rows(9×14)+ eyes + tail + tongue(仅金毛吐舌)+ face(被抬起特征格在 base 的衬底色);
// legL/legR 由底行(row8)双足自动切分。运动(osc/poseAt)对所有品种完全相同。
function deriveLegs(rows) {
  const r = rows[ROWS - 1]; const cols = [];
  for (let c = 0; c < r.length; c++) if (r[c] !== ".") cols.push(c);
  const runs = []; let cur = [];
  for (const c of cols) { if (cur.length && c !== cur[cur.length - 1] + 1) { runs.push(cur); cur = []; } cur.push(c); }
  if (cur.length) runs.push(cur);
  const L = runs[0] || [], R = runs[runs.length - 1] || [];
  return { legL: L.map((c) => [c, ROWS - 1]), legR: R.map((c) => [c, ROWS - 1]) };
}
const BREEDS = [
  { name: "westie",   face: "W", eyes: [[5, 4], [8, 4]], tail: [[11, 6]],          tongue: [], ears: [[4, 0], [9, 0]],
    rows: ["....W....W....", "...WPW..WPW...", "...WWWWWWWW...", "...WWWWWWWW...", "...WWKWWKWW...", "...WWWKKWWW...", "...WWWWWWWWW..", "...WWWWWWWW...", "....WW..WW...."] },
  { name: "golden",   face: "G", eyes: [[5, 4], [8, 4]], tail: [[12, 6], [13, 6]], tongue: [[7, 6]],
    rows: ["..............", "....GGGGGG....", "...GGGGGGGG...", "..DDGGGGGGDD..", "..DDGKGGKGDD..", "..DDGGKKGGDD..", "..DDGGGPGGDDGG", "..DDGGGGGGDD..", "....GG..GG...."] },
  { name: "shepherd", face: "T", eyes: [[5, 4], [8, 4]], tail: [[12, 6], [13, 6]], tongue: [], ears: [[3, 0], [4, 0], [9, 0], [10, 0]],
    rows: ["...BB....BB...", "...BTB..BTB...", "..BBBBBBBBBB..", "..BTTBTTBTTB..", "..BTTKTTKTTB..", "..BBTTKKTTBB..", "..BBBTTTTBBBBB", "..BBBBBBBBBB..", "....TT..TT...."] },
  { name: "bichon",   face: "W", eyes: [[5, 3], [8, 3]], tail: [[12, 5]],          tongue: [],
    rows: ["....WWWWWW....", "...WWWWWWWW...", "..WWWWWWWWWW..", "..WWWKWWKWWW..", "..WWPWKKWPWW..", "..WWWWPPWWWWW.", "...WWWWWWWW...", "...WWWWWWWW...", "....WW..WW...."] },
  { name: "maltese",  face: "W", eyes: [[5, 4], [8, 4]], tail: [[12, 6]],          tongue: [],
    rows: ["......RR......", ".....RWWR.....", "....WWWWWW....", "...WWWWWWWW...", "..WWWKWWKWWW..", "..WWPWKKWPWW..", "..WWWWWWWWWWW.", "..WWWWWWWWWW..", "...WW....WW..."] },
  { name: "shihtzu",  face: "W", eyes: [[5, 4], [8, 4]], tail: [[12, 6], [13, 6]], tongue: [],
    rows: ["......AA......", ".....AAAA.....", "....WWWWWW....", "..CCWWWWWWCC..", "..CCWKWWKWCC..", "..CCPWKKWPCC..", "..CCWWWWWWCCWW", "..CCWWWWWWCC..", "....WW..WW...."] },
  { name: "poodle",   face: "N", eyes: [[5, 3], [8, 3]], tail: [],                 tongue: [],
    rows: ["....NNNNNN....", "...NNNNNNNN...", "..NNNNNNNNNN..", "..UUNKNNKNUU..", "..UUPNKKNPUU..", "..UUNNNNNNUU..", "...UNNNNNNU...", "...UNNNNNNU...", "....NN..NN...."] },
].map((b) => ({ ...b, ...deriveLegs(b.rows) }));

// ---- Lottie 基元 -------------------------------------------------
const rect = (cxp, cyp, w, h) => ({ ty: "rc", p: { a: 0, k: [cxp, cyp] }, s: { a: 0, k: [w, h] }, r: { a: 0, k: 0 } });
const fill = (color, sid, opacity = 100) => ({ ty: "fl", c: sid ? { sid } : { a: 0, k: color }, o: { a: 0, k: opacity } });
const grTr = () => ({ ty: "tr", p: { a: 0, k: [0, 0] }, a: { a: 0, k: [0, 0] }, s: { a: 0, k: [100, 100] }, r: { a: 0, k: 0 }, o: { a: 0, k: 100 } });
const group = (nm, items) => ({ ty: "gr", nm, it: [...items, grTr()] });

// ---- 烘焙:连续函数 → 关键帧(线性 + 1D 简化)-------------------
function bakeTrack(fn, dims, total, eps) {
  const raw = []; for (let f = 0; f <= total; f++) raw.push({ t: f, v: fn(f) });
  // 段边界阶跃修正:poseAt/zzTrack 等按 inState 在段边界处「跳值」(如睡觉→投喂,眯眼 eyeSy
  // 0.16→1、Zz 不透明度→0)。整数取样会把这一跳摊成「边界前一整帧」的斜坡;而循环段运行时
  // 回卷点在 end 之前,会渲染到这段斜坡 →「半睁眼/Zz 忽暗」抽搐。故在每个边界前 1e-3 帧补一个
  // 「本段值」采样,把跳变收窄到 1e-3 帧(回卷点 < end,永不命中)。
  if (typeof SEGS !== "undefined") {
    for (let i = 1; i < SEGS.length; i++) { const b = SEGS[i].start; if (b > 0 && b < total) raw.push({ t: b - 1e-3, v: fn(b - 1e-3) }); }
    raw.sort((a, c) => a.t - c.t);
  }
  // 误差有界化简:锚点→候选末点连一条直线,只有「区间内每一点都在 eps 内」才丢弃中间点。
  // (旧版只比对相邻点,慢速微动(睡觉呼吸 ~0.05px/帧)会被整段抹平成直线 → 回卷处跳变抽搐。)
  const MAXRUN = 240; // 纯静止段封顶,避免 O(n²) 退化
  const keptIdx = [0]; let anchor = 0;
  for (let i = 2; i < raw.length; i++) {
    const A = raw[anchor], B = raw[i], span = B.t - A.t;
    let ok = span <= MAXRUN;
    for (let j = anchor + 1; j < i && ok; j++) {
      const C = raw[j];
      for (let d = 0; d < dims; d++) {
        const pred = span === 0 ? A.v[d] : A.v[d] + (B.v[d] - A.v[d]) * (C.t - A.t) / span;
        if (Math.abs(pred - C.v[d]) > eps[d]) { ok = false; break; }
      }
    }
    if (!ok) { anchor = i - 1; keptIdx.push(anchor); }
  }
  const last = raw.length - 1;
  if (keptIdx[keptIdx.length - 1] !== last) keptIdx.push(last);
  return keptIdx.map((idx) => ({ t: raw[idx].t, s: raw[idx].v.slice() }));
}
const anim = (fn, dims, total, eps) => {
  const k = bakeTrack(fn, dims, total, eps);
  if (k.every((kf) => kf.s.every((x, d) => Math.abs(x - k[0].s[d]) < 1e-9))) return { a: 0, k: dims === 1 ? k[0].s[0] : k[0].s };
  return { a: 1, k };
};
const stat = (v) => ({ a: 0, k: v });

// ---- 缓动 --------------------------------------------------------
const clamp01 = (x) => Math.max(0, Math.min(1, x));
const clamp = (x, lo, hi) => Math.max(lo, Math.min(hi, x));
const easeInOut = (t) => { t = clamp01(t); return t * t * (3 - 2 * t); };
const easeOut = (t) => { t = clamp01(t); return 1 - Math.pow(1 - t, 3); };
const easeIn = (t) => { t = clamp01(t); return t * t * t; };
const lerp = (a, b, t) => a + (b - a) * t;
const TAU = Math.PI * 2;

// ---- 时间轴分段 --------------------------------------------------
const SEG = [
  // 循环段统一循环体 120 帧(dur 136 − LOOP_IN 16),子周期整除 → 无缝;一次性段(peek/success/feed)按需定长;spin 自带 80 帧无缝体;sleep 保持已验收时长。
  ["peek", 36], ["idle", 136], ["typing", 136], ["running", 136],
  ["success", 90], ["error", 136], ["hover", 136], ["click", 136],
  ["play", 136], ["drag", 136], ["sleep", 156],
  ["feed", 132], ["scratch", 136], ["lickpaw", 136], ["spin", 96], ["stretch", 136], ["lookout", 136],
  ["earperk", 136],
];
let acc = 0;
const SEGS = SEG.map(([name, dur]) => { const s = { name, start: acc, dur }; acc += dur; return s; });
const TOTAL = acc;
const segAt = (f) => { for (let i = SEGS.length - 1; i >= 0; i--) if (f >= SEGS[i].start) return i; return 0; };
const inState = (f, ...names) => names.includes(SEGS[segAt(f)].name);

// ---- 姿态(平移/缩放,单位 = CELL28 px;运行时按 SC 缩放)--------
const REST = {
  rigDx: 0, rigDy: 0, rigSx: 1, rigSc: 1, headDx: 0, headDy: 0, earSy: 1,
  legLDx: 0, legLDy: 0, legRDx: 0, legRDy: 0, tailDx: 0, tailDy: 0, eyeSy: 1,
};
// earSy = 耳尖纵向缩放(>1 立耳变高,锚定在耳根 → 只长高、不平移,永远黏在头上不会飞出去)。
// 只有立耳品种(西高地/德牧)有耳层;垂耳品种无层 → 自动空操作。耳层位置只随 headDx/headDy(与眼/口同步)。
const POSE = {
  peek: { ...REST }, idle: { ...REST },
  typing: { ...REST, headDy: -4, eyeSy: 1.04, earSy: 1.32 }, // 专注 → 立耳
  running: { ...REST, headDy: -3, earSy: 1.12 },
  success: { ...REST, earSy: 1.18 },
  error: { ...REST, rigDy: 8, headDy: 5, eyeSy: 0.28, tailDy: 10 }, // 沮丧:耳不立(放松)
  hover: { ...REST, eyeSy: 0.26, headDy: -2, earSy: 1.3 },   // 注意到光标 → 立耳
  click: { ...REST, headDx: 12, headDy: 6 },
  play: { ...REST, earSy: 1.18 },
  drag: { ...REST, rigDy: -46, legLDy: 13, legRDy: 13, eyeSy: 1.08 },
  sleep: { ...REST, rigDy: 12, eyeSy: 0.16, legLDy: -26, legRDy: -26 }, // 收腿+眯眼+下沉(呼吸在 osc)
  // ── 投喂 + 活物引擎微动作(osc 驱动细节)──
  feed: { ...REST, earSy: 1.12 }, // 仰头等→接住跳→咀嚼→爱心(饼干为 prop)
  scratch: { ...REST, headDx: 5, eyeSy: 0.5 }, // 抓痒:歪头眯眼 + 后爪抖(osc)
  lickpaw: { ...REST, headDy: 9 }, // 舔爪:头低就爪(osc 抬前爪 + 舔)
  spin: { ...REST }, // 追尾转圈:绕地面小圈兜跑(osc 驱动,不挤压不翻转)
  stretch: { ...REST }, // 伸懒腰作揖:前低后翘(osc)
  lookout: { ...REST, headDx: 14, headDy: -3, eyeSy: 1.06, earSy: 1.4 }, // 望屏外:扭头(耳随头平移)+ 警觉立耳
  earperk: { ...REST, earSy: 1.58, eyeSy: 1.05 }, // 竖耳微动作:强立耳 + 耳尖抖(osc)
};
function osc(name, f, w) {
  const d = {}; const t = f / FR;
  switch (name) {
    case "peek": {
      // 史诗登场:从台下猛窜起(快冲)→ 过冲 → 落定压一下 + 扬尘 → 阻尼回弹安定(一次性)。
      const seg = SEGS[0]; const p = clamp01((f - seg.start) / seg.dur);
      const e = easeOut(Math.min(1, p / 0.58));
      let rise = (1 - e) * 188, sq = 0;                                            // 主体上冲(0.58 到位,更猛)
      if (p > 0.58) { const q = (p - 0.58) / 0.42; rise += -14 * Math.sin(Math.PI * q) * Math.exp(-2.0 * q); // 过冲回弹
        sq = 0.4 * Math.sin(Math.PI * Math.min(1, q / 0.3)) * Math.exp(-2.5 * q); }                          // 落定压一下
      d.rigDy = rise;
      d.rigSc = -0.12 * sq * w; d.rigSx = 0.18 * sq * w;
      d.earSy = (p > 0.5 ? 0.35 * Math.exp(-3 * (p - 0.5)) : 0) * w;               // 冒头时耳朵警觉一立
      d.headDy = (p > 0.5 ? -2.5 * Math.sin(Math.PI * (p - 0.5) / 0.5) : 0) * w;
      break;
    }
    case "idle": {
      // 安静呼吸(2次/循环=60帧)+ 重心轻摆(1次/循环)+ 尾巴慢摇。周期均整除 120 → 无缝。
      d.rigDy = -4 * Math.sin(TAU * f / 60) * w;          // 胸腔起伏(负=吸气抬升)
      d.rigDx = 2 * Math.sin(TAU * f / 120) * w;          // 重心左右轻移
      d.headDy = 1 * Math.sin(TAU * f / 60 + 0.6) * w;    // 头随呼吸轻点
      d.tailDy = -6 * Math.sin(TAU * f / 30) * w;         // 尾巴慢摇
      break;
    }
    case "typing": {
      // 前爪交替敲键 + 专注小点头 + 尾轻摆。打字节奏 6拍/循环(20帧)。
      const tap = TAU * f / 20;
      d.legLDx = -2 * w; d.legLDy = -6 * Math.max(0, Math.sin(tap)) * w;            // 左爪抬落敲键
      d.legRDx = 2 * w; d.legRDy = -6 * Math.max(0, Math.sin(tap + Math.PI)) * w;   // 右爪反相敲键
      d.headDy = 1.5 * Math.sin(tap) * w;                 // 随敲键专注小点头
      d.tailDy = -3 * Math.sin(TAU * f / 40) * w;
      d.earSy = 0.06 * Math.abs(Math.sin(TAU * f / 30)) * w; // 耳尖随敲键轻抖(立耳品种)
      break;
    }
    case "running": {
      // 清晰奔跑步态:大幅交替蹬腿 + 颠簸 + 触地压扁/腾空拉伸 + 尾巴飞甩。步频 6步/循环(20帧)→ 无缝。
      const ph = TAU * f / 20;
      const bounce = Math.abs(Math.sin(ph));               // 0 触地 1 腾空
      d.legLDx = 13 * Math.sin(ph) * w; d.legLDy = -13 * Math.max(0, Math.sin(ph)) * w;
      d.legRDx = 13 * Math.sin(ph + Math.PI) * w; d.legRDy = -13 * Math.max(0, Math.sin(ph + Math.PI)) * w;
      d.rigDy = -9 * bounce * w;                           // 腾空颠簸(pose 已含 -3 前压)
      const sq = 0.14 - 0.24 * bounce;                     // 触地压扁(+0.14)/ 腾空拉伸(-0.10)
      d.rigSc = -0.10 * sq * w; d.rigSx = 0.15 * sq * w;
      d.headDy = -2 * bounce * w;                          // 头随步频前压
      d.tailDy = -14 * Math.sin(TAU * f / 10) * w;         // 尾巴高频飞甩
      break;
    }
    case "success": {
      // 史诗庆祝(迪士尼分镜):蓄力下蹲 → 爆发拉伸起跳 → 滞空(减速悬停)→ 重落地压扁(分量)
      // → 渐弱回弹安定。耳朵起跳上扬、头领先、尾巴狂摇 + 起跳/落地的甩尾跟随。
      const seg = SEGS[segAt(f)]; const p = clamp01((f - seg.start) / seg.dur);
      let up = 0, sq = 0; // up 正=离地;sq 正=压扁(矮宽)
      if (p < 0.15) { const q = easeOut(p / 0.15); up = -7 * q; sq = 0.62 * q; }                                  // ① 蓄力下蹲(聚气压扁)
      else if (p < 0.24) { const q = easeIn((p - 0.15) / 0.09); up = lerp(-7, 28, q); sq = lerp(0.62, -0.5, q); }  // ② 爆发起跳(高瘦拉伸)
      else if (p < 0.38) { const q = easeOut((p - 0.24) / 0.14); up = lerp(28, 40, q); sq = lerp(-0.5, -0.05, q); } // ③ 升顶减速(滞空)
      else if (p < 0.50) { const q = easeIn((p - 0.38) / 0.12); up = lerp(40, 0, q); sq = lerp(-0.05, 0.1, q); }   // ④ 加速下坠
      else if (p < 0.57) { const q = (p - 0.50) / 0.07; up = lerp(0, -6, Math.sin(Math.PI * q)); sq = 0.74 * Math.sin(Math.PI * q); } // ⑤ 重落地压扁(分量!)
      else if (p < 0.73) { const q = (p - 0.57) / 0.16; up = 16 * Math.sin(Math.PI * q); sq = -0.22 * Math.sin(Math.PI * q); }        // ⑥ 回弹一(高瘦)
      else if (p < 0.85) { const q = (p - 0.73) / 0.12; up = -3 * Math.sin(Math.PI * q); sq = 0.24 * Math.sin(Math.PI * q); }         // ⑦ 落定小压
      d.rigDy = -up * w;
      d.rigSc = -0.12 * sq * w; d.rigSx = 0.18 * sq * w;
      d.earSy = (p > 0.15 && p < 0.55 ? 0.35 * Math.sin(Math.PI * clamp01((p - 0.15) / 0.4)) : 0) * w; // 起跳~滞空耳朵上扬
      d.headDy = (p > 0.15 && p < 0.5 ? -3 : p >= 0.5 && p < 0.57 ? 4 : 0) * w;                         // 头领先(起跳上抬/落地下压)
      d.tailDy = (-16 * Math.sin(TAU * 3.4 * t) - 10 * Math.sin(Math.PI * clamp01((p - 0.15) / 0.12))) * w; // 狂摇 + 起跳甩尾跟随
      break;
    }
    case "error": {
      // 沮丧:沉重叹气 + 缓慢摇头(no-no)+ 尾巴无力垂摆。周期整除 120 → 无缝。
      d.rigDy = 1.6 * Math.sin(TAU * f / 120) * w;         // 一次叹气/循环
      d.headDx = 2.5 * Math.sin(TAU * f / 60) * w;          // 缓慢摇头(2次/循环)
      d.headDy = 1.2 * Math.sin(TAU * f / 120 + 1) * w;     // 头随叹气起伏
      d.tailDy = 1.5 * Math.sin(TAU * f / 120) * w;         // 尾巴无力垂摆
      break;
    }
    case "hover": {
      // 注意到光标:眯眼微笑(pose)+ 开心轻颠 + 欢快摇尾。周期整除 120 → 无缝。
      d.rigDy = -3 * Math.abs(Math.sin(TAU * f / 40)) * w;   // 开心轻颠(3次/循环)
      d.headDy = -1.5 * Math.abs(Math.sin(TAU * f / 40)) * w;
      d.tailDy = -12 * Math.sin(TAU * f / 24) * w;           // 欢快摇尾(5次/循环)
      break;
    }
    case "click": {
      // 被点一下:好奇歪头(pose)+ 小幅歪头晃 + 摇尾(气泡为 prop)。
      d.headDx = 2 * Math.sin(TAU * f / 30) * w;             // 好奇歪头小晃
      d.rigDy = -2 * Math.abs(Math.sin(TAU * f / 30)) * w;
      d.tailDy = -10 * Math.sin(TAU * f / 20) * w;
      break;
    }
    case "play": {
      // 史诗玩耍:连续弹跳,每跳 触地压扁(矮宽)→ 腾空拉伸(高瘦)+ 耳朵上扬 + 狂摇尾 + 落地扬尘。
      const cyc = (f % 30) / 30;                              // 一跳周期(4跳/循环,30帧)
      const up = 24 * Math.sin(Math.PI * cyc);               // 抛物线弹跳
      const sq = 0.42 * Math.cos(TAU * cyc);                 // 触地 +0.42 压扁 / 顶 -0.42 拉伸
      d.rigDy = -up * w;
      d.rigSc = -0.12 * sq * w; d.rigSx = 0.18 * sq * w;
      d.earSy = 0.3 * Math.sin(Math.PI * cyc) * w;           // 腾空耳朵上扬
      d.headDy = -2 * Math.sin(Math.PI * cyc) * w;           // 头随腾空抬
      d.tailDy = -16 * Math.sin(TAU * f / 12) * w;           // 狂摇尾
      break;
    }
    case "drag": {
      // 被拎起悬空:钟摆式摇晃 + 四爪无重力乱蹬 + 尾巴飘。周期整除 120 → 无缝。
      d.rigDx = 10 * Math.sin(TAU * f / 60) * w;             // 钟摆摇晃(2次/循环)
      d.headDy = 2 * Math.sin(TAU * f / 60) * w;             // 头随摆动
      d.legLDx = 6 * Math.sin(TAU * f / 30) * w; d.legLDy = 4 * Math.sin(TAU * f / 30 + 1) * w;        // 前爪乱蹬
      d.legRDx = 6 * Math.sin(TAU * f / 30 + 0.7) * w; d.legRDy = 4 * Math.sin(TAU * f / 30 + 1.6) * w; // 后爪乱蹬
      d.tailDy = 6 * Math.sin(TAU * f / 40) * w;             // 尾巴无重力飘
      break;
    }
    case "sleep": d.rigDy = 5 * Math.sin(TAU * f / 140) * w; break; // 慢呼吸:周期 140=循环体长 → 无缝(192 不整除会抽搐)
    case "feed": {
      // 仰头期待 → 起跳接住 → 落地压扁 → 咀嚼 → 满足摇尾冒心(饼干/心为 prop)。
      const seg = SEGS[segAt(f)]; const p = clamp01((f - seg.start) / seg.dur);
      if (p < 0.18) { d.headDy = -8 * easeOut(p / 0.18) * w; d.tailDy = -6 * Math.sin(TAU * 2 * t) * w; }                          // 仰头期待 + 期待摇尾
      else if (p < 0.30) { const q = (p - 0.18) / 0.12; d.rigDy = -20 * Math.sin(Math.PI * q) * w; d.headDy = -8 * (1 - q) * w; }  // 起跳接住
      else if (p < 0.40) { const q = (p - 0.30) / 0.10; d.rigDy = 4 * Math.sin(Math.PI * q) * w; d.rigSc = -0.10 * Math.sin(Math.PI * q) * w; d.rigSx = 0.14 * Math.sin(Math.PI * q) * w; } // 落地压扁
      else if (p < 0.62) { d.headDy = 3 * Math.abs(Math.sin(TAU * 8 * t)) * w; d.rigDy = 1.5 * Math.abs(Math.sin(TAU * 8 * t)) * w; d.tailDy = -9 * Math.sin(TAU * 3 * t) * w; } // 咀嚼(头身同嚼)
      else { d.rigDy = -4 * Math.abs(Math.sin(TAU * 3 * t)) * w; d.tailDy = -15 * Math.sin(TAU * 3.5 * t) * w; }                    // 满足轻颠 + 狂摇尾
      break;
    }
    case "scratch": {
      // 抓痒:歪头眯眼(pose)+ 后腿伸出身下高频快抖 + 身子随挠轻颤。15次/循环(8帧)→ 无缝。
      const ph = TAU * f / 8;
      d.legRDy = (6 + 5 * Math.sin(ph)) * w;            // 后腿伸出快抖(向下=露出)
      d.legRDx = 4 * Math.sin(ph) * w;
      d.rigDx = 1.2 * Math.sin(ph) * w;                 // 身子随抓轻颤
      d.headDx = 2 * Math.sin(TAU * f / 24) * w;        // 头随痒处轻歪
      break;
    }
    case "lickpaw": {
      // 舔爪:低头凑爪(pose)+ 前爪抬到嘴边轻动 + 随舔轻点头。6次/循环(20帧)→ 无缝。
      const lk = TAU * f / 20;
      d.legLDy = (-3 + 2 * Math.sin(lk)) * w; d.legLDx = -2 * w;   // 前爪略抬内收并随舔轻动
      d.headDy = 1.5 * Math.sin(lk) * w;                          // 头随舔轻点(低头由 pose 提供)
      d.tailDy = -4 * Math.sin(TAU * f / 40) * w;
      break;
    }
    case "spin": {
      // 兜圈跑(追尾):绕地面小椭圆跑一整圈,远处抬高+缩小=纵深;
      // 不做水平挤压/镜像翻转(那会把像素挤成一条线,正是旧版「很丑」的根因)。
      // 周期 80 帧 = 运行时循环体长度 → 跨循环点无缝。
      const ph = TAU * f / 80;                       // 一圈/80帧
      const dep = (1 - Math.cos(ph)) / 2;            // 0=身前(近) 1=身后(远)
      const gait = TAU * f / 20;                     // 步频:每圈 4 步
      const bob = Math.abs(Math.sin(TAU * f / 10));  // 跑动颠簸
      d.rigDx = 44 * Math.sin(ph) * w;               // 水平绕圈(始终满宽)
      d.rigDy = (-24 * dep - 4 * bob) * w;           // 远处明显抬高 + 颠簸
      d.rigSc = -0.22 * dep * w;                     // 远小近大(叠加到 base rigSc=1)
      d.legLDx = 8 * Math.sin(gait) * w; d.legLDy = -8 * Math.max(0, Math.sin(gait)) * w;
      d.legRDx = 8 * Math.sin(gait + Math.PI) * w; d.legRDy = -8 * Math.max(0, Math.sin(gait + Math.PI)) * w;
      d.headDx = -4 * Math.sin(ph) * w;              // 头朝圈心轻倾(入弯)
      d.tailDx = 4 * Math.sin(ph) * w; d.tailDy = -10 * Math.sin(gait) * w; // 尾外甩 + 飘
      break;
    }
    case "stretch": {
      // 作揖伸懒腰:前身下压+前爪前伸,后臀翘起,然后还原。整段一次平滑起落(首尾归零)→ 无缝。
      const e = (1 - Math.cos(TAU * f / 120)) / 2;   // 0→1→0 平滑(一次/循环)
      d.headDy = 8 * e * w; d.legLDy = 9 * e * w; d.legLDx = -6 * e * w;               // 前低 + 前爪前伸
      d.legRDy = -5 * e * w; d.tailDy = -10 * e * w;                                   // 后臀翘 + 翘尾
      d.rigDx = -3 * e * w;                                                            // 重心前移
      break;
    }
    case "lookout": {
      // 望屏外:扭头(pose)+ 缓慢扫视 + 踮脚张望 + 好奇轻摇尾 + 耳尖随警觉轻抖。周期整除 120 → 无缝。
      d.headDx = 2 * Math.sin(TAU * f / 60) * w;            // 缓慢扫视(2次/循环)
      d.headDy = -1 * Math.abs(Math.sin(TAU * f / 60)) * w;
      d.rigDy = -1.5 * Math.abs(Math.sin(TAU * f / 120)) * w; // 踮脚张望
      d.tailDy = -5 * Math.sin(TAU * f / 40) * w;           // 好奇轻摇尾
      d.earSy = 0.08 * Math.abs(Math.sin(TAU * f / 20)) * w;  // 耳尖警觉轻抖(立耳品种)
      break;
    }
    case "earperk": {
      // 竖耳微动作:双耳立起(pose)+ 耳尖快速抖动 + 偶尔歪头听声(立耳品种才有耳层)。
      d.earSy = 0.12 * Math.abs(Math.sin(TAU * f / 15)) * w;  // 耳尖抖(8次/循环,变高)
      d.headDx = 2.5 * Math.sin(TAU * f / 60) * w;            // 歪头循声
      d.tailDy = -4 * Math.sin(TAU * f / 40) * w;
      break;
    }
  }
  return d;
}
function poseAt(f) {
  const i = segAt(f); const seg = SEGS[i];
  // 运行时按状态机跳段进入(非顺序播放),入场一律从 REST(idle)缓入,避免闪现
  // 时间轴上「前一段」的姿态。
  const cur = POSE[seg.name]; const prev = REST;
  const tDur = Math.min(14, seg.dur * 0.32);
  const blend = easeInOut((f - seg.start) / tDur);
  const base = {}; for (const k of Object.keys(REST)) base[k] = lerp(prev[k], cur[k], blend);
  const o = osc(seg.name, f, blend);
  const P = { ...base }; for (const k of Object.keys(o)) P[k] = (P[k] || 0) + o[k];
  if (base.eyeSy > 0.6) {
    const ph = f % 84;
    if (ph < 7) { const tri = 1 - Math.abs(ph - 3) / 3.5; P.eyeSy = base.eyeSy * (1 - 0.92 * clamp01(tri)); }
  }
  return P;
}
// 睡觉藏舌:入睡 12 帧内把粉舌淡出(其余时刻满显)。
function tongueOp(f) {
  if (!inState(f, "sleep")) return 100;
  const seg = SEGS[segAt(f)];
  return (1 - clamp01((f - seg.start) / 12)) * 100;
}
function heartTrack(f, period, phase) {
  if (!inState(f, "success", "hover", "play", "feed")) return { o: 0, dy: 0, s: 100 };
  // feed:只在「爱心收尾」段(>52%)冒心
  if (inState(f, "feed")) { const s = SEGS[segAt(f)]; if ((f - s.start) / s.dur < 0.52) return { o: 0, dy: 0, s: 100 }; }
  const cyc = (((f + phase) % period) / period);
  const o = cyc < 0.18 ? cyc / 0.18 : 1 - (cyc - 0.18) / 0.82;
  return { o: clamp01(o) * 100, dy: -50 * cyc, s: 70 + 40 * Math.min(1, cyc * 3) };
}
function zzTrack(f, period, phase) {
  if (!inState(f, "sleep")) return { o: 0, dy: 0, s: 100 };
  const cyc = (((f + phase) % period) / period);
  const o = cyc < 0.2 ? cyc / 0.2 : 1 - (cyc - 0.2) / 0.8;
  return { o: clamp01(o) * 90, dy: -44 * cyc, s: 60 + 55 * cyc };
}
// 接地阴影随抬升量(-rigDy)缩放+变淡:腾空越高越小越淡,下压(rigDy>0)略胀 → 重量/接地感。
function shadowK(f) {
  const lift = -poseAt(f).rigDy; // 正 = 离地
  return { sx: clamp(1 - lift / 58, 0.34, 1.18), op: clamp(1 - lift / 46, 0.22, 1.12) };
}
// 落地扬尘:返回最近一次落地后的进度 age∈[0,1)(否则 -1)。接 success/feed 的落地点。
function dustAge(f) {
  if (inState(f, "success")) { const s = SEGS[segAt(f)]; const p = (f - s.start) / s.dur; if (p >= 0.50 && p < 0.66) return (p - 0.50) / 0.16; }
  if (inState(f, "feed")) { const s = SEGS[segAt(f)]; const p = (f - s.start) / s.dur; if (p >= 0.33 && p < 0.47) return (p - 0.33) / 0.14; }
  if (inState(f, "play")) { const cyc = (f % 30) / 30; if (cyc < 0.34) return cyc / 0.34; }      // 每跳落地都扬尘(周期 30)
  if (inState(f, "peek")) { const s = SEGS[segAt(f)]; const p = (f - s.start) / s.dur; if (p >= 0.58 && p < 0.82) return (p - 0.58) / 0.24; } // 窜出落定扬尘
  return -1;
}
function dustPuff(f, side) { // side -1 左 / +1 右:落地瞬间在脚边迸出,外扩+上飘+变大+淡出
  const a = dustAge(f); if (a < 0) return { o: 0, dx: 0, dy: 0, s: 50 };
  const o = a < 0.22 ? a / 0.22 : 1 - (a - 0.22) / 0.78;
  return { o: clamp01(o) * 68, dx: side * (3 + 9 * a), dy: -2 - 7 * a, s: 50 + 95 * a };
}
const HEART = [[1, 0], [3, 0], [0, 1], [1, 1], [2, 1], [3, 1], [4, 1], [0, 2], [1, 2], [2, 2], [3, 2], [4, 2], [1, 3], [2, 3], [3, 3], [2, 4]];
// 接地阴影:9×3 扁像素椭圆(比站姿宽,两侧露出可见;中行最宽)。
const SHADOW = [
  [2, 0], [3, 0], [4, 0], [5, 0], [6, 0],
  [0, 1], [1, 1], [2, 1], [3, 1], [4, 1], [5, 1], [6, 1], [7, 1], [8, 1],
  [2, 2], [3, 2], [4, 2], [5, 2], [6, 2],
];
// 落地扬尘:小团像素。
const PUFF = [[1, 0], [0, 1], [1, 1], [2, 1], [1, 2]];
const Z4 = [[0, 0], [1, 0], [2, 0], [3, 0], [2, 1], [1, 2], [0, 3], [1, 3], [2, 3], [3, 3]];
const Z3 = [[0, 0], [1, 0], [2, 0], [1, 1], [0, 2], [1, 2], [2, 2]];

const markers = SEGS.map((s) => ({ tm: s.start, cm: s.name, dr: s.dur }));
const slots = {
  bgColor: { p: { a: 0, k: COL.bg } }, furColor: { p: { a: 0, k: COLOR_OF.G } },
  furDarkColor: { p: { a: 0, k: COLOR_OF.D } }, accentColor: { p: { a: 0, k: COL.accent } },
};

// =================================================================
//  build(CFG):同一套运动出一份 Lottie
//  CFG = { W,H,CELL,Y0pad?, bg(bool), name }
// =================================================================
function build(CFG, breed) {
  const { CELL, W, H, bg } = CFG;
  const X0 = (W - COLS * CELL) / 2;
  const Y0 = bg ? Math.round((H - ROWS * CELL) / 2 + CELL) : (H - 10 / 6 * CELL - ROWS * CELL); // runtime: 贴 pet.rs SPRITE_Y
  const cx = (col) => X0 + col * CELL + CELL / 2;
  const cy = (row) => Y0 + row * CELL + CELL / 2;
  const SHELF_Y = Y0 + ROWS * CELL - CELL / 2 + Math.round(CELL / 7);
  const SC = CELL / 28; // 运动缩放(姿态值以 CELL28 px 写就)

  const cellsToShapes = (cells) => {
    const byCh = new Map();
    for (const { col, row, ch } of cells) { if (!byCh.has(ch)) byCh.set(ch, []); byCh.get(ch).push([col, row]); }
    const out = [];
    const sz = CELL + Math.max(0.8, CELL * 0.04); // 放大消同色分层抗锯齿缝
    for (const [ch, list] of byCh) out.push(group("px_" + ch, [...list.map(([c, r]) => rect(cx(c), cy(r), sz, sz)), fill(COLOR_OF[ch], SID[ch])]));
    return out;
  };
  const patchShapes = (pat, ox, oy, s, color, sid) => [group("patch", [...pat.map(([c, r]) => rect(ox + c * s + s / 2, oy + r * s + s / 2, s, s)), fill(color, sid)])];

  const IND = { rig: 100 };
  const STORE = new Map();
  const add = (nm, l) => STORE.set(nm, l);
  const ksOf = ({ p, a, s, o }) => ({ o: o || stat(100), r: stat(0), p, a, s: s || stat([100, 100, 100]) });
  const Lyr = (nm, ind, parent, anchor, shapes, tr = {}) => ({
    ty: 4, nm, ind, parent: parent ?? undefined, ip: 0, op: TOTAL, st: 0,
    ks: ksOf({ p: tr.p || stat([anchor[0], anchor[1], 0]), a: stat([anchor[0], anchor[1], 0]), s: tr.s, o: tr.o }), shapes,
  });
  const Null = (nm, ind, parent, anchor, tr = {}) => ({
    ty: 3, nm, ind, parent: parent ?? undefined, ip: 0, op: TOTAL, st: 0, sw: 1, sh: 1, sc: "#000000",
    ks: ksOf({ p: tr.p || stat([anchor[0], anchor[1], 0]), a: stat([anchor[0], anchor[1], 0]), s: tr.s, o: tr.o }),
  });

  // 部件归类:base 轮廓 + 抬起的特征层(眼/口鼻/舌)+ 腿/尾覆盖层。
  // 被抬起的格(随头/眨眼动)在 base 补「面部衬底色 FB」,防止平移时露出背景缝。
  // 睡觉 = 站姿改姿态(收腿+眯眼+藏舌+呼吸),不切造型/不交叉淡入(淡入正是睡着闪烁根因)。
  const eyeSet = new Set(breed.eyes.map(([c, r]) => c + "," + r));
  const tailSet = new Set(breed.tail.map(([c, r]) => c + "," + r));
  const legLSet = new Set(breed.legL.map(([c, r]) => c + "," + r));
  const legRSet = new Set(breed.legR.map(([c, r]) => c + "," + r));
  const tongueSet = new Set(breed.tongue.map(([c, r]) => c + "," + r));
  const earSet = new Set((breed.ears || []).map(([c, r]) => c + "," + r));
  const FB = breed.face; // 面部衬底色字符
  const parts = { base: [], face: [], eyes: [], tongue: [], ears: [], legL: [], legR: [], tail: [] };
  breed.rows.forEach((line, row) => [...line].forEach((ch, col) => {
    if (ch === ".") return; const key = col + "," + row;
    if (legLSet.has(key)) return parts.legL.push({ col, row, ch });
    if (legRSet.has(key)) return parts.legR.push({ col, row, ch });
    if (tailSet.has(key)) return parts.tail.push({ col, row, ch });
    if (earSet.has(key)) { parts.ears.push({ col, row, ch }); parts.base.push({ col, row, ch }); return; } // 耳尖单列(立耳),base 同色兜底防露缝
    if (eyeSet.has(key)) { parts.eyes.push({ col, row, ch }); parts.base.push({ col, row, ch: FB }); return; }
    if (tongueSet.has(key)) { parts.tongue.push({ col, row, ch }); parts.base.push({ col, row, ch: FB }); return; }
    if (ch === "K") { parts.face.push({ col, row, ch }); parts.base.push({ col, row, ch: FB }); return; }
    parts.base.push({ col, row, ch });
  }));
  const avg = (cells, fn) => cells.reduce((s, c) => s + fn(c), 0) / cells.length;
  const eyeAnchor = [avg(parts.eyes, (c) => cx(c.col)), avg(parts.eyes, (c) => cy(c.row))];
  const faceAnchor = [avg(parts.face, (c) => cx(c.col)), avg(parts.face, (c) => cy(c.row))];
  const tongueAnchor = [avg(parts.tongue, (c) => cx(c.col)), avg(parts.tongue, (c) => cy(c.row))];
  const legLAnchor = [avg(parts.legL, (c) => cx(c.col)), cy(ROWS - 1)];
  const legRAnchor = [avg(parts.legR, (c) => cx(c.col)), cy(ROWS - 1)];
  const tailAnchor = parts.tail.length ? [avg(parts.tail, (c) => cx(c.col)), avg(parts.tail, (c) => cy(c.row))] : [0, 0];
  // 耳锚定在「耳根」(最低耳格的下缘)→ earSy 纵向缩放只让耳尖向上长高,耳根不动(不会飞)。
  const earAnchor = parts.ears.length
    ? [avg(parts.ears, (c) => cx(c.col)), cy(Math.max(...parts.ears.map((c) => c.row))) + CELL / 2]
    : [0, 0];
  const cxc = (W - 0) / 2; // 箱心 x

  // 背景/岗台(仅 player)
  if (bg) {
    add("background", Lyr("background", 1, null, [W / 2, H / 2], [group("bg", [rect(W / 2, H / 2, W, H), fill(COL.bg, "bgColor")])]));
    add("shelf", Lyr("shelf", 2, null, [cxc, SHELF_Y], [group("line", [rect(cxc, SHELF_Y, COLS * CELL, 2), fill(COL.shelf)])]));
    add("shelfph", Lyr("shelfph", 3, null, [cx(1.5), SHELF_Y], [group("ph", [rect(cx(1.5), SHELF_Y, 3 * CELL, 2), fill(COL.accent, "accentColor")])],
      { o: anim((f) => { const on = inState(f, "typing", "running"); const b = SEGS[segAt(f)].name === "running" ? 95 : 80; return [on ? b * (0.7 + 0.3 * Math.sin(TAU * 1.6 * f / FR)) : 42]; }, 1, TOTAL, [1.2]) }));
  }

  // 站姿层(始终不透明;睡觉只改姿态,不切层)
  add("base", Lyr("base", 14, IND.rig, [cxc, H / 2], cellsToShapes(parts.base)));
  if (parts.tail.length) add("tail", Lyr("tail", 13, IND.rig, tailAnchor, cellsToShapes(parts.tail),
    { p: anim((f) => { const P = poseAt(f); return [tailAnchor[0] + P.tailDx * SC, tailAnchor[1] + P.tailDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]) }));
  add("legL", Lyr("legL", 15, IND.rig, legLAnchor, cellsToShapes(parts.legL),
    { p: anim((f) => { const P = poseAt(f); return [legLAnchor[0] + P.legLDx * SC, legLAnchor[1] + P.legLDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]) }));
  add("legR", Lyr("legR", 16, IND.rig, legRAnchor, cellsToShapes(parts.legR),
    { p: anim((f) => { const P = poseAt(f); return [legRAnchor[0] + P.legRDx * SC, legRAnchor[1] + P.legRDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]) }));
  add("face", Lyr("face", 11, IND.rig, faceAnchor, cellsToShapes(parts.face),
    { p: anim((f) => { const P = poseAt(f); return [faceAnchor[0] + P.headDx * SC, faceAnchor[1] + P.headDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]) }));
  // 舌(粉 P):睡觉淡出(藏舌)—— 仅吐舌品种(金毛)有此层
  if (parts.tongue.length) add("tongue", Lyr("tongue", 12, IND.rig, tongueAnchor, cellsToShapes(parts.tongue),
    { p: anim((f) => { const P = poseAt(f); return [tongueAnchor[0] + P.headDx * SC, tongueAnchor[1] + P.headDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]),
      o: anim((f) => [tongueOp(f)], 1, TOTAL, [2]) }));
  add("eyes", Lyr("eyes", 10, IND.rig, eyeAnchor, cellsToShapes(parts.eyes),
    { p: anim((f) => { const P = poseAt(f); return [eyeAnchor[0] + P.headDx * SC, eyeAnchor[1] + P.headDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]),
      s: anim((f) => [100, poseAt(f).eyeSy * 100, 100], 3, TOTAL, [0.4, 1, 0.4]) }));
  // 耳尖(立耳品种):位置只随头动(headDx/headDy,与眼/口同步,永不脱离头部);
  // 立耳 = earSy 纵向缩放(锚在耳根 → 只向上长高)。base 留同色兜底,缩放不露缝。
  if (parts.ears.length) add("ears", Lyr("ears", 9, IND.rig, earAnchor, cellsToShapes(parts.ears),
    { p: anim((f) => { const P = poseAt(f); return [earAnchor[0] + P.headDx * SC, earAnchor[1] + P.headDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]),
      s: anim((f) => [100, poseAt(f).earSy * 100, 100], 3, TOTAL, [0.4, 1, 0.4]) }));

  // 根 null(全局升降/拖拽悬空)
  add("rig", Null("rig", IND.rig, null, [cxc, SHELF_Y], {
    p: anim((f) => { const P = poseAt(f); return [cxc + P.rigDx * SC, SHELF_Y + P.rigDy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]),
    s: anim((f) => { const P = poseAt(f); return [P.rigSx * P.rigSc * 100, P.rigSc * 100, 100]; }, 3, TOTAL, [0.5, 0.5, 0.5]), // 追尾转圈:纵深缩放(rigSc),无水平挤压
  }));

  // 接地阴影:不挂 rig(留在地面),随抬升量缩放变淡、随 rigDx 横移 —— 重量/接地感的地基。
  // 比站姿宽(两侧露出可见)、压扁、压低到爪线之下,深色高不透明 → 暗终端上也看得见。
  {
    const ssz = CELL * 0.92, sw = 9, sh = 3;
    const fx = cxc, fy = bg ? SHELF_Y - Math.round(CELL * 0.05) : cy(ROWS - 1) + CELL * 0.72;
    add("shadow", Lyr("shadow", 8, null, [fx, fy],
      patchShapes(SHADOW, fx - sw * ssz / 2, fy - sh * ssz / 2, ssz, COL.shadow), {
        p: anim((f) => [fx + poseAt(f).rigDx * SC, fy, 0], 3, TOTAL, [0.3, 0.3, 0.3]),
        s: anim((f) => { const k = shadowK(f); return [k.sx * 100, k.sx * 60, 100]; }, 3, TOTAL, [0.5, 0.5, 0.5]), // Y 压扁成薄水洼
        o: anim((f) => [70 * shadowK(f).op], 1, TOTAL, [1]),
      }));
  }
  // 落地扬尘:脚边左右各一团,落地瞬间迸出(不挂 rig,留在落点)。
  {
    const dsz = CELL * 0.5, pw = 3 * dsz;
    const fy = bg ? SHELF_Y - Math.round(CELL * 0.2) : cy(ROWS - 1) + CELL * 0.4;
    for (const side of [-1, 1]) {
      const nm = side < 0 ? "dustL" : "dustR"; const bx = cxc + side * CELL * 1.7;
      add(nm, Lyr(nm, side < 0 ? 6 : 7, null, [bx, fy],
        patchShapes(PUFF, bx - pw / 2, fy - pw / 2, dsz, COL.dust), {
          o: anim((f) => [dustPuff(f, side).o], 1, TOTAL, [2]),
          p: anim((f) => { const d = dustPuff(f, side); return [bx + d.dx * SC, fy + d.dy * SC, 0]; }, 3, TOTAL, [0.3, 0.3, 0.3]),
          s: anim((f) => { const v = dustPuff(f, side).s; return [v, v, 100]; }, 3, TOTAL, [0.6, 0.6, 0.6]),
        }));
    }
  }

  // 道具:像素爱心 / Z / bark 气泡(位置以格坐标,随尺度走)
  const heartLayer = (nm, ind, gcol, grow, period, phase, onlyPlay) => {
    const s = 1.8 * CELL / 5; const bx = cx(gcol), by = cy(grow); const w = 5 * s;
    return Lyr(nm, ind, IND.rig, [bx, by], patchShapes(HEART, bx - w / 2, by - w / 2, s, COL.heart), {
      o: anim((f) => { const tr = heartTrack(f, period, phase); return [onlyPlay && !inState(f, "play") ? 0 : tr.o]; }, 1, TOTAL, [2]),
      p: anim((f) => [bx, by + heartTrack(f, period, phase).dy * SC, 0], 3, TOTAL, [0.3, 0.3, 0.3]),
      s: anim((f) => { const v = heartTrack(f, period, phase).s; return [v, v, 100]; }, 3, TOTAL, [0.6, 0.6, 0.6]),
    });
  };
  add("heartA", heartLayer("heartA", 30, 6, -1.1, 40, 0, false));
  add("heartB", heartLayer("heartB", 31, 8, -1.1, 40, 20, true));
  const zzLayer = (nm, ind, gcol, grow, pat, cells, period, phase) => {
    const s = 1.3 * CELL / 4; const bx = cx(gcol), by = cy(grow); const w = cells * s;
    return Lyr(nm, ind, IND.rig, [bx, by], patchShapes(pat, bx - w / 2, by - w / 2, s, COL.zz), {
      o: anim((f) => [zzTrack(f, period, phase).o], 1, TOTAL, [2]),
      p: anim((f) => [bx, by + zzTrack(f, period, phase).dy * SC, 0], 3, TOTAL, [0.3, 0.3, 0.3]),
      s: anim((f) => { const v = zzTrack(f, period, phase).s; return [v, v, 100]; }, 3, TOTAL, [0.6, 0.6, 0.6]),
    });
  };
  // Zz 周期 70 整除睡觉循环体 140(96 不整除会在循环回卷处让 Zz 跳变=抽搐)。
  add("zzBig", zzLayer("zzBig", 32, 7.5, -1.2, Z4, 4, 70, 0));
  add("zzSmall", zzLayer("zzSmall", 33, 9.2, -2, Z3, 3, 70, 35));
  {
    const bw = 1.6 * CELL, bh = 1.3 * CELL; const bx = cx(10.5), by = cy(-1.4);
    const bubO = (f) => { if (!inState(f, "click")) return 0; const s = SEGS[segAt(f)]; const p = clamp01((f - s.start) / s.dur); return p < 0.12 ? (p / 0.12) * 100 : p < 0.7 ? 100 : (1 - (p - 0.7) / 0.3) * 100; };
    add("bubble", Lyr("bubble", 34, IND.rig, [bx, by], [
      group("b", [rect(bx, by, bw, bh), fill(COL.bubble)]),
      group("bar", [rect(bx, by - bh * 0.12, bw * 0.14, bh * 0.42), fill(COL.accent, "accentColor")]),
      group("dot", [rect(bx, by + bh * 0.3, bw * 0.14, bh * 0.18), fill(COL.accent, "accentColor")]),
    ], { o: anim((f) => [bubO(f)], 1, TOTAL, [2]) }));
  }
  // 投喂饼干:从右上抛物线落到嘴边,接住即「吃掉」(消失)
  {
    const bs = 1.1 * CELL; const sx = cx(11), sy = cy(-2), ex = cx(7), ey = cy(5);
    const biscuit = (f) => {
      if (!inState(f, "feed")) return { o: 0, x: sx, y: sy };
      const s = SEGS[segAt(f)]; const q = (f - s.start) / s.dur;
      if (q > 0.30) return { o: 0, x: ex, y: ey };
      const u = clamp01(q / 0.30);
      return { o: 100, x: lerp(sx, ex, u), y: lerp(sy, ey, u) - CELL * 2.2 * u * (1 - u) };
    };
    add("biscuit", Lyr("biscuit", 35, IND.rig, [sx, sy], [group("bk", [rect(sx, sy, bs, bs * 0.8), fill(COL.biscuit)])], {
      o: anim((f) => [biscuit(f).o], 1, TOTAL, [2]),
      p: anim((f) => { const b = biscuit(f); return [b.x, b.y, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]),
    }));
  }

  let ORDER = ["dustL", "dustR", "bubble", "biscuit", "zzSmall", "zzBig", "heartB", "heartA", "eyes", "face", "tongue", "ears", "base", "tail", "legL", "legR", "shadow"];
  if (bg) ORDER = [...ORDER, "shelfph", "shelf", "background"];
  ORDER = [...ORDER, "rig"];
  const layers = ORDER.map((n) => STORE.get(n)).filter(Boolean); // tongue/tail 等空层按品种缺省

  return { v: "5.7.0", fr: FR, ip: 0, op: TOTAL, w: W, h: H, nm: "Tn pixel pet — " + breed.name, ddd: 0, assets: [], markers, slots, layers };
}

// ---- 出两份 ------------------------------------------------------
const PLAYER_DIR = resolve(__dirname, "../public/projects/main-project/scene-1");
const RUNTIME_DIR = resolve(__dirname, "../../../crates/tn-ui/assets/pet");
mkdirSync(PLAYER_DIR, { recursive: true });
mkdirSync(RUNTIME_DIR, { recursive: true });

const sz = (o) => (JSON.stringify(o).length / 1024).toFixed(0);
const goldenBreed = BREEDS.find((b) => b.name === "golden");

// ① player —— 金毛 showcase(512×512 + 背景/岗台 + 槽),浏览器预览/审稿
const player = build({ CELL: 28, W: 512, H: 512, bg: true }, goldenBreed);
writeFileSync(resolve(PLAYER_DIR, "lottie.json"), JSON.stringify(player));
writeFileSync(resolve(PLAYER_DIR, "controls.json"), JSON.stringify({
  controls: [
    { sid: "bgColor", label: "背景海拔" }, { sid: "furColor", label: "毛色(主 G)" },
    { sid: "furDarkColor", label: "毛色(垂耳 D)" }, { sid: "accentColor", label: "磷光强调色" },
  ],
}, null, 2));
console.log(`player  512px  layers=${player.layers.length}  size=${sz(player)}KB  (golden showcase)`);

// ② runtime —— 7 个品种各一份(100×84,pet.rs box 原生尺度;终端按 breed 选用)
for (const breed of BREEDS) {
  const runtime = build({ CELL: 6, W: 100, H: 84, bg: false }, breed); // = pet.rs box(BOX_W/BOX_H/CELL/SPRITE_Y)
  writeFileSync(resolve(RUNTIME_DIR, breed.name + ".json"), JSON.stringify(runtime));
  console.log(`runtime ${breed.name.padEnd(9)} ${runtime.w}x${runtime.h}  layers=${runtime.layers.length}  size=${sz(runtime)}KB`);
}
console.log("markers:", markers.map((m) => `${m.cm}@${m.tm}`).join("  "));
