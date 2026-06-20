// Tn 宠物 · 像素小狗矢量化重做(换渲染方向:Lottie/Skottie 路径)
// ------------------------------------------------------------------
// 与原型一致:14列×9行 前视像素金毛(grid 取自 crates/tn-ui/src/pet.rs GOLDEN)。
// 像素身份不变(方块格子、磷光配色),但用 Lottie 把原本逐帧重绘的 dx/dy/缩放
// 做成连续插值 —— 子像素平滑运动,而格子始终轴对齐(只平移/缩放,不旋转,
// 与原型动画词汇一致)。一条时间轴演完全套姿态,每段打 marker 供集成层 seek。
// 运行:node script/gen_pet_lottie.mjs
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const OUT_DIR = resolve(__dirname, "../public/projects/main-project/scene-1");

const FR = 60, W = 512, H = 512;

// ---- 像素网格(与 pet.rs 一致)----------------------------------
const COLS = 14, ROWS = 9, CELL = 28;
const X0 = (W - COLS * CELL) / 2;      // 60
const Y0 = 168;                         // row8 落在岗台附近
const cx = (col) => X0 + col * CELL + CELL / 2;
const cy = (row) => Y0 + row * CELL + CELL / 2;
const SHELF_Y = Y0 + ROWS * CELL - CELL / 2 + 4; // 岗台线

// GOLDEN 站姿 / 趴姿(直接抄 pet.rs)
const STAND = [
  "..............", "....GGGGGG....", "...GGGGGGGG...", "..DDGGGGGGDD..",
  "..DDGKGGKGDD..", "..DDGGKKGGDD..", "..DDGGGPGGDDGG", "..DDGGGGGGDD..", "....GG..GG....",
];
const LIE = [
  "..............", "..............", "....GGGGGG....", "...GGGGGGGG...",
  "..DDGKGGKGDD..", "..DDGGKKGGDD..", "..DDGGGPGGDD..", ".GGGGGGGGGGGG.", ".GGGGGGGGGGGGG",
];
const EYES = [[5, 4], [8, 4]];          // 眼格(blink/squint 缩放)
const TAILC = [[12, 6], [13, 6]];       // 尾格(摇尾)
const LEGL = [[4, 8], [5, 8]];          // 左腿/爪
const LEGR = [[8, 8], [9, 8]];          // 右腿/爪

// ---- 调色板(像素表沿用)----------------------------------------
const hex = (h) => { const n = parseInt(h, 16); return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255, 1]; };
const COL = {
  G: hex("F2C867"), D: hex("DAA14A"), K: hex("323F49"), P: hex("FFAAAB"),
  heart: hex("F08C98"), zz: hex("69748E"), accent: hex("5BE7C4"),
  bg: hex("0E1422"), shelf: hex("2A3550"), bubble: hex("232C42"),
};
const SID = { G: "furColor", D: "furDarkColor" }; // 可换色槽
const COLOR_OF = { G: COL.G, D: COL.D, K: COL.K, P: COL.P };

// ---- Lottie 基元 -------------------------------------------------
const rect = (cxp, cyp, w, h) => ({ ty: "rc", p: { a: 0, k: [cxp, cyp] }, s: { a: 0, k: [w, h] }, r: { a: 0, k: 0 } });
const fill = (color, sid, opacity = 100) => ({ ty: "fl", c: sid ? { sid } : { a: 0, k: color }, o: { a: 0, k: opacity } });
const stroke = (color, width, sid) => ({ ty: "st", c: sid ? { sid } : { a: 0, k: color }, o: { a: 0, k: 100 }, w: { a: 0, k: width }, lc: 2, lj: 2 });
const grTr = () => ({ ty: "tr", p: { a: 0, k: [0, 0] }, a: { a: 0, k: [0, 0] }, s: { a: 0, k: [100, 100] }, r: { a: 0, k: 0 }, o: { a: 0, k: 100 } });
const group = (nm, items) => ({ ty: "gr", nm, it: [...items, grTr()] });

