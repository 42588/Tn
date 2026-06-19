# 命令历史与检索 模块索引

## 定位

本模块是「持久化结构化命令历史」与「统一模糊检索入口(picker)」两项能力的责任域母索引,只保留范围、目标、子文档表和与现有资产的映射。具体设计写入子文档,不在本文展开大方案。

来源:[终端项目互联网调研报告](../参考资料/终端项目互联网调研报告.md) 的可落地候选,经用户拍板保留**点 1(Atuin 式持久化历史)**与**点 2(fzf + zoxide 式统一检索)**,点 3(lazygit 本地 Git 面板)本轮不做。

## 北极星

**让「我刚才/上次/在这个目录里执行过什么」永不丢失、可被一个入口模糊检索并跳回原始输出。**

- 命令历史从「内存里、按会话、重启即丢」升级为「本地持久、跨会话可搜、带上下文」。
- 检索从「命令面板只能启动 profile」升级为「单入口模糊检索历史/文件/目录/分支/布局,带预览」。

## 责任范围

| 能力 | 借鉴 | 一句话 |
|---|---|---|
| 点 1 持久化结构化命令历史 | Atuin | SQLite 落库 command/cwd/exit/duration/session/host + **输出行区间**,三档作用域可搜 |
| 点 2 统一模糊检索入口 | fzf + zoxide | 一个 picker 聚合多数据源,fzf 子集语法 + frecency 目录排序 + 右侧预览 |

**边界**:不做云同步实现(只留接口口子)、不做 lazygit 式 Git 写操作(点 3 已搁置)、不替换终端核心。

## 与现有资产的映射

| 复用资产 | 源码入口 | 用途 |
|---|---|---|
| 命令块模型 | [tn-blocks](../../crates/tn-blocks/src/lib.rs) | 点 1 的数据来源(字段已齐) |
| Shell integration(OSC 133/633/7) | [tn-shell](../../crates/tn-shell/src/lib.rs) | 命令边界 + cwd 采集,**免 shell hook** |
| 小狗单击历史面板 | [terminal_view/mod.rs](../../crates/tn-ui/src/terminal_view/mod.rs) | 点 1 的 UI 升级起点(现仅当前会话) |
| 命令面板 | [workspace.rs](../../crates/tn-ui/src/workspace.rs) | 点 2 的 picker 浮层基础(现仅 profile) |
| 最近工作目录 | [terminal_view/launch.rs](../../crates/tn-ui/src/terminal_view/launch.rs) | 与 frecency 目录库合并 |
| Quick Look 预览 | [quick_look.rs](../../crates/tn-ui/src/quick_look.rs) | 点 2 右侧预览窗 |

## 子文档

| 子文档 | 状态 | 摘要 |
|---|---|---|
| [设计方案](设计方案.md) | 设计稿·未实现 | 源项目机制一手拆解 + 点 1/点 2 数据契约、接线点、frecency 公式、检索语法、分期与验收 |

## 状态

- 设计稿已建立,**实现未启动**。
- 分期顺序:**Phase 1 = 点 1 SQLite 历史库(同时是点 2 的数据源)→ Phase 2 = 点 2 统一 picker**。
- 实现登记见 [TODO](../../TODO.md) 待办区。

## 反向链接

- [文档治理索引](../文档治理索引.md)
- [终端项目互联网调研报告](../参考资料/终端项目互联网调研报告.md)
- [项目当前功能与状态](../项目当前功能与状态.md)
- [TODO](../../TODO.md)
