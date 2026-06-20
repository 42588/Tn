//! 极小 Lottie 播放器(只够跑「像素小狗」那一份 JSON)。
//!
//! 背景:本机无 cmake,rlottie/skia 无法构建;而我们生成的 `assets/pet/golden.json`
//! **只含 矩形 + 纯色填充 + 平移/缩放(无旋转)+ 线性关键帧 + 单层父子**(详见
//! `design/pet-lottie/`)。于是不引任何引擎/GPU,纯 Rust 把矩形按变换栅格化进 RGBA
//! 缓冲(轴对齐矩形的解析式覆盖率抗锯齿 → 亚像素平移平滑),`sid` 槽位颜色由主题解析。
//! 产物喂给 GPUI `RenderImage`(与 QuickLook 走同一条贴图路子)。
//!
//! 只支持我们用到的子集;换了 JSON 特性(描边/贝塞尔/旋转/蒙版)需在此补。

use serde_json::Value;
use std::collections::HashMap;

#[derive(Clone)]
enum Track {
    Static(Vec<f32>),
    Keys(Vec<(f32, Vec<f32>)>), // (time, value) 线性插值
}

impl Track {
    fn parse(v: &Value) -> Track {
        let animated = v.get("a").and_then(|a| a.as_i64()).unwrap_or(0) == 1;
        let k = match v.get("k") {
            Some(k) => k,
            None => return Track::Static(vec![0.0]),
        };
        if !animated {
            return Track::Static(num_vec(k));
        }
        let mut keys = Vec::new();
        if let Some(arr) = k.as_array() {
            for kf in arr {
                let t = kf.get("t").and_then(|t| t.as_f64()).unwrap_or(0.0) as f32;
                let s = kf.get("s").map(num_vec).unwrap_or_else(|| vec![0.0]);
                keys.push((t, s));
            }
        }
        if keys.is_empty() {
            Track::Static(vec![0.0])
        } else {
            Track::Keys(keys)
        }
    }

    fn sample(&self, f: f32) -> Vec<f32> {
        match self {
            Track::Static(v) => v.clone(),
            Track::Keys(keys) => {
                if f <= keys[0].0 {
                    return keys[0].1.clone();
                }
                let last = keys.last().unwrap();
                if f >= last.0 {
                    return last.1.clone();
                }
                for w in keys.windows(2) {
                    let (t0, s0) = &w[0];
                    let (t1, s1) = &w[1];
                    if f >= *t0 && f <= *t1 {
                        let u = if (t1 - t0).abs() < 1e-6 { 0.0 } else { (f - t0) / (t1 - t0) };
                        return s0
                            .iter()
                            .zip(s1.iter())
                            .map(|(a, b)| a + (b - a) * u)
                            .collect();
                    }
                }
                last.1.clone()
            }
        }
    }
    fn sample1(&self, f: f32) -> f32 {
        *self.sample(f).first().unwrap_or(&0.0)
    }
}

fn num_vec(v: &Value) -> Vec<f32> {
    match v {
        Value::Array(a) => a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect(),
        Value::Number(n) => vec![n.as_f64().unwrap_or(0.0) as f32],
        _ => vec![0.0],
    }
}

struct Rect {
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
}
struct ShapeGroup {
    rects: Vec<Rect>,
    color: [f32; 4], // 内联色(若无 sid)
    slot: Option<String>,
    fill_op: f32, // 0..1
}
struct Layer {
    a: Track,
    p: Track,
    s: Track,
    o: Track,
    parent: Option<i64>,
    ind: i64,
    groups: Vec<ShapeGroup>,
}

/// 段标记(state → 帧区间)。
#[derive(Clone)]
pub struct Marker {
    pub name: String,
    pub start: f32,
    pub dur: f32,
}

pub struct PetLottie {
    pub fr: f32,
    pub w: f32,
    pub h: f32,
    layers: Vec<Layer>, // 顺序同 JSON:index0 = 最前;栅格化按逆序(后→前)
    pub markers: Vec<Marker>,
    slots: HashMap<String, [f32; 4]>,
}