// 把若干格子(按颜色批量)做成 shapes 数组。格子放大 1px → 相邻方块重叠,
// 消除同色分层在交界处的抗锯齿发丝缝(静止时无缝;部件位移时按原型自然错格)。
function cellsToShapes(cells, size = CELL + 1) {
  const byCh = new Map();
  for (const { col, row, ch } of cells) { if (!byCh.has(ch)) byCh.set(ch, []); byCh.get(ch).push([col, row]); }
  const out = [];
  for (const [ch, list] of byCh) {
    const rects = list.map(([c, r]) => rect(cx(c), cy(r), size, size));
    out.push(group("px_" + ch, [...rects, fill(COLOR_OF[ch], SID[ch])]));
  }
  return out;
}
// 小像素图案(任意 cell 尺寸,自带颜色)
function patchShapes(pattern, ox, oy, s, color, sid) {
  const rects = [];
  pattern.forEach(([c, r]) => rects.push(rect(ox + c * s + s / 2, oy + r * s + s / 2, s, s)));
  return [group("patch", [...rects, fill(color, sid)])];
}

// ---- 烘焙:连续函数 → 关键帧(线性 + 1D 简化)-------------------
function bakeTrack(fn, dims, total, eps) {
  const raw = []; for (let f = 0; f <= total; f++) raw.push({ t: f, v: fn(f) });
  const kept = [raw[0]];
  for (let i = 1; i < raw.length - 1; i++) {
    const prev = kept[kept.length - 1], next = raw[i + 1], curr = raw[i];
    const span = next.t - prev.t; let drop = true;
    for (let d = 0; d < dims; d++) {
      const pred = span === 0 ? prev.v[d] : prev.v[d] + (next.v[d] - prev.v[d]) * (curr.t - prev.t) / span;
      if (Math.abs(pred - curr.v[d]) > eps[d]) { drop = false; break; }
    }
    if (!drop) kept.push(curr);
  }
  kept.push(raw[raw.length - 1]);
  return kept.map((k) => ({ t: k.t, s: k.v.slice() }));
}
const anim = (fn, dims, total, eps) => {
  const k = bakeTrack(fn, dims, total, eps);
  if (k.every((kf) => kf.s.every((x, d) => Math.abs(x - k[0].s[d]) < 1e-9))) return { a: 0, k: dims === 1 ? k[0].s[0] : k[0].s };
  return { a: 1, k };
};
const stat = (v) => ({ a: 0, k: v });

// ---- 缓动 --------------------------------------------------------
const clamp01 = (x) => Math.max(0, Math.min(1, x));
const easeInOut = (t) => { t = clamp01(t); return t * t * (3 - 2 * t); };
const easeOut = (t) => { t = clamp01(t); return 1 - Math.pow(1 - t, 3); };
const lerp = (a, b, t) => a + (b - a) * t;
const TAU = Math.PI * 2;

// ---- 时间轴分段 --------------------------------------------------
const SEG = [
  ["peek", 36], ["idle", 132], ["typing", 120], ["running", 132],
  ["success", 78], ["error", 132], ["hover", 96], ["click", 84],
  ["play", 132], ["drag", 108], ["sleep", 156],
];
let acc = 0;
const SEGS = SEG.map(([name, dur]) => { const s = { name, start: acc, dur }; acc += dur; return s; });
const TOTAL = acc;
const segAt = (f) => { for (let i = SEGS.length - 1; i >= 0; i--) if (f >= SEGS[i].start) return i; return 0; };
const inState = (f, ...names) => names.includes(SEGS[segAt(f)].name);

// ---- 各状态静态目标姿态(平移/缩放,无旋转)----------------------
const REST = {
  rigDx: 0, rigDy: 0, headDx: 0, headDy: 0,
  legLDx: 0, legLDy: 0, legRDx: 0, legRDy: 0, tailDx: 0, tailDy: 0, eyeSy: 1,
};
const POSE = {
  peek: { ...REST },
  idle: { ...REST },
  typing: { ...REST, headDy: -4, eyeSy: 1.04 },
  running: { ...REST, headDy: -3 },
  success: { ...REST },
  error: { ...REST, rigDy: 8, headDy: 5, eyeSy: 0.28, tailDy: 10 },
  hover: { ...REST, eyeSy: 0.26, headDy: -2 },
  click: { ...REST, headDx: 12, headDy: 6 },
  play: { ...REST },
  drag: { ...REST, rigDy: -46, legLDy: 13, legRDy: 13, eyeSy: 1.08 },
  sleep: { ...REST },
};

