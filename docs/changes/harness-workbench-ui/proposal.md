# Proposal

- **Status:** implemented
- **Owner:** project owner
- **Why:** 当前项目可以通过多个底层命令启动，但还没有类似 `dsh web` 的统一入口；现有 Web 控制台的用户可见文案也不是简体中文。
- **What changes (user-facing):** 增加项目内的 `dsh` CLI launcher，支持 `pnpm dsh web` 启动浏览器网关，并提供不覆盖现有 DeepSeek Harness 命令的 PowerShell wrapper；同时将 `apps/web` 的页面文案、状态提示和表单辅助信息统一为简体中文，保留现有 gateway、BrowserHarnessClient 和 runtime 行为。
- **Out of scope:** 三栏工作台重构、Rust daemon、gateway RPC、认证、远程项目持久化、后端协议变更。
- **Risks / rollback:** 变更集中在 CLI 入口、`package.json`、静态 Web 文案和变更记录；可回退这些文件，不影响 runtime 数据。

## Acceptance

- [x] 从项目目录执行 `pnpm dsh web` 可以启动 Web gateway。
- [x] `scripts/dsh.ps1 web` 可以转发到项目 launcher，且不会覆盖已有 DeepSeek Harness 命令。
- [x] `dsh --help`、未知子命令和启动失败都有清晰提示与非零退出码。
- [x] 页面语言声明为 `zh-CN`，所有用户可见标题、按钮、标签、占位符、空状态、toast 和错误提示使用简体中文。
- [x] 创建 session、发送 turn、实时事件、inspect/continue 的调用和状态行为保持不变。
- [x] 现有 Node 测试、Web smoke 和格式检查通过。
