# StateRoot

[English](README.md) | [简体中文](README.zh-CN.md)

**运行环境可以换，工作不能丢。**

StateRoot 连接你已经在用的 AI 智能体，让它们跨会话共享知识、工具和工作中积累的经验。换一个智能体接着做，让一个制定计划、其他智能体并行执行，再合并成果，或恢复到之前保存的项目版本。

设置完成后，你通过与智能体对话来指挥工作。它们按照内置 Skill 使用 StateRoot CLI；你可以通过编辑器工作台查看进度和结果。

支持 Claude Code、Codex、Cursor、Kimi、CodeBuddy、OpenClaw、Hermes 等智能体。

*面向 AI 智能体的持久化联邦式元框架（meta-harness）。*

**共享智能，独立执行。**

<p align="center">
  <img alt="智能体在不同工具之间传递共享知识并接续项目工作" src="demo.gif">
</p>

<p align="center">
  <a href="https://open-vsx.org/extension/CognizTech/stateroot"><img src="https://img.shields.io/open-vsx/v/CognizTech/stateroot?color=7ee0c8&labelColor=0c1016&logo=cursor&style=for-the-badge&label=Cursor" alt="安装 Cursor 扩展"></a>
  <a href="https://marketplace.visualstudio.com/items?itemName=CognizTech.stateroot"><img src="https://img.shields.io/visual-studio-marketplace/v/CognizTech.stateroot?color=7ee0c8&labelColor=0c1016&logo=visualstudiocode&style=for-the-badge" alt="VS Code 扩展市场"></a>
  <a href="https://github.com/CognizTech/stateroot/releases"><img src="https://img.shields.io/github/v/release/CognizTech/stateroot?color=7ee0c8&labelColor=0c1016&logo=github&style=for-the-badge" alt="发布版本"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-Apache--2.0-7ee0c8?labelColor=0c1016&style=for-the-badge" alt="许可证"></a>
</p>
<p align="center">
  <a href="https://stateroot.dev"><img src="https://img.shields.io/badge/Website-stateroot.dev-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="官网"></a>
  <a href="https://stateroot.dev/docs/intro"><img src="https://img.shields.io/badge/Docs-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="文档"></a>
  <a href="https://stateroot.dev/docs/getting-started/quickstart"><img src="https://img.shields.io/badge/Quickstart-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="快速入门"></a>
  <a href="https://stateroot.dev/docs/reference/cli"><img src="https://img.shields.io/badge/CLI-7ee0c8?style=for-the-badge&labelColor=0c1016" alt="CLI 参考"></a>
  <a href="https://discord.gg/wrjgpnuky"><img src="https://img.shields.io/badge/Discord-Join%20community-5865F2?style=for-the-badge&logo=discord&logoColor=white&labelColor=0c1016" alt="加入 StateRoot Discord 社区"></a>
  <a href="https://qm.qq.com/q/ZFoqWSP9yo"><img src="https://img.shields.io/badge/QQ-Join%20group-12B7F5?style=for-the-badge&logo=qq&logoColor=white&labelColor=0c1016" alt="加入 StateRoot QQ 群"></a>
</p>