// ---- 振荡 --------------------------------------------------------
function osc(name, f, w) {
  const d = {}; const t = f / FR;
  switch (name) {
    case "peek": {
      const seg = SEGS[0]; const p = clamp01((f - seg.start) / seg.dur);
      const e = easeOut(Math.min(1, p / 0.82)); let rise = (1 - e) * 170;
      if (p > 0.82) rise += -5 * Math.sin((p - 0.82) / 0.18 * Math.PI);
      d.rigDy = rise; break;
    }
    case "idle":
      d.rigDy = 6 * Math.sin(TAU * t / 2.2) * w;
      d.tailDy = -10 * Math.sin(TAU * 0.55 * t) * w; break;
    case "typing":
      d.headDy = 2 * Math.sin(TAU * 2.6 * t) * w;
      d.tailDy = -4 * Math.sin(TAU * 1.1 * t) * w; break;
    case "running": {
      const ph = TAU * 2.4 * t;
      d.legLDx = 11 * Math.sin(ph) * w; d.legLDy = -10 * Math.max(0, Math.sin(ph)) * w;
      d.legRDx = 11 * Math.sin(ph + Math.PI) * w; d.legRDy = -10 * Math.max(0, Math.sin(ph + Math.PI)) * w;
      d.rigDy = -8 * Math.abs(Math.sin(ph)) * w; d.tailDy = -14 * Math.sin(TAU * 3 * t) * w; break;
    }
    case "success": {
      const seg = SEGS[segAt(f)]; const p = clamp01((f - seg.start) / seg.dur); let dy = 0;
      if (p < 0.16) dy = 6 * (p / 0.16);
      else if (p < 0.62) { const q = (p - 0.16) / 0.46; dy = -30 * Math.sin(Math.PI * q); }
      else if (p < 0.78) { const q = (p - 0.62) / 0.16; dy = 5 * Math.sin(Math.PI * q); }
      d.rigDy = dy * w; d.tailDy = -14 * Math.sin(TAU * 3.4 * t) * w; break;
    }
    case "error": d.rigDy = 1.6 * Math.sin(TAU * t / 2.6) * w; break;
    case "hover": d.tailDy = -11 * Math.sin(TAU * 1.4 * t) * w; break;
    case "click": {
      const seg = SEGS[segAt(f)]; const p = clamp01((f - seg.start) / seg.dur);
      d.headDx = 3 * Math.sin(TAU * 2 * t) * w; d.tailDy = -12 * Math.sin(TAU * 2.2 * t) * w;
      if (p < 0.2) d.headDy = -4 * (p / 0.2) * w; break;
    }
    case "play": {
      const ph = TAU * 2.1 * t;
      d.rigDy = -20 * Math.abs(Math.sin(ph)) * w; d.tailDy = -16 * Math.sin(TAU * 4 * t) * w; break;
    }
    case "drag":
      d.rigDx = 9 * Math.sin(TAU * 0.6 * t) * w;
      d.legLDx = 6 * Math.sin(TAU * 0.9 * t) * w; d.legRDx = 6 * Math.sin(TAU * 0.9 * t + 0.5) * w;
      d.legLDy = 4 * Math.sin(TAU * 0.9 * t + 1) * w; d.legRDy = 4 * Math.sin(TAU * 0.9 * t + 1.4) * w; break;
    case "sleep": d.rigDy = 5 * Math.sin(TAU * t / 3.2) * w; break;
  }
  return d;
}

function poseAt(f) {
  const i = segAt(f); const seg = SEGS[i];
  const cur = POSE[seg.name]; const prev = i > 0 ? POSE[SEGS[i - 1].name] : REST;
  const tDur = Math.min(14, seg.dur * 0.32);
  const blend = easeInOut((f - seg.start) / tDur);
  const base = {}; for (const k of Object.keys(REST)) base[k] = lerp(prev[k], cur[k], blend);
  const o = osc(seg.name, f, blend);
  const P = { ...base }; for (const k of Object.keys(o)) P[k] = (P[k] || 0) + o[k];
  if (base.eyeSy > 0.6) { // 眨眼:睁眼态周期性瞬闭
    const ph = f % 84;
    if (ph < 7) { const tri = 1 - Math.abs(ph - 3) / 3.5; P.eyeSy = base.eyeSy * (1 - 0.92 * clamp01(tri)); }
  }
  return P;
}

// 站姿/趴姿交叉淡入(sleep 段切换)
function sleepBlend(f) {
  if (!inState(f, "sleep")) return 0;
  const seg = SEGS[segAt(f)]; const p = f - seg.start; const fade = 8;
  if (p < fade) return p / fade;
  if (p > seg.dur - fade) return Math.max(0, (seg.dur - p) / fade);
  return 1;
}
const standOp = (f) => [(1 - sleepBlend(f)) * 100];
const lieOp = (f) => [sleepBlend(f) * 100];