impl PetLottie {
    pub fn parse(json: &str) -> anyhow::Result<PetLottie> {
        let v: Value = serde_json::from_str(json)?;
        let fr = v.get("fr").and_then(|x| x.as_f64()).unwrap_or(60.0) as f32;
        let w = v.get("w").and_then(|x| x.as_f64()).unwrap_or(100.0) as f32;
        let h = v.get("h").and_then(|x| x.as_f64()).unwrap_or(84.0) as f32;

        let mut slots = HashMap::new();
        if let Some(obj) = v.get("slots").and_then(|s| s.as_object()) {
            for (name, def) in obj {
                if let Some(k) = def.get("p").and_then(|p| p.get("k")) {
                    slots.insert(name.clone(), to_rgba(&num_vec(k)));
                }
            }
        }

        let mut markers = Vec::new();
        if let Some(arr) = v.get("markers").and_then(|m| m.as_array()) {
            for m in arr {
                markers.push(Marker {
                    name: m.get("cm").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                    start: m.get("tm").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                    dur: m.get("dr").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
                });
            }
        }

        let mut layers = Vec::new();
        if let Some(arr) = v.get("layers").and_then(|l| l.as_array()) {
            for l in arr {
                let ty = l.get("ty").and_then(|x| x.as_i64()).unwrap_or(4);
                let ks = l.get("ks");
                let track = |name: &str, dflt: Vec<f32>| {
                    ks.and_then(|k| k.get(name))
                        .map(Track::parse)
                        .unwrap_or(Track::Static(dflt))
                };
                let mut groups = Vec::new();
                if ty == 4 {
                    if let Some(shapes) = l.get("shapes").and_then(|s| s.as_array()) {
                        for g in shapes {
                            if let Some(grp) = parse_group(g) {
                                groups.push(grp);
                            }
                        }
                    }
                }
                layers.push(Layer {
                    a: track("a", vec![0.0, 0.0, 0.0]),
                    p: track("p", vec![0.0, 0.0, 0.0]),
                    s: track("s", vec![100.0, 100.0, 100.0]),
                    o: track("o", vec![100.0]),
                    parent: l.get("parent").and_then(|x| x.as_i64()),
                    ind: l.get("ind").and_then(|x| x.as_i64()).unwrap_or(-1),
                    groups,
                });
            }
        }

        Ok(PetLottie { fr, w, h, layers, markers, slots })
    }

    /// 主题驱动换色:把某个槽位设为新 RGBA(0..1)。(预留:品种色目前内嵌固定)
    #[allow(dead_code)]
    pub fn set_slot(&mut self, name: &str, rgba: [f32; 4]) {
        self.slots.insert(name.to_string(), rgba);
    }

    pub fn marker(&self, name: &str) -> Option<&Marker> {
        self.markers.iter().find(|m| m.name == name)
    }

