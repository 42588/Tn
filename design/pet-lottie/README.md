# 宠物动画 · 像素小狗矢量化重做(Lottie / Skottie 探索)

> 状态:**原型探索**(换渲染方向)。代表犬 = 金毛,全套上下文姿态已出可播放原件。
> 这是对「三大特色之一 · 像素宠物」的渲染方向探索 —— 把原本 GPUI 逐帧重绘的
> 像素狗,改用 Lottie(Bodymovin)在 skia/Skottie 上做**连续插值**播放。
> 尚未接入产品;落地与否、是否引入 skia 渲染层,见下文「待定夺」。

## 这是什么

- **像素身份不变**:狗的网格 = `crates/tn-ui/src/pet.rs` 里的 `GOLDEN`(14 列 × 9 行,
  站姿 `rows` + 趴姿 `lie_rows`),格子、配色、眼/口/鼻/尾/腿坐标都按源码 1:1 取。
- **变的只是运动**:原型靠每帧重画整张雪碧图;这里把每个部件的 `dx/dy/缩放` 做成
  Lottie 关键帧,Skottie 连续插值 → **子像素平滑运动**,而方块始终轴对齐
  (只平移/缩放,**不旋转** —— 与原型动画词汇一致,见
  [docs/宠物/宠物交互动画实现方案.md](../../docs/宠物/宠物交互动画实现方案.md) §0.1)。
- **磷光纪律**:大面积不透明海拔;强调色 `#5BE7C4` 只给「活信号」(岗台磷光段在
  typing/running 变亮),毛色为暖金,无装饰 glow。

## 全套姿态(一条时间轴顺序演完,每段打 marker)

`peek → idle → typing → running → success → error → hover → click → play → drag → sleep`

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

# 2) 生成/更新动画(自写生成器,产物落到播放器的 scene-1)
node script/gen_pet_lottie.mjs    # → public/projects/main-project/scene-1/{lottie.json,controls.json}

# 3) 起播放器,浏览器开预览
npm run dev                       # http://localhost:3030/main-project/scene-1
#   定帧检视:  .../scene-1?frame=350
```

入库的只有:`script/gen_pet_lottie.mjs`、生成的 `lottie.json` / `controls.json`、本 README。

## 待定夺(落地前)

- 是否真的把宠物渲染从 GPUI quad 切到 skia/Skottie(引擎取舍、体积、与磷光契约的一致性)。
- 其余 6 个品种(西高地/德牧/比熊/马尔济斯/西施/泰迪):生成器已按「网格 + 部件坐标」
  泛化,补品种 = 填 `rows/lie_rows/eyes/tail/leg` 即可,本轮先只做金毛代表犬。
- 投喂/微动作/工作共情等完整规则尚未覆盖(本轮聚焦核心 11 姿态)。
