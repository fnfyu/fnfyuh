# fnfyu harness

一个本地优先、事件驱动、可替换执行环境的 coding-agent runtime。`local-first-harness` 是兼容协议与内部仓库名，产品面向用户统一称为 `fnfyu harness`。

## 当前状态

核心 coding-agent 循环已可用：多步任务中只读工具（read/list/search/image）自动执行，写入/编辑/进程工具经审批后继续，测试失败会把真实 stdout/stderr 反馈给模型修复后重试，跨回合保留最近 192 KiB 工具输出上下文，运行中的任务可通过 `runtime.v1.turn.stop` 请求停止（当前步骤结束后不再继续，不中断已发出的模型请求）。模型步数上限 32 步/回合。

底座包含：SQLite hash-chained append-only event log、replay/resume/fork projection、deny-by-default policy、trusted-workspace execution seam、stdio daemon、CLI 和 TypeScript SDK，均已在 WSL2 Docker builder 中通过 Rust/Node 测试。带 `command_id` 的 durable receipt/replay、create/fork/recover 原子提交、typed subscription、租约化持久 outbox、artifact retrieval、backend inspect/continuation，以及可配置的 provider/model settings。配置只保存 provider/model 元数据和 secret reference，不保存 API key。执行面默认仍是明确标注的 trusted-host；`container`/`vm` 只在预检通过时启用，失败即 fail-closed，不会回退到 host。Web/IDE 客户端位于 `apps/web`、`apps/gateway` 和 `apps/ide`。

尚未完成：daemon 在模型/工具执行期间无法响应除 stop 外的并发 RPC（同步串行循环）；无 session 级 diff 视图（Web 的文件操作记录仅汇总成功的写入/编辑）；无 token 流式输出（回合以完整响应结束）。

## 设计原则

- SQLite + append-only event log 是唯一事实源。
- Session/Turn/Step、工具审批和执行结果都以不可变事件表示。
- Agent 只提出 execution intent；PolicyEngine 决策后由 ExecutionBroker 执行。
- shell/file intent 只能通过同一 capability policy 与 ExecutionBroker；shell 永不经字符串 shell。默认不启用 host process；trusted-host opt-in 不是 hostile sandbox。
- daemon 与 CLI/Web/IDE 通过版本化 JSON-RPC 与流式事件连接。
- 插件默认进程外，协议和 manifest 先于具体实现。

## Layout

```text
crates/                  Rust domain modules and adapters
apps/                    daemon and client entrypoints
sdk/                     TypeScript JSON-RPC client
plugins/                 external-plugin protocol adapters
docs/changes/            accepted change pack
```

## 启动 Web

产品首选启动命令是 `fnfyuh`。在仓库根目录执行：

```bash
pnpm fnfyuh web
```

它会通过 WSL2 + Docker **增量构建当前源码镜像**（包括 Rust daemon；不默认强制拉取基础镜像），然后启动 fnfyu harness 网关。当前仓库源码只读挂载到网关可即时更新 Web/网关脚本，但**不会更新镜像内的 `/usr/local/bin/harnessd` 和镜像依赖**；修改 Rust、依赖或打包内容后须重建镜像并重启网关。默认 host network 只监听 `127.0.0.1:8787`，本机浏览器无需设置 token，可打开 <http://127.0.0.1:8787>。WSL/Docker Desktop 环境中请确认 Windows 侧 localhost 转发正常。

Windows PowerShell 也可以使用不依赖全局安装的 wrapper：

```powershell
.\scripts\fnfyuh.ps1 web
```

如果希望安装成全局 `fnfyuh` 命令：

```bash
pnpm link --global
fnfyuh web
```

仅确认现有镜像与当前源码匹配时，可显式跳过构建；该选项会继续使用旧 daemon 二进制。需要检查基础镜像更新时使用 `--build`（带 `docker compose build --pull`）：

```bash
fnfyuh web --no-build
fnfyuh web --build
```

显式非本地部署可覆盖 `HARNESS_GATEWAY_HOST`，但必须配置 `HARNESS_GATEWAY_TOKEN`；无 token 时非 loopback 监听的 `/rpc` 和 `/events` 会拒绝请求。不要将 `0.0.0.0` 视为本地模式；浏览器 token 配置仅适用于显式部署，不是默认启动步骤。

编码任务若需要运行测试/编译命令，默认本地策略只允许工作区文件读写，不运行宿主进程。由你信任此工作区时设置 `HARNESS_TRUSTED_PROCESS=1`，并通过 `HARNESS_ALLOWED_PROGRAMS` 提供**绝对可执行文件路径**的列表（Linux/WSL 用冒号分隔，如 `/usr/bin/python3:/usr/bin/git`）；然后重启网关。写文件和运行命令默认仍要在界面批准；仅需无人值守时才另设 `HARNESS_TRUSTED_AUTO_APPROVE=1`。该模式的进程在网关容器内以用户权限运行，**不是沙箱**，请不要用于不信任的工作区。

原有 `pnpm dsh web` 仍作为兼容入口保留，不会覆盖系统已有的 DeepSeek Harness `dsh` 命令。

## OpenAI Codex OAuth provider

工作台复用 `@earendil-works/pi-ai` 的 `openaiCodexProvider().auth.oauth`，通过 OpenAI 官方 OAuth + PKCE 登录 ChatGPT/Codex，不读取 ChatGPT Cookie，也不调用 Codex CLI 登录。设置面板中选择 `OpenAI Codex（ChatGPT OAuth）`，添加模型后可选择浏览器登录或设备码登录；凭据保存在 `/data/openai-codex-oauth.json`，请求前由 pi-ai 自动 refresh。若浏览器授权页被 Cloudflare 拦截，可尝试设备码方式，但仍受 OpenAI 地区与网络策略限制。

该 provider 使用 `chatgpt.com/backend-api` 的 Codex transport，不需要 `OPENAI_API_KEY`。OAuth callback 端口默认为 `1455`，Docker Web profile 已映射该端口。模型设置中勾选“支持图片理解”后，图片附件会通过 pi-ai 的 Codex 请求投影发送。

## WSL2 + Docker 成品环境

在 WSL2 中执行：

```bash
bash scripts/wsl-docker-verify.sh
```

它会构建包含 Rust 1.85.1、Node 22/npm 的镜像，并运行 SDK 与 Rust workspace 验证。完整要求和运行方式见 [`docs/operations/wsl-docker.md`](docs/operations/wsl-docker.md)。

See `docs/changes/local-first-harness/` for the implementation slice and verification plan.
