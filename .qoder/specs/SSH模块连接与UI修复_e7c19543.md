# SSH 模块连接与 UI 修复方案

## Context

用户通过 SSH Quick Connect 面板连接本机 WSL2 的 SSH 服务（172.x.x.x:2222），遇到以下问题：
1. **连接后完全无反馈** — 根因是 `check_server_key` 对不在 `known_hosts` 中的主机直接返回 `false` 拒绝连接，且 TCP 连接本身无超时设置，若主机不可达则无限挂起
2. **密码输入框样式不符** — 当前使用 `ui_accent` 纯色背景 + 右上角绝对定位，不符合 Calm Glass 毛玻璃设计语言
3. **缺乏用户提示** — 连接过程无任何状态反馈（Connecting... / 失败原因 / 超时提示等）

---

## Task 1: 修复 known_hosts 严格拒绝 + 添加连接超时

**文件**: `crates/tn-pty/src/ssh.rs`

### 1.1 放宽 `check_server_key` 策略

当前 `check_server_key` 对 `known_hosts` 中不存在的主机直接返回 `Ok(false)` 导致连接静默失败。改为：
- known_hosts 中找到且匹配 → 接受
- known_hosts 文件不存在 / 主机未记录 → **接受连接**，并将主机密钥追加写入 `~/.ssh/known_hosts`（首次信任模式，TOFU）
- known_hosts 中找到但密钥**不匹配** → 拒绝（可能中间人攻击），通过 `in_tx` 向终端输出明确的红色警告信息

```rust
async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
    let known_hosts_path = home_dir()
        .map(|h| h.join(".ssh").join("known_hosts"))
        .unwrap_or_else(|| PathBuf::from("known_hosts"));

    // 文件不存在 → TOFU (Trust On First Use)
    if !known_hosts_path.exists() {
        tracing::info!("SSH: known_hosts not found, accepting key (TOFU)");
        let _ = append_known_host(&known_hosts_path, &self.host, self.port, key);
        return Ok(true);
    }

    match russh::keys::check_known_hosts_path(&self.host, self.port, key, &known_hosts_path) {
        Ok(true) => Ok(true),  // 已知且匹配
        Ok(false) => {
            // 未记录 → 接受并追加
            tracing::info!("SSH: Host {}:{} not in known_hosts, accepting (TOFU)", self.host, self.port);
            let _ = append_known_host(&known_hosts_path, &self.host, self.port, key);
            Ok(true)
        }
        Err(_) => {
            // 密钥不匹配 → 拒绝
            tracing::warn!("SSH: HOST KEY MISMATCH for {}:{}!", self.host, self.port);
            Ok(false)
        }
    }
}
```

新增辅助函数 `append_known_host` 将主机公钥追加到 `known_hosts` 文件。

### 1.2 添加 TCP 连接超时

在 `run_session` 的 `client::connect` 处包裹 `tokio::time::timeout`（15 秒），超时后输出明确的错误消息并进入重连循环：

```rust
let connect_res = tokio::time::timeout(
    Duration::from_secs(15),
    client::connect(config.clone(), (cfg.host.as_str(), cfg.port), handler),
).await;

let mut handle = match connect_res {
    Ok(Ok(h)) => h,
    Ok(Err(e)) => {
        let msg = format!("\r\n\x1b[31m[SSH]\x1b[0m 连接失败: {}\r\n正在 5 秒后重试...\r\n", e);
        let _ = in_tx.send(msg.into_bytes());
        // ... reconnect logic
    }
    Err(_) => {
        let msg = format!("\r\n\x1b[31m[SSH]\x1b[0m 连接超时 ({}:{}, 15s)\r\n正在 5 秒后重试...\r\n", cfg.host, cfg.port);
        let _ = in_tx.send(msg.into_bytes());
        // ... reconnect logic
    }
};
```

---

## Task 2: 添加连接状态反馈

**文件**: `crates/tn-pty/src/ssh.rs`, `crates/tn-pty/src/lib.rs`, `crates/tn-ui/src/terminal_view/mod.rs`

### 2.1 连接中的实时文本反馈

在 `run_session` 关键阶段通过 `in_tx` 向终端写入状态文本（利用 ANSI 颜色）：

```
\x1b[36m[SSH]\x1b[0m 正在连接 user@host:port ...
\x1b[36m[SSH]\x1b[0m 验证主机密钥...
\x1b[36m[SSH]\x1b[0m 尝试密钥认证 (~/.ssh/id_ed25519)...
\x1b[36m[SSH]\x1b[0m 密钥认证失败, 尝试密码认证...
\x1b[32m[SSH]\x1b[0m 连接成功! 打开远程 shell...
```

### 2.2 新增 `PtyEvent::Connecting` 状态事件（可选）

在 `PtyEvent` 枚举中新增 `Connecting { host: String }` 变体，让 UI 层可在终端视图 header 或状态区域显示连接中状态。这是一个增量改进，主要依靠 2.1 的文本反馈作为最小可行方案。

---

## Task 3: 密码输入框 UI 重构（Calm Glass 风格）

**文件**: `crates/tn-ui/src/terminal_view/mod.rs`（render 部分，~L1752-1786）

### 当前问题
- 背景色用 `self.ui_accent` 纯色 + 0.95 不透明度 → 突兀
- 定位：absolute top(40.) right(20.) → 角落偏置，不居中
- 英文提示文案
- 无 Calm Glass 材质（无渐变、无 rim、无 shadow）

