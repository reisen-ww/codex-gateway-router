# AI Toolbox Gateway Router

这是基于 [AI Toolbox](https://github.com/coulsontl/ai-toolbox) 的 MIT fork，增加了 Codex 多站点聚合路由：把多个 API 站点的模型合并进 Codex 模型列表，并按站点前缀自动路由。

<p align="center">
  <img src="tauri/icons/128x128@2x.png" alt="AI Toolbox Gateway Router Logo" width="128" height="128">
</p>

<p align="center">
  <strong>个人 AI 工具箱</strong> - 一站式管理 AI 编程助手配置
</p>

<p align="center">
  <a href="https://github.com/coulsontl/ai-toolbox/releases">
    <img src="https://img.shields.io/github/v/release/coulsontl/ai-toolbox?style=flat-square" alt="Release">
  </a>
  <a href="https://github.com/coulsontl/ai-toolbox/blob/main/LICENSE">
    <img src="https://img.shields.io/github/license/coulsontl/ai-toolbox?style=flat-square" alt="License">
  </a>
  <a href="https://github.com/coulsontl/ai-toolbox/releases">
    <img src="https://img.shields.io/github/downloads/coulsontl/ai-toolbox/total?style=flat-square" alt="Downloads">
  </a>
</p>

---

## 简介

AI Toolbox Gateway Router 基于上游 AI Toolbox，是一个跨平台桌面应用，旨在帮助开发者高效管理各类 AI 编程助手的配置。支持 **Windows**、**macOS** 和 **Linux**。

### 主要功能

- **OpenCode 配置管理** - 可视化管理 OpenCode 的供应商和模型配置，支持列表页快速启停
- **Oh My OpenAgent / Oh My OpenCode Slim 插件配置管理** - 可视化管理 Oh My OpenAgent 及 Oh My OpenCode Slim 插件的配置
- **Claude Code 配置管理** - 一键切换 Claude Code 的官方订阅/自定义 API 供应商配置，支持动态获取模型列表、全局 Prompt 和插件管理
- **Codex 配置管理** - 管理 OpenAI Codex CLI 的官方账号/自定义渠道、模型映射、全局 Prompt、插件和会话历史
- **Gemini CLI 配置管理** - 管理 Gemini CLI 的官方账号/自定义渠道、`.env` / `settings.json`、全局 Prompt 和用量信息
- **OpenClaw 配置管理** - 管理 OpenClaw 的模型、供应商、配置文件路径和会话记录
- **Pi 配置管理** - 管理 Pi CLI 的模型、供应商、扩展、Prompt 和运行时配置
- **本机代理网关** - 提供统一本机代理入口，支持 Claude Code / Codex / Gemini CLI 接管、协议转换、故障切换、请求日志、用量统计和模型价格管理
- **Image 工作台** - 管理图片生成/编辑渠道，创建图片任务，保存历史记录和生成资产
- **MCP 服务器管理** - 集中管理 MCP（Model Context Protocol）服务器配置，支持导入/导出、收藏、分组和多工具同步
- **Skills 技能管理** - 管理 Skills 中央仓库，支持从 Git 仓库/本地目录安装，按工具启停同步，自定义工具和分组管理
- **会话管理** - 浏览、搜索、重命名、导入、导出和删除 OpenCode / Claude Code / Codex / Gemini CLI / OpenClaw / Pi 会话
- **WSL 同步** - 将 Windows 端的各种 CLI 配置、MCP 和 Skills 配置同步到 WSL 环境，支持自动同步和自定义映射
- **SSH 同步** - 将本机配置、MCP 和 Skills 同步到远程 SSH 主机，支持连接管理、路径映射和手动同步
- **供应商管理** - 统一管理多个 AI 供应商（OpenAI、Anthropic、自定义代理等）
- **系统托盘** - 通过系统托盘快速切换各模块的供应商、模型、Prompt、MCP 和 Skills 启用状态，无需打开主窗口
- **数据备份** - 支持本地备份、WebDAV 云端备份、GitHub/Gitee 私有仓库备份、自动备份、可选备份加密（密码保存在本机系统凭据库）、自定义备份项、图片资产备份和敏感文件过滤
- **主题切换** - 支持亮色/暗色/跟随系统主题
- **多语言** - 支持中文和英文界面
- **自动更新检查** - 启动时自动检查新版本

### Codex 聚合模式

在 Codex 页面进入网关模式后，可直接点击供应商列表旁的 **「聚合模式」** 打开配置子面板，无需跳转到网关设置页。子面板支持：

- 选择参与聚合的站点，并调整站点优先顺序
- 为站点设置自定义名称；未填写时沿用配置中的供应商名称
- 配置模型名命名方式和站点与上游模型名之间的分隔符
- 启用严格分组，为不同模型组配置独立的站点顺序
- 直接启用或关闭聚合接管，修改后自动重新接管 Codex

模型名示例：

```text
思辰888.model
```

分隔符不能包含字母、数字、空白、下划线或连字符，以便可靠地区分站点名称和上游模型名。旧版基于站点 ID 的模型名仍保留兼容解析。

## 截图

<p align="center">
  <img src="docs/screenshots/app_screenshot_opencode_model.jpg" alt="OpenCode 配置管理" width="80%">
  <img src="docs/screenshots/app_screenshot_opencode_provider.jpg" alt="OpenCode 供应商管理" width="80%">
  <img src="docs/screenshots/app_screenshot_opencode_tray.jpg" alt="系统托盘快速切换配置" width="30%">
  <br>
  <em>OpenCode 和 Oh My OpenAgent / Oh My OpenCode Slim 插件配置管理</em>
</p>

<p align="center">
  <img src="docs/screenshots/app_screenshot_claudecode.jpg" alt="Claude Code 配置管理" width="80%">
  <img src="docs/screenshots/app_screenshot_codex.jpg" alt="Codex 配置管理" width="80%">
  <br>
  <em>Claude Code / Codex 配置管理</em>
</p>

<p align="center">
  <img src="docs/screenshots/app_screenshot_mcp.jpg" alt="MCP 服务器管理" width="80%">
  <img src="docs/screenshots/app_screenshot_skills.jpg" alt="Skills 技能管理" width="80%">
  <br>
  <em>MCP 服务器管理 / Skills 技能管理</em>
</p>

<p align="center">
  <img src="docs/screenshots/app_screenshot_settings.jpg" alt="设置页面" width="80%">
  <img src="docs/screenshots/app_screenshot_wsl.jpg" alt="WSL 同步" width="80%">
  <br>
  <em>设置页面 / WSL 同步</em>
</p>

## 下载安装

前往 [Releases](https://github.com/coulsontl/ai-toolbox/releases) 页面下载适合您系统的安装包：

| 系统 | 安装包 |
|------|--------|
| Windows | `.msi` / `.exe` |
| macOS | `.dmg` |
| Linux | `.deb` / `.AppImage` |

macOS 也可以通过 Homebrew 安装、升级和卸载：

```bash
brew tap coulsontl/ai-toolbox https://github.com/coulsontl/ai-toolbox
brew install --cask coulsontl/ai-toolbox/ai-toolbox
sudo xattr -rd com.apple.quarantine /Applications/AI\ Toolbox.app

brew upgrade --cask coulsontl/ai-toolbox/ai-toolbox
brew uninstall --cask coulsontl/ai-toolbox/ai-toolbox
# 可选：不再需要此 tap 时移除
brew untap coulsontl/ai-toolbox
```

说明：

- 当前 Cask 暂时直接托管在本仓库，因此首次需要使用带仓库 URL 的 `brew tap`。
- 后续发布新版本后，仓库中的 `Casks/ai-toolbox.rb` 会由 release workflow 自动更新，`brew upgrade` 即可获取新版本。

Windows 也可以通过 [Scoop](https://scoop.sh/) 安装、升级和卸载：

```bash
scoop bucket add ai-toolbox https://github.com/coulsontl/ai-toolbox
scoop install ai-toolbox/ai-toolbox

# 升级：先刷新 bucket 拿到新 manifest，再升级应用
scoop update
scoop update ai-toolbox
scoop uninstall ai-toolbox
# 可选：不再需要此 bucket 时移除
scoop bucket rm ai-toolbox
```

说明：

- 当前 bucket 暂时直接托管在本仓库，因此首次需要使用带仓库 URL 的 `scoop bucket add`。
- 后续发布新版本后，仓库中的 `bucket/ai-toolbox.json` 会由 release workflow 自动更新，`scoop update ai-toolbox` 即可获取新版本。
- 通过 Scoop 安装时，应用内自动更新不可用，请使用 `scoop update ai-toolbox` 升级。

## 技术栈

| 类别 | 技术 |
|------|------|
| **桌面框架** | Tauri 2.x |
| **前端** | React 19 + TypeScript 5 |
| **UI 组件库** | Ant Design 6 |
| **状态管理** | Zustand |
| **国际化** | i18next (中文/英文) |
| **数据库** | SQLite JSONB（SurrealDB 仅用于旧版本一次性导入） |
| **构建工具** | Vite 7 |
| **包管理器** | pnpm |

## 项目结构

```
ai-toolbox/
├── web/                          # 前端源码
│   ├── app/                      # 应用层（App、路由、Provider）
│   ├── components/               # 通用组件
│   │   └── layout/               # 布局组件（MainLayout）
│   ├── features/                 # 功能模块（按业务划分）
│   │   ├── daily/                # 【日常】模块
│   │   │   └── notes/            # 笔记功能（Markdown）
│   │   ├── coding/               # 【编码】模块
│   │   │   ├── opencode/         # OpenCode 配置管理
│   │   │   ├── claudecode/       # Claude Code 配置管理
│   │   │   ├── codex/            # Codex 配置管理
│   │   │   ├── geminicli/        # Gemini CLI 配置管理
│   │   │   ├── openclaw/         # OpenClaw 配置管理
│   │   │   ├── pi/               # Pi 配置管理
│   │   │   ├── mcp/              # MCP 服务器管理
│   │   │   ├── skills/           # Skills 技能管理
│   │   │   ├── gateway/          # 本机代理网关
│   │   │   ├── image/            # Image 工作台
│   │   │   └── shared/           # 供应商、Prompt、会话等共享能力
│   │   └── settings/             # 【设置】模块
│   ├── stores/                   # 全局状态（Zustand）
│   ├── services/                 # API 服务层
│   ├── i18n/                     # 国际化配置
│   ├── constants/                # 常量（模块配置）
│   ├── hooks/                    # 全局 Hooks
│   ├── types/                    # 全局类型定义
│   └── utils/                    # 工具函数
├── tauri/                        # Tauri 后端 (Rust)
│   ├── src/
│   │   ├── main.rs               # 入口
│   │   ├── lib.rs                # 库入口、命令注册
│   │   └── coding/               # 编码模块
│   │       ├── claude_code/      # Claude Code 后端
│   │       ├── codex/            # Codex 后端
│   │       ├── open_code/        # OpenCode 后端
│   │       ├── gemini_cli/       # Gemini CLI 后端
│   │       ├── open_claw/        # OpenClaw 后端
│   │       ├── pi/               # Pi 后端
│   │       ├── oh_my_openagent/  # Oh My OpenAgent 后端
│   │       ├── oh_my_opencode_slim/ # Oh My OpenCode Slim 后端
│   │       ├── mcp/              # MCP 服务器后端
│   │       ├── skills/           # Skills 技能后端
│   │       ├── proxy_gateway/    # 本机代理网关后端
│   │       ├── image/            # 图片任务和资产后端
│   │       ├── session_manager/  # 会话管理后端
│   │       ├── ssh/              # SSH 同步后端
│   │       └── wsl/              # WSL 同步后端
│   ├── Cargo.toml                # Rust 依赖
│   └── tauri.conf.json           # Tauri 配置
├── package.json                  # 前端依赖
├── vite.config.ts                # Vite 配置
└── tsconfig.json                 # TypeScript 配置
```

## 开发指南

### 前置要求

- Node.js 18+
- pnpm 9+
- Rust 1.86+
- 参考 [Tauri 前置要求](https://tauri.app/start/prerequisites/)

### 安装依赖

```bash
pnpm install
```

### 启动开发服务器

```bash
pnpm tauri dev
```

### 构建生产版本

```bash
pnpm tauri build
```

### 代码检查

```bash
# TypeScript 类型检查
pnpm tsc --noEmit

# Rust 代码检查
cd tauri && cargo check
```

## 功能模块

| 模块 | 子模块 | 状态 | 描述 |
|------|--------|------|------|
| 编码 | OpenCode | ✅ 完成 | OpenCode 供应商/模型配置管理 |
| 编码 | Oh My OpenAgent | ✅ 完成 | Oh My OpenAgent 插件配置管理 |
| 编码 | Oh My OpenCode Slim | ✅ 完成 | Oh My OpenCode Slim 插件配置管理 |
| 编码 | Claude Code | ✅ 完成 | Claude Code 官方订阅/自定义 API 配置切换，支持 Prompt、插件、会话管理 |
| 编码 | Codex | ✅ 完成 | OpenAI Codex CLI 官方账号/自定义渠道管理，支持模型映射、Prompt、插件、会话管理和多站点聚合路由 |
| 编码 | Gemini CLI | ✅ 完成 | Gemini CLI 官方账号/自定义渠道、Prompt、用量和会话管理 |
| 编码 | OpenClaw | ✅ 完成 | OpenClaw 模型、供应商、配置文件和会话管理 |
| 编码 | Pi | ✅ 完成 | Pi CLI 模型、供应商、扩展、Prompt 和会话管理 |
| 编码 | Gateway | ✅ 完成 | 本机代理网关、CLI 接管、协议转换、故障切换、Codex 聚合路由、请求日志和用量统计 |
| 编码 | Image | ✅ 完成 | 图片生成/编辑渠道、任务历史和资产管理 |
| 编码 | MCP 服务器 | ✅ 完成 | MCP 服务器配置管理，支持导入/导出、收藏、分组和工具同步 |
| 编码 | Skills 技能 | ✅ 完成 | Skills 中央仓库管理，支持 Git/本地安装、自定义工具和多工具同步 |
| 编码 | 会话管理 | ✅ 完成 | 多工具会话浏览、搜索、重命名、导入、导出和删除 |
| 设置 | WSL 同步 | ✅ 完成 | CLI、MCP 和 Skills 配置同步到 WSL 环境 |
| 设置 | SSH 同步 | ✅ 完成 | CLI、MCP 和 Skills 配置同步到远程 SSH 主机 |
| 设置 | 通用设置 | ✅ 完成 | 语言切换、主题切换、启动项、代理、可见模块和版本更新检查 |
| 设置 | 备份设置 | ✅ 完成 | 本地/WebDAV 备份恢复、自动备份、自定义备份项和文件过滤 |
| 设置 | S3 设置 | ✅ 完成 | S3 兼容存储配置 |
| 设置 | 供应商设置 | ✅ 完成 | AI 供应商统一管理 |
| 日常 | 笔记 | 🚧 开发中 | Markdown 笔记管理、搜索 |

## 数据存储

使用 SQLite 作为本地主数据库，并通过 JSONB 表保存各模块配置。旧版本 SurrealDB 数据会在启动时执行一次性导入，导入完成后主数据以 SQLite 为准。

### 设计原则

- **本地优先**：所有数据存储在本地，保护隐私
- **服务层 API**：前端通过服务层与后端交互，不直接使用 localStorage
- **灵活备份**：支持本地 ZIP、WebDAV 云端备份、自动备份、自定义备份项和外部配置文件备份

### 数据表

| 表名 | 描述 |
|------|------|
| `settings` | 应用设置 |
| `app_migration` | 应用内部迁移记录 |
| `opencode_common_config` | OpenCode 通用配置 |
| `opencode_prompt_config` | OpenCode Prompt 配置 |
| `opencode_favorite_provider` | OpenCode 收藏供应商 |
| `opencode_favorite_plugin` | OpenCode 收藏插件 |
| `claude_provider` | Claude Code 供应商配置 |
| `claude_common_config` | Claude Code 通用配置 |
| `claude_prompt_config` | Claude Code Prompt 配置 |
| `codex_provider` | Codex 供应商配置 |
| `codex_common_config` | Codex 通用配置 |
| `codex_prompt_config` | Codex Prompt 配置 |
| `codex_official_account` | Codex 官方账号配置 |
| `codex_plugin_workspace_roots` | Codex 插件工作区根目录 |
| `gemini_cli_provider` | Gemini CLI 供应商配置 |
| `gemini_cli_common_config` | Gemini CLI 通用配置 |
| `gemini_cli_prompt_config` | Gemini CLI Prompt 配置 |
| `gemini_cli_official_account` | Gemini CLI 官方账号配置 |
| `pi_settings_config` | Pi 设置配置 |
| `pi_prompt_config` | Pi Prompt 配置 |
| `openclaw_common_config` | OpenClaw 通用配置 |
| `oh_my_openagent_config` | Oh My OpenAgent 配置 |
| `oh_my_openagent_global_config` | Oh My OpenAgent 全局配置 |
| `oh_my_opencode_slim_config` | Oh My OpenCode Slim 配置 |
| `oh_my_opencode_slim_global_config` | Oh My OpenCode Slim 全局配置 |
| `mcp_server` | MCP 服务器配置 |
| `mcp_preferences` | MCP 服务器偏好配置 |
| `favorite_mcp` | MCP 收藏配置 |
| `skill` | Skills 技能记录 |
| `skill_group` | Skills 分组 |
| `skill_repo` | Skills Git 仓库来源 |
| `skill_preferences` | Skills 技能偏好配置 |
| `skill_settings` | Skills 设置 |
| `custom_tool` | Skills/MCP 自定义工具配置 |
| `wsl_sync_config` | WSL 同步配置 |
| `wsl_file_mapping` | WSL 文件映射 |
| `ssh_sync_config` | SSH 同步配置 |
| `ssh_connection` | SSH 连接配置 |
| `ssh_file_mapping` | SSH 文件映射 |
| `proxy_gateway_settings` | Gateway 设置 |
| `image_channel` | 图片渠道配置 |
| `image_job` | 图片任务记录 |
| `image_asset` | 图片资产记录 |

## 贡献

欢迎提交 Issue 和 Pull Request！

1. Fork 本仓库
2. 创建特性分支 (`git checkout -b feature/amazing-feature`)
3. 提交更改 (`git commit -m 'Add some amazing feature'`)
4. 推送到分支 (`git push origin feature/amazing-feature`)
5. 提交 Pull Request

## 推荐 IDE 配置

- [VS Code](https://code.visualstudio.com/)
- [Tauri 插件](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode)
- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)

## License

[MIT](LICENSE)

## Acknowledgments

- [skills-hub](https://github.com/qufei1993/skills-hub)
- [cc-switch](https://github.com/farion1231/cc-switch)
- [linux.do](https://linux.do)
- [axonhub](https://github.com/looplj/axonhub)
