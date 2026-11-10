# StateRoot

[English](README.md) | [简体中文](README.zh-CN.md)

**面向 AI 智能体的持久化联邦式元框架（meta-harness）。**

**运行环境可以换，工作不能丢。**

StateRoot 通过共享的工作知识与版本化项目状态，将 Claude Code、Codex、Cursor、Kimi、CodeBuddy、OpenClaw、Hermes、Pi、DeepSeek Harness 等智能体运行环境（harness）联结起来。你可以换一个运行环境继续工作，把计划交给另一个智能体，跨工具委派范围明确的任务，让多个智能体在独立的 StateRoot 分叉中并行工作，再在工作汇合时合并成果。

记忆、个性、规则、技能、工具、计划，以及来之不易的经验准则，都能在使用它们的运行环境之间持续保留。每个运行环境仍保有自己的运行时、模型与优势。

**共享智能，独立执行。**

<p align="center">
  <img alt="关掉一个智能体，打开另一个，它已经知道项目的来龙去脉" src="demo.gif">
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

## 为什么选择 StateRoot

每个模型都有适合自己的运行环境：Claude 擅长在 Claude Code 中工作，GPT 在 Codex 中发挥优势，DeepSeek 也有自己的选择。但每个运行环境都各自保存上下文，包括会话记录、规则、技能和计划。因此，现代 AI 工作中的常见情况，比如任务做到一半碰到用量上限、另一款工具推出了更好的模型，或者只想让昂贵模型负责规划，都会带来同一种隐性成本：重新解释项目、重新阅读代码、重新教智能体你的工作方式。

StateRoot 是位于这些运行环境之上的持久化联邦协作层：智能体通过它继承共享的工作知识，并在版本化项目状态上继续工作、分叉探索，再将成果汇合。与此同时，每个模型仍然使用自己的原生运行环境。只需一个本地 CLI，一切都在你的机器上。

它支持两种工作方式：

- **顺序接续**：关闭 Claude Code，打开 Codex，继续工作。下一个智能体从一开始就知道目标、计划、决策，以及你的工作方式；一个智能体学到的经验准则可以成为所有智能体共同遵循的规则。
- **并行协作**：多个运行环境同时在独立的 StateRoot 分叉上工作，共享知识，独立执行。工作汇合时，`stateroot merge` 将成果合并回同一个持续演进的项目。

记忆工具保留智能体*知道什么*。StateRoot 还保留*工作本身处于什么状态*：项目中每一个有意义的状态都不可变、可恢复，并具有可验证的变更谱系。

## 接续。委派。分叉。合并。

**接续（Continue）。** 一个运行环境停止，另一个从累积的状态继续。会话钩子在启动时注入有界摘要，包含目标、计划、决策、记忆与下一步操作，无需粘贴会话记录。计划同样可以跨工具流转：强模型在自己的计划模式中制定计划，成本较低的模型负责执行，而执行者收到的摘要会明确指出：*“执行此计划，不要重新规划。”* 会话本身也会归入统一的规范存储，并能迁移到 Pi / DeepSeek Harness，成为可真正恢复的原生会话。

```text
Claude Code ──→ 状态 A ──→ 状态 B
                             │
                             ├── 用 Codex 继续
                             ├── 用 Cursor 分叉
                             └── 恢复到更早的状态
```

**委派（Delegate）。** `stateroot delegate --to codex --task "…"` 在另一个运行环境的 CLI 中执行范围明确的任务，携带完整项目上下文，限制委派深度，并记录状态谱系。父智能体收到结论，无需接收整个会话记录。

**分叉（Fork）。** `stateroot fork` 在 `refs/stateroot/forks/` 下创建内部状态引用；加上 `--worktree`，即可在受管理的本地区域创建独立工作树。多个智能体可以在同一项目的不同状态上并行工作，共享智能、独立执行。已保留的工作可以暂停并释放工作树，需要时重建同一分叉。你的 Git 分支不会被改动。

