# Flint

[English](./README.md) | [简体中文](./README.zh-CN.md)

Flint 是一个面向终端工作流的 [Zed](https://github.com/zed-industries/zed) 分支，专为通过命令行使用 Codex、Claude Code、Pi 和 OpenCode 等工具的开发者打造。

它保留了 Zed 快速、GPU 加速的编辑器、语言支持、Git 工具和扩展生态，同时以专注于终端编程智能体的工作空间取代了内置 AI 产品。

![Flint 工作空间，其中包含智能体线程面板和 Claude Code 终端会话](assets/screenshots/flint-workspace.png)

## 为什么选择 Flint？

Flint 将 IDE 的编辑与项目工具和终端原生工作流的直接性结合起来。文件、终端、差异、Git 状态和长时间运行的编程智能体会话都位于同一个工作空间中，让你可以从命令行委派任务，并在不切换应用的情况下审查结果。

| | Flint | 传统 IDE | 终端模拟器 |
| --- | --- | --- | --- |
| 代码智能与扩展 | 内置 | 内置 | 通过命令行工具添加 |
| 以终端作为主要工作空间 | 是 | 通常只是次要面板 | 是 |
| 持久化 CLI 智能体会话 | 作为线程与文件、差异并列组织 | 通常被专有聊天界面取代 | 支持，但以终端标签页或窗口管理 |
| Git 变更与可视化差异审查 | 集成 | 集成 | 主要通过命令行完成 |
| 智能体凭据与配置 | 使用现有 CLI 配置 | 通常需要在 IDE 中单独配置 | 使用现有 CLI 配置 |

Flint 适合既希望获得 IDE 的项目感知与可视化审查工具，又不愿放弃命令行智能体、终端工作流或本地配置控制权的开发者。

### 新增功能

- **一流的终端体验：** 新终端默认在中央工作区中以标签页打开，与文件和差异并列。
- **Codex、Claude Code、Pi 和 OpenCode 线程：** 使用现有的身份验证和配置，直接在终端支持的线程中启动任意受支持的 CLI。
- **智能体线程面板：** 组织会话、发现本机或已连接远程主机上的近期线程，并使用各智能体历史记录中的标题恢复工作。
- **智能体状态与连续性：** 查看 Codex 和 Claude Code 的套餐用量与重置倒计时；在线程需要关注时接收桌面通知；还可选择在重启 Flint 后重新打开可恢复的会话。
- **由智能体控制的终端：** 安装 Flint 可选的 `flintctl` 控制技能后，智能体可从本地和受支持的远程会话中检查终端状态、发送输入、等待输出，以及创建终端或同级智能体线程。
- **跨智能体交接：** 从活跃的本地线程预览一份范围明确的交接文档，然后通过另一个受支持的智能体在新线程中继续工作。
- **远程智能体线程：** 在 SSH 远程主机上运行受支持的智能体，可使用远程主机已配置的 CLI（`Direct`），也可使用由 Flint 固定并管理、且流量经由本地 Flint 转发的二进制文件（`Tunneled`）。
- **可配置的智能体工作流：** 设置命令、参数、环境变量、工作目录、可见性、面板位置和默认恢复选项。
- **丰富的 Markdown 预览：** 当 Node.js 可用时，通过内置 MathJax 渲染行内和块级 LaTeX 公式；同时支持更多 Mermaid 图表类型以及带 YAML frontmatter 的图表。
- **CSV 表格预览：** 以表格形式打开已保存的 `.csv` 文件，各列可独立调整宽度，并固定显示行号列。
- **本地化界面：** Flint 支持英文和简体中文，可在初始设置期间或设置中选择界面语言。
- **更高效的可视化导航与审查：** 通过主题感知颜色区分文件类型，以易识别的品牌图标区分智能体；可直接从编辑器工具栏打开项目变更，还可使用 Flint 的 Git 与差异视图将工作区直接与分支或提交进行比较。

![智能体线程菜单，其中包含“交接给 Codex”和“交接给 Pi”操作](assets/screenshots/handoff.png)

### 移除功能

Flint 不包含 Zed 原生的智能体与聊天界面、托管 AI 模型、模型提供商配置、Copilot 或编辑预测、账户和计费界面，以及实时协作与通话功能。由此形成了一个更精简、本地优先的产品界面，将智能体行为和凭据交由你已经使用的 CLI 工具管理。

## 试用 Flint

从 [GitHub Releases](https://github.com/shenghsi/flint/releases/latest) 下载适用于 macOS、Linux 或 Windows 的最新稳定版本。每日构建版本可从滚动更新的 [`nightly` release](https://github.com/shenghsi/flint/releases/tag/nightly) 下载。

### macOS

将 Flint 移入 `/Applications` 后，请移除隔离属性，以便 macOS 允许打开这个未签名的应用：

```sh
xattr -cr /Applications/Flint.app
```

### Linux

将 Flint 安装到 `~/.local`（无需 root 权限，并支持应用内自动更新）：

```sh
curl -f https://raw.githubusercontent.com/shenghsi/flint/main/script/install.sh | sh
```

若要安装 Nightly 而不是 Stable，请设置 `ZED_CHANNEL=nightly`：

```sh
curl -f https://raw.githubusercontent.com/shenghsi/flint/main/script/install.sh | ZED_CHANNEL=nightly sh
```

Nightly 会与 Stable 并存，安装为 `~/.local/flint-nightly.app`。Nightly 每六小时检查一次滚动更新的 `nightly` release。`~/.local/bin` 中的 `flint` 命令会指向最近安装的频道。

如果 `~/.local/bin` 尚未加入你的 `PATH`，请将其加入，以便通过 `flint` 启动 Flint。

`.deb` 和 `.rpm` 软件包会将 Flint 安装到系统级目录 `/usr/lib/flint`。这些版本由软件包管理器管理，因此应用内自动更新会被禁用；请使用 `apt`、`dnf` 等工具进行更新。

### 远程开发

Flint 支持 SSH 和 WSL 远程开发，同时将编辑器界面保留在本地。文件、终端、任务、语言服务器和智能体线程均在远程主机上运行。

远程智能体线程既可以使用远程主机自身的网络（`Direct`），也可以使用由 Flint 管理的路由（`Tunneled`）。使用 `Tunneled` 时，Flint 可以在远程主机上配置固定版本的 Codex、Claude Code、Pi 和 OpenCode 二进制文件，并将受支持的提供商流量通过本地 Flint 连接转发。这在远程计算机的互联网（VPN）访问受限时尤其有用。Flint 会在标题栏和项目选择器中标记使用隧道路由的 SSH 项目。

有关设置、SSH 连接选项、端口转发和 `agent_route` 选项，请参阅[远程开发](./docs/src/remote-development.md)。

### 开发 Flint

- [在 macOS 上构建 Flint](./docs/src/development/macos.md)
- [在 Linux 上构建 Flint](./docs/src/development/linux.md)
- [在 Windows 上构建 Flint](./docs/src/development/windows.md)

### 扩展

Flint 与 [Zed 扩展注册表](https://zed.dev/extensions)兼容。扩展无需修改即可安装和运行。

### 许可证

Flint 源代码主要采用 GPL-3.0-or-later 许可，标注的组件则采用 Apache-2.0 许可。

为了使 CI 通过，必须正确提供第三方依赖项的许可证信息。

我们使用 [`cargo-about`](https://github.com/EmbarkStudios/cargo-about) 自动满足开源许可证要求。如果 CI 失败，请检查以下情况：

- 是否为你创建的 crate 报告 `no license specified` 错误？如果是，请在该 crate 的 Cargo.toml 中的 `[package]` 下添加 `publish = false`。
- 是否为某个依赖项报告 `failed to satisfy license requirements`？如果是，请先确定该项目采用的许可证，并确认当前系统足以满足其要求。如有疑问，请咨询律师。确认无误后，将许可证的 SPDX 标识符添加到 `script/licenses/flint-licenses.toml` 的 `accepted` 数组中。
- `cargo-about` 是否无法找到某个依赖项的许可证？如果是，请按照 [cargo-about 文档](https://embarkstudios.github.io/cargo-about/cli/generate/config.html#crate-configuration)的说明，在 `script/licenses/flint-licenses.toml` 末尾添加 clarification 字段。

### 致谢

Flint 构建于 Zed Industries 的 [Zed](https://github.com/zed-industries/zed) 之上。感谢他们对开源社区的贡献。
