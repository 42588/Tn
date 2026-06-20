# 宠物动画 · 像素小狗矢量化重做(Lottie 路径,已接入终端)

> 状态:**已落地**(2026-06-20)。全部 **7 个品种** × 全套 **17 段动画**已接入终端运行时。
> 这是「三大特色之一 · 像素宠物」的渲染重做 —— 把原本 GPUI 逐帧重绘的像素狗,改用
> Lottie(Bodymovin)做**连续插值**播放,像素身份不变(只平移/缩放不旋转)。
>
> **未引入 skia**:运行时由自写的**纯 Rust mini 播放器** [`crates/tn-ui/src/pet_lottie.rs`](../../crates/tn-ui/src/pet_lottie.rs)
> 逐帧栅格化(矩形 + 填充 + 变换 + 线性关键帧 + 单层父子 + 解析式抗锯齿),喂给 GPUI
> `RenderImage`,无 cmake/GPU 依赖。`design/pet-lottie` 下的官方 skia 播放器仅作浏览器审稿用。
> 生成器逐品种出一份运行时 JSON(`crates/tn-ui/assets/pet/<breed>.json`),`pet.rs` 按 `breed` 选用。

## 这是什么

- **像素身份不变**:狗的网格 = `crates/tn-ui/src/pet.rs` 里的 `GOLDEN`(14 列 × 9 行,
  站姿 `rows` + 趴姿 `lie_rows`),格子、配色、眼/口/鼻/尾/腿坐标都按源码 1:1 取。
- **变的只是运动**:原型靠每帧重画整张雪碧图;这里把每个部件的 `dx/dy/缩放` 做成
  Lottie 关键帧,Skottie 连续插值 → **子像素平滑运动**,而方块始终轴对齐
  (只平移/缩放,**不旋转** —— 与原型动画词汇一致,见
  [docs/宠物/宠物交互动画实现方案.md](../../docs/宠物/宠物交互动画实现方案.md) §0.1)。
- **磷光纪律**:大面积不透明海拔;强调色 `#5BE7C4` 只给「活信号」(岗台磷光段在
  typing/running 变亮),毛色为暖金,无装饰 glow。

## 全套姿态(一条时间轴顺序演完,每段打 marker;运行时按状态机跳段)

`peek → idle → typing → running → success → error → hover → click → play → drag → sleep`
`→ feed(投喂) → scratch(抓痒) → lickpaw(舔爪) → spin(追尾兜圈) → stretch(伸懒腰) → lookout(望屏外)`

> 循环段周期均**锁到运行时循环体长度的整除数**(无缝循环,跨循环点不跳帧);
> success/play/feed 落地用**挤压拉伸**(矮宽↔高瘦);spin = 绕地面小圈兜跑(纵深缩放,不做水平挤压)。

| 段       | 画法(平移/缩放)                                                  |
| -------- | ----------------------------------------------------------------- |
| peek     | 整体从岗台线下 ease-out 升起 + 落定回弹                            |
| idle     | 呼吸(rig dy)+ 慢摇尾(tail dy)+ 周期眨眼(eye scaleY 瞬闭)     |
| typing   | 头部特征轻抬 + 岗台磷光段变亮(活信号)                            |
| running  | 双腿对角交替迈步(leg dx/dy)+ 步态颠簸(rig dy)+ 快摇尾 + 磷光亮 |
| success  | 一次蹦跳弧线(预蹲→上跳→回弹)+ 像素爱心上浮                       |
| error    | 身体下沉 + 委屈眼(eye scaleY→细线)+ 特征下垂                     |
| hover    | 眯眼(eye scaleY)+ 中速摇尾                                       |
| click    | 歪头杀:深色特征在金底上右下平移 + 像素「!」气泡                   |
| play     | 蹦跳 + 双爱心交替 + 快摇尾                                         |
| drag     | 整体悬空抬起 + 四肢下垂摆荡                                        |
| sleep    | 切趴姿网格(`lie_rows`)+ 闭眼细线 + 像素「Zz」上浮               |

## 关键实现:无缝分层

整只毛色轮廓 = **单层 `base`**(无内部接缝);只有深色特征(眼/口/鼻)与腿、尾作为
覆盖层在其上平移。头部「动作」= 深色特征在实心金底上滑动 → 永不露背景缝
(金底始终兜底)。眼后、口鼻后都补了金底格子。格子放大 1px 让相邻同色方块重叠,
消除分层交界处的抗锯齿发丝缝。

## 可换色槽(properties 面板实时改)

`bgColor`(背景海拔)· `furColor`(主毛色 G)· `furDarkColor`(垂耳 D)· `accentColor`(磷光)。

## 怎么生成 / 怎么预览

播放器是官方 `diffusionstudio/lottie`(skia/Skottie),**本地 scaffold,不入库**。

```bash
# 1) 在本目录 scaffold 官方播放器(若尚未 scaffold)
npx degit diffusionstudio/lottie .
npm install                       # postinstall 会把 canvaskit.wasm 拷进 public/

# 2) 生成/更新动画(自写生成器,一跑同时出 player 审稿件 + 7 份运行时资产)
node script/gen_pet_lottie.mjs    # → 审稿:public/projects/main-project/scene-1/{lottie.json,controls.json}(金毛)
                                  # → 运行时:crates/tn-ui/assets/pet/<breed>.json ×7(终端实跑)

# 3) 起播放器,浏览器开预览
npm run dev                       # http://localhost:3030/main-project/scene-1
#   定帧检视:  .../scene-1?frame=350
```

入库的只有:`script/gen_pet_lottie.mjs`、生成的 `lottie.json` / `controls.json`、本 README。

## 已落地(as-built)

- **渲染路径已定**:不切 skia,运行时用纯 Rust mini 播放器栅格化(见顶部说明);GPUI quad 直绘作回退。
- **7 品种全覆盖**:生成器按「网格 + 部件坐标」泛化,`BREEDS` 表逐品种出运行时 JSON;
  运动(`osc`/`poseAt`)对所有品种完全相同,只有像素网格/配色/部件锚点不同。`pet.rs::lottie_for(breed)` 选用,换品种即换皮。
- **投喂 + 微动作已覆盖**:feed/scratch/lickpaw/spin/stretch/lookout 全部接入,接 `pet.rs` 状态机(Feed 上下文、活物引擎 Micro)。
- **立耳 perk 已接入**:竖耳品种(西高地/德牧)的耳尖单列一层(`earDy` 通道),typing/hover/lookout/`earperk`
  微动作时立起 + 耳尖抖动,error 时耷下;`base` 留同色兜底,平移不露缝。垂耳品种无耳层 → 自动空操作。
  新增 `earperk` 段,`pet.rs::lottie_segment` 把 `Micro::EarPerk` 映射到它。

## 后续可选增强

- 运动轨迹在各品种间字节级重复(锚点不同),如需可抽出共享运动轨道减小体积。