**合并（Merge）。** `stateroot merge` 将各分叉的状态谱系合并回主线：每个分叉通过三方合并纳入累积结果，最后生成一个根状态，以主线和所有分叉的末端状态作为父节点。冲突按路径报告；已包含在现有结果中的分叉会报告无需合并。工作汇合后，其状态谱系仍然完整。

```text
                    一个项目，三个分叉
                            │
             ┌──────────────┼──────────────┐
             │              │              │
           Codex          Kimi          Cursor
             │              │              │
          分叉 A         分叉 B         分叉 C
             │              │              │
             └──────────────┼──────────────┘
                            │
                     stateroot merge
                            │
                    同一个项目继续演进
```

切换运行环境、更换模型、淘汰旧工具，项目、个性，以及累积的每一条经验都能延续。

### 主动维持工作连续性

智能体启动时，StateRoot 会在 `## Needs Attention` 中提示到期工作和尚未处理的交接；同样的信息也会出现在 `stateroot status` 和编辑器扩展中。持久化的 `stateroot obligation` 待办事项可以跨运行环境延续，用户级 `stateroot service` 则在后台协调这些事项。本地连续性机制按确定性规则运行，无需内置 AI 智能体。

计划需要通过显式执行 `stateroot plan done <id> --evidence "…"` 并提供完成凭据来关闭。即使计划关联的待办事项已经完成，StateRoot 仍会持续显示计划收尾提醒，直到智能体记录结果。[CLI 指南](docs/cli.md)介绍了持久待办、服务控制和计划生命周期。

## 原生运行环境保持原生

StateRoot 保留 Claude Code、Codex、Cursor、Kimi、CodeBuddy 等工具原有的运行时。每个运行环境继续使用自己的模型、界面、能力和优势；StateRoot 提供持久化的共享智能与工作状态层。你可以按任务选择智能体：Codex 制定计划，Kimi 实现一项任务，Cursor 在自己的分叉中实现另一项任务，最后由 Codex 审查并整合。智能体各自独立，项目上下文则在执行者更换时继续保留。

OpenClaw、Hermes 等通用智能体运行环境，也可以与 Claude Code、Codex、Cursor、Kimi、CodeBuddy 和 Pi 一起接入同一个持久化协作层。

### 哪些信息可以跨工具流转

一个项目，多个运行环境，无需新的运行时、强制云服务或工具绑定：

| 随工作流转的内容 | 实现方式 |
| --- | --- |
| 目标、状态与下一步操作 | 每个运行环境读取同一份权威状态记录 |
| 带审批生命周期的计划 | `stateroot plan` |
| 记忆与事实 | 经整理、标注来源、可搜索 |
| 规则与偏好 | 共享规则池，记录一次，各处可用 |
| 可真正恢复的会话 | 跨运行环境的规范会话存储与迁移 |
| 其他运行环境中的子智能体 | `stateroot delegate` |
| 状态谱系：分叉 / 合并 / 恢复 | 使用 Git 底层机制，不改动你的分支 |
| 个性 | 完整注入 persona + USER.md，不裁剪 |

## 共享哪些内容

| 内容 | 说明 |
| --- | --- |
| **项目状态** | 目标、阶段、交接、下一步操作，每个运行环境读取同一处记录。 |
| **计划** | 带生命周期（草案 → 已批准 → 执行中 → 已完成）和来源记录的计划存储。 |
| **技能与工具** | 在智能体配置之间同步 SKILL.md 技能包与 MCP 服务器；遇到冲突时保留原状。 |
| **会话** | 保留完整信息的跨运行环境规范会话存储；可迁移到 Pi / DeepSeek Harness。 |
| **子智能体** | 将范围明确的任务委派给其他运行环境的 CLI，限制委派深度、控制返回结果范围，并记录状态谱系。 |
| **可移植状态** | 将保留的根状态及共享项目智能与证据导出为离线 v2 包；验证后可导入而不移动当前状态，或在兼容的工作树中物化。 |
| **扩展** | `PATH` 中任何名为 `stateroot-<name>` 的可执行文件都会成为子命令，智能体也能扩展 CLI 本身。 |