### 改为 Calm Glass 浮层

参照 `workspace.rs` 中 `render_ssh_prompt`（SSH Quick Connect 面板）的风格：

```rust
let ssh_prompt = self.ssh_password_prompt.as_ref().map(|(prompt, _)| {
    let t = &self.config.theme;
    let ui = &t.ui;

    // 输入区域：遮罩密码 + 光标
    let input_row = div()
        .flex().flex_row().items_center()
        .gap(px(10.)).px(px(16.)).py(px(13.))
        .text_size(px(14.))
        .child(div().child(icon("lock", 16., ui.muted)))
        .child(
            div().flex().flex_row().items_center()
                .font_family(mono.clone())
                .when(!self.ssh_password_input.is_empty(), |d| {
                    d.child(div().text_color(col(ui.foreground))
                        .child(SharedString::from("•".repeat(self.ssh_password_input.len()))))
                })
                .child(div().text_color(col(ui.muted)).child("▏"))
                .when(self.ssh_password_input.is_empty(), |d| {
                    d.child(div().ml(px(2.)).text_color(col(ui.muted)).child("输入密码"))
                })
        );

    // 面板：Calm Glass 材质
    let panel = crate::style::shadowed(
        div().flex().flex_col()
            .w(px(400.))
            .rounded(px(R_PANEL))
            .overflow_hidden()
            .border_1().border_color(rgba(RIM))
            .bg(linear_gradient(180.,
                linear_color_stop(cola(ui.palette_bg, 0.92), 0.),
                linear_color_stop(rgba(0x161826eb), 1.),
            ))
            .child(
                div().p(px(12.)).text_size(px(12.5))
                    .text_color(col(ui.foreground))
                    .font_weight(FontWeight(560.))
                    .child(SharedString::from(prompt.clone()))
            )
            .child(div().h(px(1.)).bg(rgba(0xffffff0f)))
            .child(input_row)
            .child(div().h(px(1.)).bg(rgba(0xffffff0f)))
            .child(
                div().p(px(12.)).text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child("Enter 提交 · Esc 取消")
            ),
        vec![crate::style::soft_shadow(40.0, 120.0, -30.0, 0.9)],
    );

    // 全屏遮罩 + 居中
    div().absolute().size_full()
        .flex().items_center().justify_center()
        .bg(rgba(0x0a0b118c))  // scrim
        .child(panel)
});
```

关键改动：
- 使用 `linear_gradient` + `palette_bg` + `RIM` 边框 → Calm Glass 标准材质
- 全屏遮罩 `0x0a0b118c` + flex 居中（与命令面板一致）
- `soft_shadow` 柔和阴影
- 中文提示文案
- prompt 标题使用 `ui.foreground` + FontWeight(560.) 加粗

---

## Task 4: 错误信息和重连提示优化

**文件**: `crates/tn-pty/src/ssh.rs`

- 将所有 `[SSH]` 前缀消息改为中文，并使用 ANSI 颜色标记：
  - 连接失败 → `\x1b[31m[SSH]\x1b[0m` 红色
  - 连接成功 → `\x1b[32m[SSH]\x1b[0m` 绿色  
  - 连接中 → `\x1b[36m[SSH]\x1b[0m` 青色
  - 断线 → `\x1b[33m[SSH]\x1b[0m` 黄色
- 认证失败时不直接 `return Err`，改为先向终端输出具体失败原因再退出
- 重连消息改为：`\x1b[33m[SSH]\x1b[0m 连接已断开。5 秒后自动重连... (Ctrl+D 取消)`

---

## Task 5: 密钥不匹配的安全警告

**文件**: `crates/tn-pty/src/ssh.rs`

当 known_hosts 中存在主机但密钥不匹配时（潜在中间人攻击），通过 `PtyEvent` 新增变体 `HostKeyMismatch { host, message }` 通知 UI 层，在终端区域渲染一个醒目的红色警告浮层：

```
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
@ 警告: 远程主机标识已更改!     @
@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@
可能存在中间人攻击或主机已重装。
主机: 172.x.x.x:2222
请手动验证或删除 ~/.ssh/known_hosts 中对应条目。
连接已中止。
```

---

## 修改文件汇总

| 文件 | 改动范围 |
|------|----------|
| `crates/tn-pty/src/ssh.rs` | check_server_key TOFU 策略 + 连接超时 + 状态文本 + 中文消息 + append_known_host |
| `crates/tn-pty/src/lib.rs` | PtyEvent 可选新增变体（Connecting / HostKeyMismatch） |
| `crates/tn-ui/src/terminal_view/mod.rs` | 密码输入框 render 重构为 Calm Glass 风格 |

---

## 验证方式

1. **编译验证**: `cargo check -p tn-pty -p tn-ui`
2. **单元测试**: `cargo test -p tn-pty` — 确保 SshConfig::parse 等现有测试不被破坏
3. **手动验证**:
   - 启动 WSL2 中的 sshd（端口 2222）
   - 通过 SSH Quick Connect 面板连接 `user@172.x.x.x:2222`
   - 验证：终端实时显示连接状态文本 → 首次连接 TOFU 自动接受 → 密码提示框为 Calm Glass 风格 → 输入密码后成功连入 shell
   - 验证超时：连接不存在的 IP，15 秒后显示超时消息
   - 验证重连：已连接的会话断网后显示黄色断线消息 + 自动重连