    /// 取某 ind 图层的合成仿射(scale, offset)/轴,用于父链组合(单层父子足够)。
    fn layer_affine(&self, layer: &Layer, f: f32) -> ((f32, f32), (f32, f32)) {
        let a = layer.a.sample(f);
        let p = layer.p.sample(f);
        let s = layer.s.sample(f);
        let (ax, ay) = (a.first().copied().unwrap_or(0.0), a.get(1).copied().unwrap_or(0.0));
        let (px, py) = (p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0));
        let (sx, sy) = (s.first().copied().unwrap_or(100.0) / 100.0, s.get(1).copied().unwrap_or(100.0) / 100.0);
        // screen = s*content + (pos - s*anchor)
        ((sx, px - sx * ax), (sy, py - sy * ay))
    }

    fn find_layer(&self, ind: i64) -> Option<&Layer> {
        self.layers.iter().find(|l| l.ind == ind)
    }

    /// 渲染 `frame`,`scale` = 渲染像素 / 逻辑单位(取整裁剪取 ceil)。返回直通 alpha 的 RGBA8。
    pub fn render_rgba(&self, frame: f32, scale: f32) -> (Vec<u8>, u32, u32) {
        let pw = (self.w * scale).ceil() as u32;
        let ph = (self.h * scale).ceil() as u32;
        let mut buf = vec![0f32; (pw * ph * 4) as usize]; // 直通 alpha 累积

        // 后→前(layers[0] 最前)
        for layer in self.layers.iter().rev() {
            if layer.groups.is_empty() {
                continue;
            }
            let layer_alpha = (layer.o.sample1(frame) / 100.0).clamp(0.0, 1.0);
            if layer_alpha <= 0.001 {
                continue;
            }
            let ((lsx, lox), (lsy, loy)) = self.layer_affine(layer, frame);
            // 父链(单层:rig)
            let (psx, pox, psy, poy) = if let Some(pind) = layer.parent {
                if let Some(parent) = self.find_layer(pind) {
                    let ((sx, ox), (sy, oy)) = self.layer_affine(parent, frame);
                    (sx, ox, sy, oy)
                } else {
                    (1.0, 0.0, 1.0, 0.0)
                }
            } else {
                (1.0, 0.0, 1.0, 0.0)
            };
            // 合成:screen = ps*(ls*content + lo) + po = (ps*ls)*content + (ps*lo+po)
            let tsx = psx * lsx;
            let tox = psx * lox + pox;
            let tsy = psy * lsy;
            let toy = psy * loy + poy;

            // Lottie 约定:shapes 数组中靠前者在上 → 逆序栅格化(末组先画,首组后画)。
            for g in layer.groups.iter().rev() {
                let col = g.slot.as_ref().and_then(|s| self.slots.get(s)).copied().unwrap_or(g.color);
                let alpha = col[3] * g.fill_op * layer_alpha;
                if alpha <= 0.001 {
                    continue;
                }
                for r in &g.rects {
                    // content 矩形 → screen(逻辑)→ × scale(像素)
                    let x0 = (tsx * (r.cx - r.w * 0.5) + tox) * scale;
                    let x1 = (tsx * (r.cx + r.w * 0.5) + tox) * scale;
                    let y0 = (tsy * (r.cy - r.h * 0.5) + toy) * scale;
                    let y1 = (tsy * (r.cy + r.h * 0.5) + toy) * scale;
                    blend_rect(&mut buf, pw, ph, x0, x1, y0, y1, col, alpha);
                }
            }
        }

        // f32 直通 → u8 RGBA
        let mut out = vec![0u8; (pw * ph * 4) as usize];
        for i in 0..(pw * ph) as usize {
            for c in 0..4 {
                out[i * 4 + c] = (buf[i * 4 + c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
        (out, pw, ph)
    }
}

/// 轴对齐矩形 over-blend(解析覆盖率 AA;直通 alpha)。
#[allow(clippy::too_many_arguments)]
fn blend_rect(buf: &mut [f32], pw: u32, ph: u32, x0: f32, x1: f32, y0: f32, y1: f32, col: [f32; 4], alpha: f32) {
    let (xl, xr) = (x0.min(x1), x0.max(x1));
    let (yt, yb) = (y0.min(y1), y0.max(y1));
    let ix0 = xl.floor().max(0.0) as u32;
    let ix1 = (xr.ceil() as i64).clamp(0, pw as i64) as u32;
    let iy0 = yt.floor().max(0.0) as u32;
    let iy1 = (yb.ceil() as i64).clamp(0, ph as i64) as u32;
    for py in iy0..iy1 {
        let cy = (py as f32 + 1.0).min(yb) - (py as f32).max(yt);
        if cy <= 0.0 {
            continue;
        }
        for px in ix0..ix1 {
            let cx = (px as f32 + 1.0).min(xr) - (px as f32).max(xl);
            if cx <= 0.0 {
                continue;
            }
            let cov = (cx * cy).clamp(0.0, 1.0);
            let sa = alpha * cov;
            if sa <= 0.0 {
                continue;
            }
            let idx = ((py * pw + px) * 4) as usize;
            let da = buf[idx + 3];
            let out_a = sa + da * (1.0 - sa);
            if out_a <= 0.0 {
                continue;
            }
            for c in 0..3 {
                let sc = col[c];
                buf[idx + c] = (sc * sa + buf[idx + c] * da * (1.0 - sa)) / out_a;
            }
            buf[idx + 3] = out_a;
        }
    }
}

fn parse_group(g: &Value) -> Option<ShapeGroup> {
    if g.get("ty").and_then(|x| x.as_str()) != Some("gr") {
        return None;
    }
    let it = g.get("it")?.as_array()?;
    let mut rects = Vec::new();
    let mut color = [1.0, 1.0, 1.0, 1.0];
    let mut slot = None;
    let mut fill_op = 1.0;
    for item in it {
        match item.get("ty").and_then(|x| x.as_str()) {
            Some("rc") => {
                let p = item.get("p").and_then(|p| p.get("k")).map(num_vec).unwrap_or(vec![0.0, 0.0]);
                let s = item.get("s").and_then(|s| s.get("k")).map(num_vec).unwrap_or(vec![1.0, 1.0]);
                rects.push(Rect {
                    cx: p.first().copied().unwrap_or(0.0),
                    cy: p.get(1).copied().unwrap_or(0.0),
                    w: s.first().copied().unwrap_or(1.0),
                    h: s.get(1).copied().unwrap_or(1.0),
                });
            }
            Some("fl") => {
                if let Some(c) = item.get("c") {
                    if let Some(sid) = c.get("sid").and_then(|x| x.as_str()) {
                        slot = Some(sid.to_string());
                    } else if let Some(k) = c.get("k") {
                        color = to_rgba(&num_vec(k));
                    }
                }
                if let Some(o) = item.get("o").and_then(|o| o.get("k")) {
                    fill_op = (num_vec(o).first().copied().unwrap_or(100.0) / 100.0).clamp(0.0, 1.0);
                }
            }
            _ => {}
        }
    }
    if rects.is_empty() {
        return None;
    }
    Some(ShapeGroup { rects, color, slot, fill_op })
}

fn to_rgba(v: &[f32]) -> [f32; 4] {
    [
        v.first().copied().unwrap_or(0.0),
        v.get(1).copied().unwrap_or(0.0),
        v.get(2).copied().unwrap_or(0.0),
        v.get(3).copied().unwrap_or(1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = include_str!("../assets/pet/golden.json");

    #[test]
    fn renders_states_to_png() {
        let pet = PetLottie::parse(GOLDEN).expect("parse");
        assert!(pet.w > 0.0 && pet.markers.len() >= 11);
        let scale = 6.0; // 100×84 → 600×504
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/pet-lottie");
        for name in ["idle", "running", "sleep", "feed", "scratch", "lickpaw", "spin", "stretch", "lookout"] {
            let m = pet.marker(name).unwrap();
            let frame = m.start + m.dur * 0.45;
            let (rgba, w, h) = pet.render_rgba(frame, scale);
            let img = image::RgbaImage::from_raw(w, h, rgba).unwrap();
            let _ = std::fs::create_dir_all(&out);
            img.save(out.join(format!("_rs_{name}.png"))).unwrap();
        }
    }

    /// 七品种总览:每行一个品种,列为代表性姿态。确认像素身份保留 + 运动通用 + 各品种 JSON 可解析。
    #[test]
    fn renders_all_breeds_sheet() {
        let asset_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/pet");
        let breeds = ["westie", "golden", "shepherd", "bichon", "maltese", "shihtzu", "poodle"];
        let poses = ["idle", "typing", "running", "success", "sleep", "spin"];
        let scale = 4.0;
        let (_, w, h) = {
            let json = std::fs::read_to_string(asset_dir.join("golden.json")).unwrap();
            PetLottie::parse(&json).unwrap().render_rgba(0.0, scale)
        };
        let mut sheet = image::RgbaImage::new(w * poses.len() as u32, h * breeds.len() as u32);
        for (ri, b) in breeds.iter().enumerate() {
            let json = std::fs::read_to_string(asset_dir.join(format!("{b}.json"))).unwrap();
            let pet = PetLottie::parse(&json).expect("parse breed");
            for (ci, pose) in poses.iter().enumerate() {
                let m = pet.marker(pose).unwrap();
                let f = m.start + m.dur * 0.5;
                let (rgba, fw, fh) = pet.render_rgba(f, scale);
                let frame = image::RgbaImage::from_raw(fw, fh, rgba).unwrap();
                for y in 0..fh.min(h) {
                    for x in 0..fw.min(w) {
                        sheet.put_pixel(ci as u32 * w + x, ri as u32 * h + y, *frame.get_pixel(x, y));
                    }
                }
            }
        }
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/pet-lottie");
        let _ = std::fs::create_dir_all(&out);
        sheet.save(out.join("_rs_all_breeds.png")).unwrap();
    }
}
