# Tasks

## 1. CLI launcher

- **Files:** `apps/cli/dsh.mjs`, `package.json`, `scripts/dsh.ps1`
- **Verify:** `pnpm dsh --help`、`pnpm dsh web --help`、`scripts/dsh.ps1 web --help`，以及 `pnpm dsh web` + `GET /health` → 帮助退出码为 0，健康检查返回 200
- **Done when:** 用户可以通过 `pnpm dsh web` 执行 launcher；需要精确 `dsh web` 时可用 PowerShell wrapper 转发到项目脚本，且不覆盖已有 DeepSeek Harness 命令
- **Status:** done

## 2. Simplified Chinese UI

- **Files:** `apps/web/index.html`, `apps/web/app.mjs`, `apps/web/styles.css`
- **Verify:** `pnpm run web:smoke` → `web assets ok`；实时页面检查 `lang="zh-CN"`、`运行工作台` 和 `创建会话` 均存在
- **Done when:** 页面及交互反馈统一使用简体中文，`lang="zh-CN"` 正确设置，功能调用不变
- **Status:** done

## 3. Verification and handoff

- **Files:** `docs/changes/harness-workbench-ui/proposal.md`, `docs/session/HANDOFF.md`
- **Verify:** `pnpm test`、`pnpm run build:sdk`、`pnpm run check:format`、`docker compose --profile web config` → 全部通过
- **Done when:** 启动方式、验证证据和未提交状态在交付说明中明确记录
- **Status:** done
