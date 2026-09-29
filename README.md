# fnfyu harness

一个本地优先、事件驱动、可替换执行环境的 coding-agent runtime。`local-first-harness` 是兼容协议与内部仓库名，产品面向用户统一称为 `fnfyu harness`。

## 当前状态

第一阶段的 Rust workspace、SQLite hash-chained append-only event log、replay/resume/fork projection、deny-by-default policy、trusted-workspace execution seam、stdio daemon、CLI 和 TypeScript SDK 已落盘，并已在 WSL2 Docker builder 中通过 Rust/Node 验证。

当前已提供带 `command_id` 的 SQLite durable receipt/replay seam、create/fork/recover 的原子 receipt+fact 提交、typed session/cursor backlog subscription、租约化持久 outbox、artifact retrieval、backend inspect/continuation，以及可配置的 provider/model settings。内置工具包含 read、list、search、image preview、write、edit 和受策略约束的进程工具；配置只保存 provider/model 元数据和 secret reference，不保存 API key。执行面默认仍是明确标注的 trusted-host；`container`/`vm` 只在 Docker engine、镜像和运行时预检通过时启用，失败即 fail-closed，不会回退到 host。Web/IDE 客户端位于 `apps/web`、`apps/gateway` 和 `apps/ide`。

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

它会通过 WSL2 + Docker 检查 `local-first-harness:dev` 镜像；镜像不存在时自动构建，然后启动 fnfyu harness 网关。当前仓库源码会以只读方式挂载到网关，因此 Web 文案修改无需重新构建镜像。浏览器打开 <http://127.0.0.1:8787>。

Windows PowerShell 也可以使用不依赖全局安装的 wrapper：

```powershell
.\scripts\fnfyuh.ps1 web
```

如果希望安装成全局 `fnfyuh` 命令：

```bash
pnpm link --global
fnfyuh web
```

已有镜像时可以跳过检查，需要刷新 daemon 或镜像内容时强制构建：

```bash
fnfyuh web --no-build
fnfyuh web --build
```

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