### 所有智能体保持同一种工作个性

Soul + USER.md 记录智能体的名字、性格、表达方式与边界，以及你是谁、如何工作。这些内容会在每次会话启动时完整注入，不会为了适应 token 预算而截断。StateRoot 将相同的 persona、USER.md、沟通偏好和工作身份投射到每个运行环境。智能体仍然各自独立：Codex 仍是 Codex，Claude Code 仍是 Claude Code；你与智能体的工作关系则在工具更换后继续保留。你不必重新介绍自己。

### 三层记忆

- **常驻核心（Hot apex，`MEMORY.md`）**：每个会话都能看到的精选核心内容，包含项目当前事实、决策和重要上下文，可按项目或用户划分作用域。
- **编译知识库（wiki）**：从证据中逐步提炼的长期知识，包括页面、索引和日志。默认采用确定性编译，也可使用你自己的密钥启用可选的 LLM 综合整理。
- **情景日志与全文检索**：每次检查点与观察记录都以追加方式保存，可在本地搜索（`stateroot memory recall`），项目积累的知识可以通过查询重新找到。

每条事实都带有来源标记：**已验证（verified）**，来自 Git；**已观察（observed）**，来自会话记录；或**已综合（synthesized）**，来自 LLM。没有记录就保持为空。

### 经验准则：越用越成熟的判断

经验准则（Learnings）记录判断而非事实，例如“优先选择 X，而不是 Y”“绝不执行 Z”，每条都说明*何时适用*。你或任何受你委托的智能体记录一条准则后，它就会立即对所有运行环境生效，无需审批队列，也不经分类器处理。准则可作用于项目、用户、工作区或领域，之后可以被新准则取代，历史仍会保留。一个运行环境中的纠正，可以成为所有智能体共同遵循的规则，让整个智能体团队共同积累经验，避免在不同工具中重复犯错。

## 快照保存什么

Git 为你主动创建的提交做版本管理。StateRoot 则在智能体工作期间为工作树创建快照，使用 Git 底层机制存放在 `refs/stateroot` 下，不重写你的分支。

每个快照都是完整、不可变、按内容寻址的状态。你可以精确恢复、从中安全分叉、如实比较任意两个状态，并查看状态之间变更的凭据。

