# 自建 Coding-Agent Harness 调研：Claude Code、OpenAI Codex、Hermes Agent、DeepSeek Harness（DSH）

> **范围与方法。** 本文只引用项目方的官方文档、官方 GitHub 仓库及仓库内发布/安全说明；网页与仓库会持续更新，结论应在选型/实施前复核链接。文中的“源码观察”是对所引官方源码/仓库结构作出的可验证工程判断，不冒充厂商承诺。
>
> **术语澄清：Hermes。** “Hermes”有多个同名项目/模型。本文所称 **Hermes Agent** 特指 Nous Research 官方开源仓库 [`NousResearch/hermes-agent`](https://github.com/NousResearch/hermes-agent)，而非 Hermes 模型系列或任何第三方同名 CLI；它是本组中可核实的开源 agent harness。

## 结论先行

- 若目标是**研究并改造 harness 本身**：优先读 **DSH**（插件/服务/事件/可逆装载为一等概念）与 **Codex CLI**（较紧凑的 Rust 本地编码代理）；二者源码分别为 MIT、Apache-2.0。[DSH README](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.md)｜[DSH LICENSE](https://github.com/deepseek-ai/deepseek-harness/blob/master/LICENSE)｜[Codex README](https://github.com/openai/codex/blob/main/README.md)｜[Codex LICENSE](https://github.com/openai/codex/blob/main/LICENSE)
- 若目标是**产品能力面很广、常驻多渠道代理**：Hermes 的 gateway、持久记忆、计划任务和多后端执行值得借鉴；代价是 Python 单体/注册表式内部结构与很大的工具、渠道、依赖攻击面。[Hermes README](https://github.com/NousResearch/hermes-agent/blob/main/README.md)｜[Hermes 架构文档](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)
- Claude Code 的可借鉴点是成熟的**模型-工具循环、上下文治理、权限/检查点、任务隔离与多端体验**；但不能把其 CLI 当作一个可 fork 的开源 harness。Anthropic 的官方仓库公开的是安装入口和插件，仓库根部没有 `LICENSE`，README 也明确称其中包含“several Claude Code plugins”，而没有公开 CLI 实现源码；这是**仓库观察**，并非 Anthropic 的“闭源”措辞。[官方仓库 README](https://github.com/anthropics/claude-code/blob/main/README.md)｜[工作原理](https://code.claude.com/docs/en/how-claude-code)

---

## 1. Claude Code（Anthropic）

### 源码可获得性与官方入口

- **CLI/harness 的完整源码：不可作为开源源码取得（源码观察）。** 官方仓库是 [`anthropics/claude-code`](https://github.com/anthropics/claude-code)：README 提供安装、文档与插件目录；根部 `LICENSE` URL 返回 404，README 只说明仓库包含若干插件。因而应将可获得物视为闭源产品二进制/安装包加少量公开扩展，而不是可 fork 的 harness；不要从“能 npm/installer 安装”推断源码开放。[仓库 README](https://github.com/anthropics/claude-code/blob/main/README.md)｜[官方安装说明](https://code.claude.com/docs/en/overview)
- **官方文档：** [Claude Code Docs](https://code.claude.com/docs/en/overview)。官方还提供 [Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview)，可在 Claude Code 工具/能力之上编排自定义代理，但这不等于发布 CLI 内核源码。[概览](https://code.claude.com/docs/en/overview)

### 官方定位/架构优势

- **定位：** 能读取代码库、编辑文件、执行命令并接入开发工具的 agentic coding tool，覆盖 terminal、IDE、桌面和 Web；官方将“模型 + 工具 + 上下文管理层”明确称作 agentic harness。[概览](https://code.claude.com/docs/en/overview)｜[工作原理：agentic loop](https://code.claude.com/docs/en/how-claude-code#the-agentic-loop)
- **核心循环与工具：** 按“收集上下文 → 行动 → 验证”反复运行；内建文件、检索、执行、Web、代码智能等工具，并可用 MCP、skills、hooks、subagents 扩展。[工作原理](https://code.claude.com/docs/en/how-claude-code#the-agentic-loop)
- **上下文与并行：** session 使用本地 JSONL、文件检查点，支持 resume/fork；subagent 具有独立上下文窗口并仅把摘要带回，git worktree 用于并行隔离。[会话与上下文](https://code.claude.com/docs/en/how-claude-code#work-with-sessions)｜[上下文成本](https://code.claude.com/docs/en/how-claude-code#manage-context-with-skills-and-subagents)
- **安全产品化：** 提供权限模式、工作目录边界、Bash sandbox 和检查点；云端 session 使用隔离 VM、网络控制与审计日志。[安全文档](https://code.claude.com/docs/en/security)

### 缺点/边界

- **官方声明的边界：** 会话上下文会填满；自动压缩会清除旧工具结果或摘要对话，早期详细指令可能丢失。单个文件/工具结果过大还可能导致 compaction thrashing 并报错。[上下文窗口](https://code.claude.com/docs/en/how-claude-code#the-context-window)
- **官方声明的边界：** 检查点仅覆盖文件修改，不能回滚数据库、API、部署等远程副作用；并且不恢复符号链接/硬链接路径。安全机制也“不完全免疫”提示注入，MCP server 不由 Anthropic 安全审计或管理。[检查点](https://code.claude.com/docs/en/how-claude-code#undo-changes-with-checkpoints)｜[安全](https://code.claude.com/docs/en/security)
- **官方明示的 sandbox 限制：** Bash sandbox 支持 macOS、Linux、WSL2，**不支持原生 Windows**；它不是完整隔离边界（例如网络不检查 TLS 内容，宽文件/网络许可可能扩大暴露面，file tools 不处于同一 Bash sandbox 边界）。不能把“有 sandbox”写成“可安全执行不可信任务”。[Sandboxing：平台与限制](https://code.claude.com/docs/en/sandboxing#limitations)
- **源码/产品边界（客观）：** 由于内核不可审计、不可重新许可/定制部署，无法像开源项目那样替换循环、会话格式、工具执行层或供应商模型栈；Agent SDK 是扩展/编排入口，不是该缺口的替代物。这是由“公开插件仓库 + 文档 API”而非完整实现源码得出的工程限制。[官方仓库 README](https://github.com/anthropics/claude-code/blob/main/README.md)｜[Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview)

---

## 2. OpenAI Codex（CLI / agent harness）

### 源码可获得性与官方入口

- **可获得、可修改。** 官方仓库 [`openai/codex`](https://github.com/openai/codex) 把 Codex CLI 定义为“runs locally on your computer”的 coding agent，提供从源码构建说明，许可证为 **Apache-2.0**。[README](https://github.com/openai/codex/blob/main/README.md)｜[安装与构建](https://github.com/openai/codex/blob/main/docs/install.md)｜[LICENSE](https://github.com/openai/codex/blob/main/LICENSE)
- **官方文档：** [Codex CLI](https://learn.chatgpt.com/docs/codex/cli)。注意区分本地开源 **Codex CLI** 与官方 README 另行指向的云端 **Codex Web**；“开源 CLI”不表示云服务端实现公开。[README](https://github.com/openai/codex/blob/main/README.md)

### 官方定位/架构优势

- **定位：** 一个在终端中检查代码、改文件、运行命令、并可用于脚本/CI 的本地代理；`codex exec` 直接覆盖非交互自动化场景。[CLI 文档](https://learn.chatgpt.com/docs/codex/cli)
- **实现可读性：** 官方构建文档说明源码工作区是 Rust/Cargo，`cargo run --bin codex` 启动 TUI，`codex exec` 是非交互模式；对自建 harness 而言，这是可实际阅读、构建、改造的一条基线。[安装与构建](https://github.com/openai/codex/blob/main/docs/install.md)
- **扩展和接口：** CLI 文档列出 MCP、skills/plugins、subagents、Web search、云端移交、session resume 与 permissions/sandbox 边界；可在一条本地 terminal loop 中拼接这些能力。[CLI 文档](https://learn.chatgpt.com/docs/codex/cli)
- **面向产品集成：** 源码仓库包含 app-server 的 JSON-RPC 侧，官方 app-server README 记录 thread、MCP、网络策略等接口演进；适合作为 GUI/IDE 与核心代理分离的参考，但 API 有实验性部分。[app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)

### 缺点/边界

- **官方声明的边界：** 从源码构建的系统要求列出 macOS、Ubuntu/Debian，Windows 是“via WSL2”；因此不应把源码开发/运行支持假定为完全原生跨平台（即便发布安装器另有 Windows 路径）。[安装与构建：System requirements](https://github.com/openai/codex/blob/main/docs/install.md)
- **官方声明的边界：** 它运行本机已有工具、可编辑本地文件和运行命令；这本身要求使用者设置每次运行的权限与 sandbox/writable roots，且官方建议在任务前后创建 Git checkpoint 以便回退。官方安全文档还说明默认 OS sandbox 的平台实现不同，且 `--sandbox danger-full-access` / `--yolo` 会取消 sandbox 与审批（不推荐）；网络代理也不覆盖 web search、MCP、browser、cloud task 及模型/认证等所有流量面。[CLI 文档](https://learn.chatgpt.com/docs/codex/cli)｜[Approvals & security](https://learn.chatgpt.com/docs/agent-approvals-security)
- **源码观察的限制：** Codex 的开源范围是 CLI/本地 harness，不包含官方云端 Codex Web 后端；同时，app-server 文档明确标注部分 RPC/功能为 experimental，且存在“只适于 local stdio/UI”的能力约束。若把它当稳定的跨客户端协议，应自行锁版本并做兼容层。[README：Web 区分](https://github.com/openai/codex/blob/main/README.md)｜[app-server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)

---

## 3. Hermes Agent（Nous Research；名称消歧后的目标）

### 源码可获得性与官方入口

- **可获得、可修改。** 官方仓库 [`NousResearch/hermes-agent`](https://github.com/NousResearch/hermes-agent) 明示 MIT；官方架构文档提供项目结构、agent loop、provider、工具、session 与 gateway 的路径说明。[README](https://github.com/NousResearch/hermes-agent/blob/main/README.md)｜[LICENSE](https://github.com/NousResearch/hermes-agent/blob/main/LICENSE)｜[架构](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)
- **官方文档：** [Hermes Agent Docs](https://hermes-agent.nousresearch.com/docs/)。

### 官方定位/架构优势

- **定位不是纯 coding CLI：** Nous 将它定位为“self-improving AI agent”，可在 VPS/GPU/serverless 运行、可从 Telegram 等渠道沟通，支持可切换的多模型/多供应商；coding 只是其工具化自治能力的一部分。[README](https://github.com/NousResearch/hermes-agent/blob/main/README.md)
- **单一核心、多入口：** 文档显示 CLI、Gateway、ACP、batch runner、API server、Python library 都进入 `AIAgent`；平台差异放在入口层，核心 loop 处理 provider、prompt、tools、重试、压缩和持久化。[架构：System Overview 与 Agent Loop](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)
- **长期工作流能力：** SQLite + FTS5 session/search、memory/skills、cron、隔离 subagents、Python RPC pipeline 与多种 terminal backend（local/Docker/SSH/Modal/Daytona/Singularity/Vercel Sandbox）构成“常驻代理”而非短命 CLI 的能力面。[README](https://github.com/NousResearch/hermes-agent/blob/main/README.md)｜[架构：Session/Tools/Gateway](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)
- **可替换点：** 中央工具注册表、plugin discovery、可插拔 context engine/memory provider、多 API mode 的 provider resolver，让多模型和多环境是显式子系统。[架构：Major Subsystems](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)

### 缺点/边界

- **官方陈述的结构性取舍：** 工具在 import 时通过 `registry.register()` 自注册，链式依赖由 `model_tools.py` 触发发现；这让扩展很直接，但工具发现/副作用与核心加载时间耦合，难以像显式依赖注入图一样做局部静态组合/隔离（后半句为源码结构观察）。[架构：File Dependency Chain](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)
- **官方明示的安全边界：** 危险命令 approval 有 smart/manual/off；Docker/Singularity/Modal/Daytona/Vercel Sandbox 后端会跳过危险命令检查，因为容器被视为安全边界。更关键的是，文件写保护仅覆盖 `write_file`/`patch`，terminal 与同一 OS user 运行，仍可能经 shell 读写被拒路径；官方明确说这并不 sandbox hostile/compromised agent。[安全：dangerous-command approval](https://hermes-agent.nousresearch.com/docs/user-guide/security#dangerous-command-approval)｜[安全：file-write safety](https://hermes-agent.nousresearch.com/docs/user-guide/security#file-write-safety)
- **源码观察的限制：** 70+ 工具、约 28 toolsets、7 个 terminal backends、25+ 平台 adapter 与 gateway 带来很宽的插件/凭证/网络攻击面及运营负担；对只做本地 coding harness 的团队，先采用其核心 loop 的全部能力会过度设计。规模数据来自官方架构文档，风险判断是工程推论。[架构](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)
- **产品边界：** Hermes 的“自我学习”主要是 skills、记忆、摘要、搜索与自动化机制；不应解读成无需评估的自我改进模型训练。官方文档还将 trajectory generation 单列为训练数据生成功能。[README](https://github.com/NousResearch/hermes-agent/blob/main/README.md)｜[架构：Trajectories](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture)

---

## 4. DeepSeek Harness / DSH（DeepSeek AI）

### 源码可获得性与官方入口

- **可获得、可修改。** 官方仓库是 [`deepseek-ai/deepseek-harness`](https://github.com/deepseek-ai/deepseek-harness)，README 将其定义为开源 agent harness，许可证为 **MIT**，并提供从源码 build/run 的命令。[README](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.md)｜[LICENSE](https://github.com/deepseek-ai/deepseek-harness/blob/master/LICENSE)
- **官方文档：** [DeepSeek Harness Docs](https://deepseek-harness.github.io/deepseek-harness/)；架构主文档为 [Architecture](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)。

### 官方定位/架构优势

- **最鲜明定位：Everything is a Plugin。** DSH 基于 Cordis，模型 adapter、工具注册表、session log 和 agent loop 都是插件；插件为共享 context 添加 service、typed event 和可逆 effect，配置可替换、卸载可回滚，不存在需修改的“特权核心”。[README](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.md)｜[Architecture：Cordis](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)
- **组合方式可审计：** profile 由有序 bundle、profile patch、home patch 与命令行 patch 分层；官方提供 `dsh --profile web --dump-config` 查看实际启动的插件树。web/headless/sdk/acp 是不同的 profile，而非把所有模式揉进一个入口。[Architecture：Profiles and bundles](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)
- **事件溯源和可重放性：** session log 是模型上下文来源；turn/step、消息、tool 等是耐久事件，`deriveMessages()` 从 log 投影模型历史，并以“不记录就不可见”为不变量。这是构建可审计、可恢复代理的强参考。[Architecture：Turn flow/Session log](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)
- **能力 seam：** 文件系统、subprocess/sandbox、LLM、tool、subagent、持久化等按接口/提供者/消费者分离；更换 provider 可使整个执行世界（包括 Bash/PTY/LSP）迁移，是构建可测试 provider abstraction 的直接范式。[Architecture：Capability seams](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)
- **安全默认值：** base profile 配有 workspace 写入边界和风险操作审批，Windows 与 POSIX 提供不同 shell stack；Web UI 文档说明 agent 可读写、执行、委派、维护计划，但由当前权限策略决定是否审批。[dsh-base README](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/base/README.md)｜[Web UI guide](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/user/guide/index.md)

### 缺点/边界

- **官方声明：developer preview、兼容性会破坏。** README 明确说快速迭代且“THERE WILL BE COMPATIBILITY-BREAKING CHANGES”。这要求自建产品只选取并固定接口，而不是无版本隔离地追主分支。[README](https://github.com/deepseek-ai/deepseek-harness/blob/master/README.md)
- **官方声明：不安全/非 production-ready。** SAFETY.md 明确说尚未经过安全审计，不能视为 secure 或 production-ready；sandbox、审批和权限不会保证隔离，也不能保护已被授权访问的资源。[SAFETY.md](https://github.com/deepseek-ai/deepseek-harness/blob/master/SAFETY.md)
- **官方源码文档的客观限制：** patch 覆盖整块 config 而非 merge，覆盖必须重述要保留的设置；同时 plain FS provider 和 sandboxed FS provider 不能并存（注册同一 service 会拒绝加载）。这是很强的组合一致性约束，也提高定制复杂度。[dsh-base：Known Limitations](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/base/README.md#known-limitations-and-deferred-work)
- **工程取舍（源码观察）：** 全插件、事件、profile/bundle/patch 的可替换性极强，但概念数量和启动组合复杂度也高；对只有“单模型 + 本地 shell + CLI”需求的 MVP，直接复制其全部 Cordis 抽象会推迟交付。架构可借鉴其边界，而非必须全量复刻。[Architecture](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md)

---

## 简洁对比表

| 项目 | harness 源码/许可 | 最适合借鉴 | 主要边界（来源类别） |
|---|---|---|---|
| Claude Code | **完整内核不可得**；官方 repo 为安装入口/插件（仓库观察） | 端到端产品体验、权限/检查点、上下文治理、worktree/subagent UX | 远程副作用不可 checkpoint、context compaction 有损、MCP 不由厂商审计（官方）；不可 fork/审计核心（观察） |
| Codex CLI | **开源**，Apache-2.0 | Rust 本地 terminal loop、CLI/CI、app-server 分离 | 源码构建 Windows 经 WSL2（官方）；云端 Web 不在开源仓库、部分 app-server API experimental（观察/源码） |
| Hermes Agent | **开源**，MIT | 常驻多渠道、长期记忆/任务、环境后端、多模型 | 功能面与攻击面/运营面都很大（结构事实 + 工程推论）；工具 import 自注册耦合（源码） |
| DSH | **开源**，MIT | plugin graph、事件溯源、capability seam、profile composition | developer preview 且未安全审计（官方）；patch 整块替换/服务冲突约束（源码） |

## 给自建 harness 的设计建议

1. **先实现小而硬的内核。** 以不可变 `Turn/Step/ToolCall/ToolResult` 事件日志为唯一事实源（DSH），提供 `deriveModelHistory()`；UI 与存储从投影读，不直接修改代理状态。这样可重放、审计、resume/fork，并可验证模型看到的内容。
2. **将循环、工具和执行环境分层。** 模型循环只消费 `ToolRegistry` 与 `ExecutionPolicy`；文件/进程/网络由一个 capability/provider seam 提供（DSH）。每一个 provider 都接受同一效果语义，而非让 tools 直接调用本机 shell。
3. **默认最小权限且显式记录授权。** 借鉴 Claude Code/Codex 的“可见命令、可配置权限、可写根目录、sandbox”思路；但明确声明 sandbox 不是安全边界，untrusted task 仍应进 VM/container（DSH 的安全说明尤应采纳）。
4. **把上下文治理做成协议的一部分。** 区分耐久指令、可丢弃工具输出和压缩摘要；为单次大输出/压缩循环设上限与可观测报错（Claude Code 已披露这类失败模式）。subagent 应返回结构化摘要，默认不把工具轨迹塞回父上下文。
5. **协议稳定、实现可替换。** 把 app/IDE 前端和 agent server 用有版本的 JSON-RPC/streaming 协议隔开（Codex 的 app-server 思路），同时在 plugin/adapter 边界标出 stable vs experimental；不要让 UI 依赖内部循环对象。
6. **MVP 不要复制“全能代理”。** 第一版限制为：本地 workspace、受策略控制的 read/search/edit/exec、单 session JSONL/SQLite、显式 approval、`exec` 非交互入口。再按需求加 MCP、subagent、worktree、远程 gateway、长期记忆。Hermes 的广度应按需吸收，DSH 的组合能力应在接口稳定后引入。

## 官方来源索引

- Claude Code: [Docs overview](https://code.claude.com/docs/en/overview), [How it works](https://code.claude.com/docs/en/how-claude-code), [Security](https://code.claude.com/docs/en/security), [official GitHub repository](https://github.com/anthropics/claude-code).
- OpenAI Codex: [official GitHub repository](https://github.com/openai/codex), [README](https://github.com/openai/codex/blob/main/README.md), [build/install](https://github.com/openai/codex/blob/main/docs/install.md), [Codex CLI docs](https://learn.chatgpt.com/docs/codex/cli), [app-server source documentation](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md).
- Hermes Agent: [official GitHub repository](https://github.com/NousResearch/hermes-agent), [official docs](https://hermes-agent.nousresearch.com/docs/), [architecture](https://hermes-agent.nousresearch.com/docs/developer-guide/architecture).
- DSH: [official GitHub repository](https://github.com/deepseek-ai/deepseek-harness), [architecture](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md), [safety](https://github.com/deepseek-ai/deepseek-harness/blob/master/SAFETY.md), [dsh-base](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/base/README.md).