日常使用请从[快速入门](https://stateroot.dev/docs/getting-started/quickstart)和[通过智能体工作](https://stateroot.dev/docs/guides/daily-workflow)开始。[内置 Skill](stateroot-cli/assets/stateroot-skill/SKILL.md)提供智能体的操作指引。

## 为什么选择 StateRoot

更换工具、碰到用量上限，或隔一段时间回来继续项目，往往意味着重新解释决策和工作进度。StateRoot 保留有用的项目知识与保存的版本，让你按任务选择智能体，并把已有工作接着做下去。

你可以一直使用同一个智能体，也可以换一个继续，或让多个智能体同时工作。相关偏好、计划和经验会提供给接入的智能体；需要独立执行的任务则使用各自的项目副本。

## 你指挥工作，智能体操作 StateRoot。

像平时一样与智能体对话。例如：

> 把这个功能记录为共享 StateRoot 计划。我审核后，给 Kimi 和 Cursor 分配独立任务，各自在自己的 fork 上执行，最后审查并合并结果。

或者：

> 把这个项目偏好记录到 StateRoot，让我不用再向下一个智能体解释。

这些是普通请求，不是固定提示词语法。安装的 StateRoot Skill 会指导智能体保存进度、共享知识和操作项目历史。你通过对话或编辑器工作台审查工作。

从[通过智能体工作](https://stateroot.dev/docs/guides/daily-workflow)开始。设置和更新仍可直接使用 CLI；日常检查点和交接由智能体处理。

## 接续。委派。分叉。合并。

**接续（Continue）。** 在同一项目中打开另一个受支持的智能体，请它继续工作。StateRoot 提供已保留的目标、计划、决策和下一步，不需要你重述整段会话。[交接指南](https://stateroot.dev/docs/features/handoffs)。

**委派（Delegate）。** 请当前智能体把一个明确任务连同所需项目上下文交给另一个受支持的智能体，再带回结果，供当前智能体审查和使用。[委派指南](https://stateroot.dev/docs/features/delegation)。

**分叉（Fork）。** 为独立任务或不同方案建立各自的项目工作副本，称为 StateRoot fork。智能体共享项目知识，同时独立修改文件。完成的分叉可以暂停、释放工作树，需要时重建，保留的历史仍在。[分叉与保存的版本](https://stateroot.dev/docs/features/roots)。

**合并（Merge）。** 请负责协调的智能体审查贡献、解决冲突、执行相关检查，再把接受的工作合并进持续推进的项目，并保留各自的历史。[跨智能体工作](https://stateroot.dev/docs/guides/versioning-across-agents)。

```text
                     一个项目，共享知识
                             |
              +--------------+--------------+
              |              |              |
            Codex          Kimi          Cursor
              |              |              |
            分叉 A         分叉 B         分叉 C
              |              |              |
              +--------------+--------------+
                             |
                        审查并合并结果
                             |
                          继续项目
```

### 主动维持工作连续性

StateRoot 会在智能体会话和编辑器工作台中呈现到期事项、尚未处理的交接，以及需要补充完成证据的计划。你可以询问智能体有哪些事项需要关注、还剩什么工作。本地后台服务会跨会话保留和协调这些跟进事项，本身不运行 AI 智能体。

## 继续使用你已经在用的智能体

每个智能体保留自己的模型、界面和优势。让一个负责规划，其他智能体实施，再由协调者审查结果。OpenClaw、Hermes 等通用智能体可以与 Claude Code、Codex、Cursor、Kimi、CodeBuddy 和 Pi 一起参与。

### 哪些信息可以跨工具流转

共享项目知识、有用的偏好和保存的工作状态会在接入的智能体之间延续。智能体可以接着执行任务，也可以在自己的 fork 中独立工作，同时使用相关共享上下文。

## 共享哪些内容

| 保留下来的内容 | 你可以做什么 |
| --- | --- |
| **个性与偏好** | 保留称呼、沟通方式和你希望采用的工作方法。 |
| **项目知识** | 为下一项任务保留目标、决策、事实、规则和之前尝试过的方案。 |
| **计划与依赖** | 在一个智能体中规划，在另一个中执行，跟踪前置条件、负责人、进度和完成证据。 |
| **技能与工具** | 复用完整 Skill 包与兼容的 MCP 配置；通过委派使用某个智能体独有的原生能力。 |
| **经验准则** | 把有用的纠正记录为共享指导，带入后续智能体会话。 |
| **原生会话** | 将受支持的会话迁移到 Pi 或 DeepSeek Harness；在规定范围内恢复已保留的 Codex 会话。 |
| **可移植项目状态** | 导出并检查包含共享知识和证据的根状态，再在别处导入或应用兼容的项目状态。 |
| **可扩展命令** | 项目需要额外操作时，由智能体编写可执行 CLI 扩展。 |

详情：[技能](https://stateroot.dev/docs/features/skills)、[MCP 工具](https://stateroot.dev/docs/features/mcp)、[规则](https://stateroot.dev/docs/features/rules)、[会话](https://stateroot.dev/docs/features/sessions)、[便携交换](https://stateroot.dev/docs/guides/sharing-a-project#portable-local-exchange)和 [CLI 扩展](https://stateroot.dev/docs/features/extensions)。

### 接入的智能体共享同一种工作个性

共享 persona 与用户档案记录你希望如何与智能体合作，包括名字、表达方式、偏好和工作关系。StateRoot 通过受支持的会话集成完整提供 persona 与 USER.md。智能体保留各自模型和优势，同时继承共享身份。[个性说明](https://stateroot.dev/docs/features/personality)。

### 三层记忆

- **当前事实：** 为下一次会话整理的简明核心信息。
- **长期项目知识：** 根据保留的证据建立 wiki，可选择启用模型辅助整理。
- **工作历史：** 可检索的检查点与观察记录，帮助智能体找回早先的决策和上下文。

记录会区分已验证的变化、已观察的活动与 AI 生成的总结。[记忆](https://stateroot.dev/docs/features/memory)与[项目 wiki](https://stateroot.dev/docs/features/wiki)。

### 教会一个智能体，把经验带到下一次工作。

例如，告诉 Cursor 业务逻辑应放在路由处理函数之外。StateRoot 会保存这个项目偏好，并将其带入 Codex、Kimi 等接入智能体的后续会话。

经验准则是关于如何工作的指导，可作用于当前项目、你的一般工作、工作区或某个领域。记录后即生效，更新时保留历史。事实与个性有各自的共享记录。[经验准则](https://stateroot.dev/docs/features/learnings)。

## 智能体工作时，保留可以回去的版本

StateRoot 随着工作推进保存项目版本。你可以比较变化、在独立副本中尝试另一种方案，或在出错时恢复到之前保存的版本。

快照使用 Git，但不重写你的常规分支。恢复会生成一个新的根状态，中间的工作仍留在历史中。恢复历史状态时，仍然遵循当前的文件排除规则。

请智能体说明改了什么、验证了什么、还剩什么。编辑器的 Lineage 视图可查看保留的历史和比较结果。[快照与恢复](https://stateroot.dev/docs/features/roots)解释覆盖范围与限制；[首次连续性验证](https://stateroot.dev/docs/guides/first-continuity-proof)提供可亲自检查的操作流程。

## 安装

安装适用于 [**Cursor**](https://open-vsx.org/extension/CognizTech/stateroot) 或 [**VS Code**](https://marketplace.visualstudio.com/items?itemName=CognizTech.stateroot) 的 StateRoot 扩展，即可获得引导式设置，并在支持的平台上自动安装 CLI。

CLI 提供 **Linux x64 和 ARM64**、**Windows x64**、**macOS Apple Silicon** 版本。一个二进制文件，无需额外运行时。

### Linux

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

默认安装到 `~/.local/bin`，请将该目录加入 `PATH`。二进制文件需要 glibc 2.17 或更新版本，支持 Ubuntu 16.04、Debian 9、RHEL 7 及之后的发行版。

### Windows

从 [Releases](https://github.com/CognizTech/stateroot/releases) 下载 [**StateRootSetup-x64.msi**](https://github.com/CognizTech/stateroot/releases/latest/download/StateRootSetup-x64.msi)，或运行：

```powershell
irm https://github.com/CognizTech/stateroot/releases/latest/download/install.ps1 | iex
```

`stateroot-windows-x64.exe` 是便携版 CLI，不是安装程序。

### macOS（Apple Silicon）

```bash
curl -sSfL https://github.com/CognizTech/stateroot/releases/latest/download/install.sh | sh
```

默认安装到 `~/.local/bin`。Intel Mac 可以[从源码构建](https://stateroot.dev/docs/getting-started/installation#from-source)，也可以直接克隆本仓库，再执行 `cargo build --release -p stateroot-cli`。需要 Rust 1.85+，libgit2 已随源码提供；生成的二进制文件位于 `target/release/stateroot`。

```bash
stateroot --version
stateroot doctor     # 检查本地设置
```

## 快速入门

初始化项目并完成一次机器设置。核心连续性功能不需要额外的模型 API 密钥。

```bash
cd my-project
stateroot init
stateroot setup      # once per machine: identity, harnesses, skills
```

在项目中打开你常用的智能体，直接描述想完成的工作。StateRoot 安装的内置 Skill 会指导智能体保存进度并使用共享项目状态；受支持的会话钩子会自动提供接续摘要。

第一次使用时，可以这样说：

> 在这个项目中使用 StateRoot。先告诉我当前目标和下一步，再帮我完成任务。

切换工具前，对当前智能体说：

> 准备一份 StateRoot 交接，记录改了什么、验证了什么、还剩什么。我接下来会在 Codex 中继续。

然后在同一项目中打开另一个受支持的智能体：

> 从最新的 StateRoot 交接继续。开始之前，先说明当前进展。

你负责指挥工作，智能体负责检查点、共享计划和交接。如果新智能体没有获得上下文，请让它加载 StateRoot Skill 并诊断集成。钩子支持取决于运行环境和使用界面；需要手动恢复时，Skill 会提供操作流程。

### 通过智能体使用 StateRoot

| 你的目标 | 可以对智能体说 |
| --- | --- |
| 在这里规划，在别处实施 | “把任务记录为共享 StateRoot 计划。我审核后，再由另一个智能体实施。” |
| 委派一个明确任务 | “通过 StateRoot 把这项审查委派给 Codex，带回审查结果。” |
| 并行推进工作 | “给这些独立任务各建一个 StateRoot fork，并跟踪每个智能体的进度。” |
| 汇总结果 | “审查完成的 fork、解决冲突、执行相关检查，再合并接受的工作。” |
| 恢复到之前的状态 | “先展示上一个良好根状态之后的变化，再恢复到那个状态。” |
| 保留纠正意见 | “把这个偏好记录到 StateRoot，让其他智能体也遵循。” |

这些是自然语言示例，不是固定提示词语法。跨运行环境执行需要目标智能体支持的 CLI 及可用账户。智能体应报告实际的交接、计划、fork 或验证结果，并明确说明阻塞和未完成事项。

[用户指南](https://stateroot.dev/docs/guides/daily-workflow)介绍具体流程与结果判断方法。智能体遵循[内置 Skill](stateroot-cli/assets/stateroot-skill/SKILL.md)。

### 自己查看进展

编辑器工作台可以查看计划、智能体、共享知识和状态谱系。如果偏好终端，也可以手动检查：

```bash
stateroot status     # 当前工作及需要关注的事项
stateroot log        # 保留的项目历史
stateroot doctor     # 安装健康状态及恢复诊断
```

从[快速入门](https://stateroot.dev/docs/getting-started/quickstart)开始。完整 [CLI 参考](https://stateroot.dev/docs/reference/cli)供智能体、集成开发和直接操作使用。

### 添加或刷新编程智能体集成

当 StateRoot 新增对某个编程智能体的支持，或你之后安装了已受支持的智能体时，请先安装该智能体的 CLI，再更新 StateRoot，并重新运行设置中的运行环境部分：

```bash
stateroot self-update
stateroot setup --only harnesses
stateroot doctor
```

如果设置提示运行环境已配置，请选择 **Yes** 重新配置。保留你正在使用的智能体选项，勾选新检测到的智能体，然后确认。这样可以刷新所选集成，无需重新设置身份或重新初始化项目。完成后，重启编程智能体以加载更新后的配置。

对于下方标注为**仅限 nightly** 的集成，请用以下命令替换上面的第一条更新命令，再执行相同的 setup 和 doctor 命令：

```bash
stateroot self-update --tag nightly
```

Nightly 构建可从[滚动预览版](https://github.com/CognizTech/stateroot/releases/tag/nightly)获取；标注为仅限 nightly 的集成尚未包含在最新稳定版中。若要跳过设置向导，直接刷新检测到的集成，可运行 `stateroot install`。

## 支持的运行环境

**钩子 + 会话记录：** Claude Code · Codex · Cursor · Kimi Code · CodeBuddy · OpenClaw · Hermes · Pi

**会话记录 / 委派：** DeepSeek Harness · OpenCode · 安装时检测到的其他运行环境

各运行环境的说明：[Harnesses](https://stateroot.dev/docs/harnesses/overview)。

## 文档

用户文档位于 **[stateroot.dev](https://stateroot.dev)**，本仓库提供 CLI。

| 章节 | 内容 |
| --- | --- |
| [安装](https://stateroot.dev/docs/getting-started/installation) | Cursor、VS Code、Linux、Windows、macOS、源码构建 |
| [快速入门](https://stateroot.dev/docs/getting-started/quickstart) | 设置 → 首个任务 → 换一个智能体继续 |
| [用户指南](https://stateroot.dev/docs/guides/daily-workflow) | 指挥智能体、审查结果、恢复和接续工作 |
| [概念](https://stateroot.dev/docs/concepts/overview) | 根状态、连续性、身份、记忆、来源 |
| [能力详情](https://stateroot.dev/docs/features/roots) | 技术行为、限制与命令示例 |
| [运行环境](https://stateroot.dev/docs/harnesses/overview) | 各工具的安装方式与协议 |
| [智能体 Skill](stateroot-cli/assets/stateroot-skill/SKILL.md) | 安装给智能体的操作指引 |
| [CLI 参考](https://stateroot.dev/docs/reference/cli) | 智能体及直接操作所需的完整命令 |
| [MCP 工具](https://stateroot.dev/docs/reference/mcp-tools) | 本地 stdio 服务器 |

机器可读索引：[stateroot.dev/llms.txt](https://stateroot.dev/llms.txt)。

## 隐私

项目数据保存在仓库内（`.stateroot/`）。Persona 和 USER.md 位于 `~/.stateroot/`。搜索数据保存在 `.stateroot/local/`，不会进入快照。

快照遵循项目根目录的 `.gitignore` 和 `.staterootignore`，并排除 `.git/` 和 `local/`。可选的 LLM 综合整理仅在配置 `DEEPSEEK_API_KEY`（优先使用，模型为 `deepseek-v4-flash`）或 `OPENAI_API_KEY`（模型为 `gpt-5.6-luna`）时运行。

详情：[隐私](https://stateroot.dev/docs/guides/privacy)。

## 共享项目

将 `.stateroot/` 提交到仓库后，项目的权威状态记录就能成为团队共享的资产。共享边界经过明确划分，便于合并：

- **随仓库共享：** 目标与项目状态、计划、经验准则、规则、技能池、wiki 与记忆页面、交接历史、项目级 soul 覆盖层和状态转换记录。你的 persona 和 USER.md 保存在 `~/.stateroot/`，不会随仓库共享，因此每位成员都能保有自己智能体的个性。
- **仅保留在本地**（初始化时写入 `.stateroot/.gitignore`）：搜索索引、捕获暂存区（spool）、委派记录、同步游标、核心记忆 `memories/MEMORY.md` 与 `memories/episodic.jsonl`（个人视角与私有日志；共享知识保存在 wiki 和经验准则中）、当前交接（用于当前会话连续性；历史会共享），以及 `roots/`。如果团队成员需要保留的状态谱系，请让智能体单独传递。
- 追加式日志通过 `.stateroot/.gitattributes` 按并集合并。如果某个应仅保留在本地的路径被 Git 跟踪，`stateroot doctor` 会发出提醒。

团队成员的使用流程：克隆共享项目、完成机器设置，再在项目中打开受支持的智能体。请它先检查共享目标、计划和经验准则，再继续工作。[共享项目指南](https://stateroot.dev/docs/guides/sharing-a-project)介绍仓库共享与便携交换方式。

## 参与贡献

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

需要 Rust 1.85+，libgit2 已随源码提供。CLI 内置的会话技能位于 [`stateroot-cli/assets/stateroot-skill/`](stateroot-cli/assets/stateroot-skill/)，面向技能市场的安装技能位于 [`skills/stateroot/`](skills/stateroot/)。文档：[参与贡献](https://stateroot.dev/docs/developer-guide/contributing)。

请保持[产品意图](https://stateroot.dev/docs/features/rules)：完整注入 persona / USER.md；交接内容过于简略时提示而不拒绝；不自动给经验准则分类；不为了节省 token 而裁剪身份信息。

## 许可证

Apache-2.0，详见 [LICENSE](LICENSE)。
