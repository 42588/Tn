# Tn 捆绑资源清单(bundled resources)

> 这份清单是「以免后续忘记」的唯一权威台账:Tn 发行时**所有**外部资源都登记在此,
> 无论它是编译进 `tn.exe` 还是作为独立文件随装。新增资源**必须**在此登记。

Tn 的捆绑资源分两类,机制完全不同:

## A. 已编译进 `tn.exe`(compile-time,运行时无独立文件)

这些通过 `include_bytes!` / `include_str!` 在编译期硬编码进二进制,装机时**不需要**额外文件。

| 资源 | 源路径 | 载入处 | 说明 |
| --- | --- | --- | --- |
| 字体 | `crates/tn-ui/assets/fonts/` 下 JetBrainsMono Nerd Font ×4 + Inter ×3 + Space Grotesk ×2 | `crates/tn-ui/src/lib.rs` | 等宽/UI/展示字,`include_bytes!` 进 exe,运行时不依赖系统安装 |
| 窗口图标 | `crates/tn-ui/assets/tn.ico` | `crates/tn-ui/src/platform.rs` | |
| 默认配置 | `config/config.toml` | `crates/tn-config/src/config.rs` | |
| 默认主题 | `config/themes/tn-dark.toml` | `crates/tn-config/src/theme.rs` | |

> 字体/图标/配置已经在 exe 里了,**不用也不该**再做成独立文件。

## B. 原生 sidecar 二进制(随 exe 同目录,不能塞进 exe 运行)

这些是 Windows 必须以**磁盘上真实文件**载入的原生件:DLL 要 `LoadLibrary`、helper exe 要
`CreateProcess`。它们**无法**从 exe 内部直接运行,所以装机时必须和 `tn.exe` 放在同一目录。

布局:`vendor/<名字>/<arch>/<文件>`。`build.rs` 按目标 arch 把它们拷到 `target/<profile>/`
(`tn.exe` 旁),开发 `cargo run` 与发行包都能就近找到;装机时由安装器放进安装目录。

| 文件 | 路径 | 版本 | 来源 / 签名 | 为什么需要 | 载入处 |
| --- | --- | --- | --- | --- | --- |
| `conpty.dll` | `vendor/conpty/x64/` | 1.24.2605.12001 | NuGet `Microsoft.Windows.Console.ConPTY`,**微软签名** | 系统 conhost(Win10 19041)以 legacy 模式跑 ConPTY,吞掉子进程的备用屏/鼠标 DECSET → codex 等全屏 TUI 滚不动。新版走 passthrough 如实转发 | `portable-pty` `load_conpty` 自动优先用 exe 旁的 `conpty.dll` |
| `OpenConsole.exe` | `vendor/conpty/x64/` | 1.24.2605.12001 | 同上,**微软签名** | 上面 `conpty.dll` 会从自身目录拉起它作为真正的控制台宿主,故必须同目录 | 由 `conpty.dll` 启动 |
| `pdfium.dll` | `vendor/pdfium/x64/` | 150.0.7869.0 | pdfium-binaries 社区构建(`bblanchon`),**未签名**;BSD-3 / Apache-2.0 | Quick Look 的 PDF 渲染引擎(`pdfium-render` 经 `LoadLibrary` 绑定);ABI 与 crate 版本耦合,故捆绑指定版而非用系统库 | `crates/tn-ui/src/quick_look.rs`:先 exe 旁、后系统库 |

## 接线方式

- `crates/tn-app/build.rs`:遍历 `vendor/*/<arch>/`,把每个文件拷到 exe 同目录(按文件大小增量拷,
  不动正在被占用的 DLL)。**新增 sidecar 只需丢进 `vendor/<名字>/<arch>/`,无需改 build.rs。**
- 各 loader 统一「先 exe 旁、后系统」的查找顺序,使开发态与装机态一致。
- 目前只捆绑 **x64**。`arm64` / `x86` 如需支持,放进对应 `vendor/<名字>/<arch>/` 即可;build.rs 已按
  `CARGO_CFG_TARGET_ARCH` 选目录。

## 安装器(可安装)

发行单元 = 安装器,把 `tn.exe` + 上述 B 类 sidecar 装进同一安装目录(B 类就近被找到,A 类已在 exe 内)。

用 **cargo-packager**(NSIS)产出 per-user 安装器:

```sh
cargo install cargo-packager --locked          # 一次性
cargo build --release -p tn-app                 # build.rs 把 sidecar 拷到 target/release/
cargo packager --release -p tn-app --out-dir dist -f nsis
# → crates/tn-app/dist/tn_<版本>_x64-setup.exe
```

- 配置在 [`crates/tn-app/Cargo.toml`](../Cargo.toml) 的 `[package.metadata.packager]`:`resources`
  把三个 sidecar 一并打进安装目录,`installMode = "currentUser"`(免 UAC,装到
  `%LOCALAPPDATA%\Programs\Tn`),图标用 `tn.ico`。
- 安装器自带 `uninstall.exe`,并登记到「添加/删除程序」。
- 实测验证:静默装到临时目录后,安装目录内 `tn.exe + conpty.dll + OpenConsole.exe + pdfium.dll`
  四件齐全,布局正确;静默卸载干净无残留。
- 安装包版本号取自 workspace `version`(当前 `0.0.0`);正式发布前在根 `Cargo.toml` 调版本即可。

## 注记

- 仓库根历史上散落的 `pdfium.dll` / `pdfium.tgz` / `pdfium-win-x64.tgz` 已从跟踪中移除(`.gitignore`
  锚定忽略),改由本目录 `vendor/pdfium/x64/pdfium.dll` 作为唯一权威副本。
- 二进制合计约 8 MB(conpty 0.1 + OpenConsole 1 + pdfium 7)。若日后嫌仓库增重,可迁 Git LFS;
  当前规模直接入库可接受。