// ---- 道具轨迹 ----------------------------------------------------
function heartTrack(f, period, phase) {
  if (!inState(f, "success", "hover", "play")) return { o: 0, dy: 0, s: 100 };
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

// =================================================================
//  组装(显式 z 序)
// =================================================================
const IND = { rig: 100 };
const STORE = new Map();
const add = (nm, l) => STORE.set(nm, l);
const ksOf = ({ p, a, s, o }) => ({ o: o || stat(100), r: stat(0), p, a, s: s || stat([100, 100, 100]) });
function layer(nm, ind, parent, anchor, shapes, tr = {}) {
  return { ty: 4, nm, ind, parent: parent ?? undefined, ip: 0, op: TOTAL, st: 0,
    ks: ksOf({ p: tr.p || stat([anchor[0], anchor[1], 0]), a: stat([anchor[0], anchor[1], 0]), s: tr.s, o: tr.o }), shapes };
}
function nullL(nm, ind, parent, anchor, tr = {}) {
  return { ty: 3, nm, ind, parent: parent ?? undefined, ip: 0, op: TOTAL, st: 0, sw: 1, sh: 1, sc: "#000000",
    ks: ksOf({ p: tr.p || stat([anchor[0], anchor[1], 0]), a: stat([anchor[0], anchor[1], 0]), s: tr.s, o: tr.o }) };
}

// 把 STAND 网格按部件归类
const eyeSet = new Set(EYES.map(([c, r]) => c + "," + r));
const tailSet = new Set(TAILC.map(([c, r]) => c + "," + r));
const legLSet = new Set(LEGL.map(([c, r]) => c + "," + r));
const legRSet = new Set(LEGR.map(([c, r]) => c + "," + r));
// 分解策略:整只毛色轮廓 = 单层 base(无内部接缝);只有"深色特征"(眼/口/鼻)
// 与腿/尾作为覆盖层在其上平移。头部"动作"= 深色特征在实心金底上滑动 → 看作
// 瞥眼/歪头,永不露底缝(金底始终在后兜底)。
const parts = { base: [], face: [], eyes: [], legL: [], legR: [], tail: [] };
STAND.forEach((line, row) => {
  [...line].forEach((ch, col) => {
    if (ch === ".") return; const key = col + "," + row;
    if (legLSet.has(key)) { parts.legL.push({ col, row, ch }); return; }
    if (legRSet.has(key)) { parts.legR.push({ col, row, ch }); return; }
    if (tailSet.has(key)) { parts.tail.push({ col, row, ch }); return; }
    if (eyeSet.has(key)) { parts.eyes.push({ col, row, ch }); parts.base.push({ col, row, ch: "G" }); return; } // 眼后补金底
    if (ch === "K" || ch === "P") { parts.face.push({ col, row, ch }); parts.base.push({ col, row, ch: "G" }); return; } // 口/鼻 = 随头偏移
    parts.base.push({ col, row, ch });
  });
});

// 各部件锚点(由格子重心算,稳健)
const avg = (cells, fn) => cells.reduce((s, c) => s + fn(c), 0) / cells.length;
const eyeAnchor = [avg(parts.eyes, (c) => cx(c.col)), avg(parts.eyes, (c) => cy(c.row))];
const faceAnchor = [avg(parts.face, (c) => cx(c.col)), avg(parts.face, (c) => cy(c.row))];
const legLAnchor = [avg(parts.legL, (c) => cx(c.col)), cy(8)];
const legRAnchor = [avg(parts.legR, (c) => cx(c.col)), cy(8)];
const tailAnchor = [avg(parts.tail, (c) => cx(c.col)), cy(6)];

// 背景 + 岗台(world)
add("background", layer("background", 1, null, [256, 256], [group("bg", [rect(256, 256, W, H), fill(COL.bg, "bgColor")])]));
add("shelf", layer("shelf", 2, null, [256, SHELF_Y], [
  group("line", [rect(256, SHELF_Y, 392, 2), fill(COL.shelf)]),
], { o: stat(100) }));
// 岗台磷光段(typing/running 变亮 = 活信号)
add("shelfph", layer("shelfph", 3, null, [148, SHELF_Y], [group("ph", [rect(148, SHELF_Y, 84, 2), fill(COL.accent, "accentColor")])],
  { o: anim((f) => { const on = inState(f, "typing", "running"); const b = SEGS[segAt(f)].name === "running" ? 95 : 80; return [on ? b * (0.7 + 0.3 * Math.sin(TAU * 1.6 * f / FR)) : 42]; }, 1, TOTAL, [1.2]) }));

// 站姿层(全部 parent = rig)
add("base", layer("base", 14, IND.rig, [256, 256], cellsToShapes(parts.base), { o: anim(standOp, 1, TOTAL, [2]) }));
add("tail", layer("tail", 13, IND.rig, tailAnchor, cellsToShapes(parts.tail),
  { p: anim((f) => { const P = poseAt(f); return [tailAnchor[0] + P.tailDx, tailAnchor[1] + P.tailDy, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]), o: anim(standOp, 1, TOTAL, [2]) }));
add("legL", layer("legL", 15, IND.rig, legLAnchor, cellsToShapes(parts.legL),
  { p: anim((f) => { const P = poseAt(f); return [legLAnchor[0] + P.legLDx, legLAnchor[1] + P.legLDy, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]), o: anim(standOp, 1, TOTAL, [2]) }));
add("legR", layer("legR", 16, IND.rig, legRAnchor, cellsToShapes(parts.legR),
  { p: anim((f) => { const P = poseAt(f); return [legRAnchor[0] + P.legRDx, legRAnchor[1] + P.legRDy, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]), o: anim(standOp, 1, TOTAL, [2]) }));
// 深色特征覆盖层(在金底上平移 = 头部表情/歪头)
add("face", layer("face", 11, IND.rig, faceAnchor, cellsToShapes(parts.face),
  { p: anim((f) => { const P = poseAt(f); return [faceAnchor[0] + P.headDx, faceAnchor[1] + P.headDy, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]), o: anim(standOp, 1, TOTAL, [2]) }));
add("eyes", layer("eyes", 10, IND.rig, eyeAnchor, cellsToShapes(parts.eyes),
  { p: anim((f) => { const P = poseAt(f); return [eyeAnchor[0] + P.headDx, eyeAnchor[1] + P.headDy, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]),
    s: anim((f) => [100, poseAt(f).eyeSy * 100, 100], 3, TOTAL, [0.4, 1, 0.4]), o: anim(standOp, 1, TOTAL, [2]) }));

// 趴姿(sleep,单层;眼格画成闭合细线)
{
  const lieCells = [];
  LIE.forEach((line, row) => [...line].forEach((ch, col) => { if (ch !== "." && !(eyeSet.has(col + "," + row) && ch === "K")) lieCells.push({ col, row, ch }); }));
  const shapes = cellsToShapes(lieCells);
  // 闭眼细线(眼格)
  shapes.unshift(group("lie_eyes", [...EYES.map(([c, r]) => rect(cx(c), cy(r) + 8, CELL, 6)), fill(COL.K)]));
  add("lie", layer("lie", 12, IND.rig, [256, 256], shapes, { o: anim(lieOp, 1, TOTAL, [2]) }));
}

// 根 null
add("rig", nullL("rig", IND.rig, null, [256, SHELF_Y], {
  p: anim((f) => { const P = poseAt(f); return [256 + P.rigDx, SHELF_Y + P.rigDy, 0]; }, 3, TOTAL, [0.4, 0.4, 0.4]),
}));

// 道具:像素爱心
const HEART = [[1, 0], [3, 0], [0, 1], [1, 1], [2, 1], [3, 1], [4, 1], [0, 2], [1, 2], [2, 2], [3, 2], [4, 2], [1, 3], [2, 3], [3, 3], [2, 4]];
function heartLayer(nm, ind, bx, by, period, phase, onlyPlay) {
  const s = 9; const w = 5 * s, h = 5 * s;
  return layer(nm, ind, IND.rig, [bx, by], patchShapes(HEART, bx - w / 2, by - h / 2, s, COL.heart),
    { o: anim((f) => { const t = heartTrack(f, period, phase); return [onlyPlay && !inState(f, "play") ? 0 : t.o]; }, 1, TOTAL, [2]),
      p: anim((f) => [bx, by + heartTrack(f, period, phase).dy, 0], 3, TOTAL, [0.5, 0.5, 0.5]),
      s: anim((f) => { const v = heartTrack(f, period, phase).s; return [v, v, 100]; }, 3, TOTAL, [0.6, 0.6, 0.6]) });
}
add("heartA", heartLayer("heartA", 30, 230, 150, 40, 0, false));
add("heartB", heartLayer("heartB", 31, 286, 150, 40, 20, true));

// 道具:像素 Z
const Z4 = [[0, 0], [1, 0], [2, 0], [3, 0], [2, 1], [1, 2], [0, 3], [1, 3], [2, 3], [3, 3]];
const Z3 = [[0, 0], [1, 0], [2, 0], [1, 1], [0, 2], [1, 2], [2, 2]];
function zzLayer(nm, ind, bx, by, pat, s, period, phase) {
  const w = (pat === Z4 ? 4 : 3) * s, h = (pat === Z4 ? 4 : 3) * s;
  return layer(nm, ind, IND.rig, [bx, by], patchShapes(pat, bx - w / 2, by - h / 2, s, COL.zz),
    { o: anim((f) => [zzTrack(f, period, phase).o], 1, TOTAL, [2]),
      p: anim((f) => [bx, by + zzTrack(f, period, phase).dy, 0], 3, TOTAL, [0.5, 0.5, 0.5]),
      s: anim((f) => { const v = zzTrack(f, period, phase).s; return [v, v, 100]; }, 3, TOTAL, [0.6, 0.6, 0.6]) });
}
add("zzBig", zzLayer("zzBig", 32, 300, 188, Z4, 8, 96, 0));
add("zzSmall", zzLayer("zzSmall", 33, 332, 168, Z3, 7, 96, 48));

// 道具:bark 气泡(像素「!」)
{
  const bubO = (f) => { if (!inState(f, "click")) return 0; const s = SEGS[segAt(f)]; const p = clamp01((f - s.start) / s.dur); return p < 0.12 ? (p / 0.12) * 100 : p < 0.7 ? 100 : (1 - (p - 0.7) / 0.3) * 100; };
  add("bubble", layer("bubble", 34, IND.rig, [330, 150], [
    group("bg", [rect(330, 150, 44, 36), fill(COL.bubble)]),
    group("bar", [rect(330, 146, 6, 14), fill(COL.accent, "accentColor")]),
    group("dot", [rect(330, 160, 6, 6), fill(COL.accent, "accentColor")]),
  ], { o: anim((f) => [bubO(f)], 1, TOTAL, [2]) }));
}

// ---- 显式 z 序(前 → 后;rig/null 末尾)----
const ORDER = ["bubble", "zzSmall", "zzBig", "heartB", "heartA",
  "eyes", "face", "base", "tail", "legL", "legR", "lie",
  "shelfph", "shelf", "background", "rig"];
const layers = ORDER.map((n) => { const l = STORE.get(n); if (!l) throw new Error("missing " + n); return l; });

const markers = SEGS.map((s) => ({ tm: s.start, cm: s.name, dr: s.dur }));
const slots = {
  bgColor: { p: { a: 0, k: COL.bg } },
  furColor: { p: { a: 0, k: COL.G } },
  furDarkColor: { p: { a: 0, k: COL.D } },
  accentColor: { p: { a: 0, k: COL.accent } },
};
const doc = { v: "5.7.0", fr: FR, ip: 0, op: TOTAL, w: W, h: H, nm: "Tn pixel pet — GOLDEN (full poses)", ddd: 0, assets: [], markers, slots, layers };

mkdirSync(OUT_DIR, { recursive: true });
writeFileSync(resolve(OUT_DIR, "lottie.json"), JSON.stringify(doc));
writeFileSync(resolve(OUT_DIR, "controls.json"), JSON.stringify({
  controls: [
    { sid: "bgColor", label: "背景海拔" },
    { sid: "furColor", label: "毛色(主 G)" },
    { sid: "furDarkColor", label: "毛色(垂耳 D)" },
    { sid: "accentColor", label: "磷光强调色" },
  ],
}, null, 2));

const kf = JSON.stringify(doc).match(/"t":/g)?.length || 0;
console.log(`OK  op=${TOTAL}f (${(TOTAL / FR).toFixed(1)}s)  layers=${layers.length}  keyframes≈${kf}  size=${(JSON.stringify(doc).length / 1024).toFixed(0)}KB`);
console.log("markers:", markers.map((m) => `${m.cm}@${m.tm}`).join("  "));
