# Proposal

- **Status:** implemented
- **Owner:** project owner
- **Why:** 项目已有事件驱动 runtime，但产品身份、启动入口、内置工具、模型供应商配置和工作台设置还没有形成一个可持续扩展的用户面。
- **What changes (user-facing):** 将产品面向用户命名为 `fnfyu harness`，提供 `fnfyuh` 启动命令；建立受 workspace policy 约束的内置工具注册中心；在简体中文工作台设置中管理 provider profile 和 model；支持 OpenAI-compatible、OpenAI、DeepSeek、Ollama/本地模型和 Anthropic；保留现有 `local-first-harness.v1` 协议与 session/event/recovery 兼容性。
- **Out of scope:** 多用户远程控制台、云端密钥托管、默认联网浏览器、任意插件自动获得工具权限、破坏性 schema 重写。Codex OAuth is a local pi-ai bridge backed by the runtime data volume.
- **Risks / rollback:** 新增配置文件和 RPC 只向后兼容扩展；API key 只通过 secret reference 注入，不写入 settings、事件、日志或浏览器；可按 vertical slice 回退新 launcher、settings、tool/provider adapters，不修改既有事件事实。

## Acceptance

- [x] `fnfyuh` 或项目内 `pnpm fnfyuh` 能启动 Web workbench，产品可见文案使用 `fnfyu harness`。
- [x] Web 设置可以列出、新增、更新 provider profile 和 model，删除 provider profile，并选择默认 model；API key 只显示配置状态和 secret reference。
- [x] provider profile 能路由到 OpenAI Codex OAuth、OpenAI-compatible、OpenAI、DeepSeek、Ollama/本地 endpoint 和 Anthropic；未知或未配置 provider 明确失败，不静默回退。
- [x] agent tool schema 至少包含读取文件、列出文件、文本搜索、图片读取/预览、写入、精确编辑和受策略约束的进程工具。
- [x] 所有文件/图片工具经过同一 policy/execution seam；越界、符号链接和超限请求在执行前拒绝，并留下可审计事件。
- [x] 图片结果以受限 artifact/media metadata 传递，浏览器可预览；不把任意二进制或 secret 写入事件正文。
- [x] 现有 session、turn、event subscription、operation inspect/continue/recovery 和旧 `local-first-harness.v1` 客户端继续工作。
- [x] Node/Rust targeted tests、SDK build、Web smoke、Compose config 和至少一次真实 `fnfyuh` Web health check 通过。