物化历史状态时，仍然遵循当前的 `.gitignore` 和 `.staterootignore` 排除规则。`stateroot doctor` 会分别报告当前根状态、元数据兼容性、捕获证据和恢复限制，以及一般安装健康状况。若需将一次命令执行记录关联到计划及符合条件的项目文件，可运行 `stateroot plan verify <id> -- <command> [args...]`；之后使用 `stateroot plan done <id> --evidence "…" --verification <observation-id>` 附上成功的执行记录。记录到命令执行成功，并不代表所有验收条件都已通过。具体范围见 [CLI 指南](docs/cli.md#optional-plan-command-observations-and-state-compatibility)。

恢复报告和摘要会注明信息来自哪里：**已验证（verified）**，来自 Git；**已观察（observed）**，来自会话记录；或**已综合（synthesized）**，来自 LLM。没有记录就保持为空。

完整文档：**[stateroot.dev/docs](https://stateroot.dev/docs/intro)**。

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
stateroot doctor     # passes with zero config and zero keys
```

## 快速入门

零配置、零密钥、一个二进制文件。`stateroot doctor` 开箱即可通过检查。

```bash
cd my-project
stateroot init
stateroot setup      # once per machine: identity, harnesses, skills
```

照常在你常用的智能体中工作，会话钩子会注入摘要，无需手动粘贴内容。

**接下来体验它的价值：** 关闭当前智能体，在同一项目中打开另一个受支持的运行环境。会话开始时，它已经知道目标、计划、决策和下一步操作，无需重新解释，也无需从头重读代码库。

```bash
stateroot status
stateroot log
stateroot resume --harness cursor
stateroot checkpoint --note "wired auth middleware — unblocks handlers" --files src/auth.rs
stateroot handoff write --from cursor \
  --objective "Ship local handoffs" \
  --task "Finish adversarial CLI tests" \
  --context-summary "Structured write path is in; remaining work is workspace checks."
```

如果没有出现摘要，请使用你实际所在的运行环境标识（`claude`、`codex`、`cursor`、`kimi`、`openclaw`、`hermes`）运行 `stateroot resume --harness <id>`。直接读取完整输出，不要接 `| head` / `| tail` 等截断命令。完整摘要才是权威状态记录。

操作指南：[快速入门](https://stateroot.dev/docs/getting-started/quickstart)。

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

**钩子 + 会话记录：** Claude Code · Codex · Cursor · Kimi Code · CodeBuddy· OpenClaw · Hermes · Pi

**会话记录 / 委派：** DeepSeek Harness · OpenCode · 安装时检测到的其他运行环境

各运行环境的说明：[Harnesses](https://stateroot.dev/docs/harnesses/overview)。

## 文档

用户文档位于 **[stateroot.dev](https://stateroot.dev)**，本仓库提供 CLI。

| 章节 | 内容 |
| --- | --- |
| [安装](https://stateroot.dev/docs/getting-started/installation) | Cursor、VS Code、Linux、Windows、macOS、源码构建 |
| [快速入门](https://stateroot.dev/docs/getting-started/quickstart) | 初始化 → 设置 → 首次恢复上下文 |
| [概念](https://stateroot.dev/docs/concepts/overview) | 根状态、连续性、身份、记忆、来源 |
| [能力](https://stateroot.dev/docs/features/roots) | 状态谱系、交接、记忆、wiki、技能、MCP、规则 |
| [运行环境](https://stateroot.dev/docs/harnesses/overview) | 各工具的安装方式与协议 |
| [CLI 参考](https://stateroot.dev/docs/reference/cli) | 所有命令 |
| [MCP 工具](https://stateroot.dev/docs/reference/mcp-tools) | 本地 stdio 服务器 |

机器可读索引：[stateroot.dev/llms.txt](https://stateroot.dev/llms.txt)。

## 隐私

项目数据保存在仓库内（`.stateroot/`）。Persona 和 USER.md 位于 `~/.stateroot/`。搜索数据保存在 `.stateroot/local/`，不会进入快照。

快照遵循项目根目录的 `.gitignore` 和 `.staterootignore`，并排除 `.git/` 和 `local/`。可选的 LLM 综合整理仅在配置 `DEEPSEEK_API_KEY`（优先使用，模型为 `deepseek-v4-flash`）或 `OPENAI_API_KEY`（模型为 `gpt-5.6-luna`）时运行。

详情：[隐私](https://stateroot.dev/docs/guides/privacy)。

## 共享项目

将 `.stateroot/` 提交到仓库后，项目的权威状态记录就能成为团队共享的资产。共享边界经过明确划分，便于合并：

- **随仓库共享：** 目标与项目状态、计划、经验准则、规则、技能池、wiki 与记忆页面、交接历史、项目级 soul 覆盖层和状态转换记录。你的 persona 和 USER.md 保存在 `~/.stateroot/`，不会随仓库共享，因此每位成员都能保有自己智能体的个性。
- **仅保留在本地**（初始化时写入 `.stateroot/.gitignore`）：搜索索引、捕获暂存区（spool）、委派记录、同步游标、核心记忆 `memories/MEMORY.md` 与 `memories/episodic.jsonl`（个人视角与私有日志；共享知识保存在 wiki 和经验准则中）、当前交接（用于当前会话连续性；历史会共享），以及 `roots/`。状态谱系通过 Git 本身传递：`git push origin 'refs/stateroot/*'`。
- 追加式日志通过 `.stateroot/.gitattributes` 按并集合并。如果某个应仅保留在本地的路径被 Git 跟踪，`stateroot doctor` 会发出提醒。

团队成员的使用流程：克隆仓库，运行一次 `stateroot install`，打开任意运行环境。摘要已经包含目标、计划和项目积累的经验。

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
