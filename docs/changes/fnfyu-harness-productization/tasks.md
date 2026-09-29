# Tasks

## 1. Product identity and launcher

- **Files:** `package.json`, `apps/cli/fnfyuh.mjs`, `scripts/fnfyuh.ps1`, `apps/web/index.html`, `README.md`, `docker-compose.yml`
- **Verify:** `pnpm fnfyuh --help`, `fnfyuh --help`, Compose config, live `/health`
- **Done when:** `fnfyuh` is the preferred startup command and visible branding says `fnfyu harness`; old `dsh` compatibility is not overwritten
- **Status:** done

## 2. SettingsStore and provider/model RPC

- **Files:** `crates/settings/**`, workspace `Cargo.toml`, `apps/daemon/Cargo.toml`, `apps/daemon/src/main.rs`, `sdk/src/runtime.mjs`, `sdk/src/browser.mjs`
- **Verify:** settings unit tests, daemon RPC contract tests, `runtime.v1.settings.get/save`, no secret values in serialized responses
- **Done when:** provider profiles and models persist atomically, validate ownership, expose sanitized configuration state, and select a default model
- **Status:** done

## 3. Provider adapters

- **Files:** `crates/agent-runtime/src/lib.rs`, settings integration, targeted tests, `docs/providers.md`
- **Verify:** provider request-shape tests for OpenAI Codex OAuth, OpenAI-compatible, DeepSeek/Ollama presets, and Anthropic; credential redaction tests
- **Done when:** configured models route to their provider adapter and malformed/unconfigured providers fail explicitly
- **Status:** done

## 4. Built-in tool registry and safe media tools

- **Files:** `crates/protocol/src/lib.rs`, `crates/policy-engine/src/lib.rs`, `crates/execution-broker/src/lib.rs`, `crates/agent-runtime/src/lib.rs`, `apps/daemon/src/main.rs`, `crates/session-engine/src/lib.rs`
- **Verify:** Rust policy/broker tests for list/read/search/image bounds, symlink rejection, artifact metadata, and existing write/exec behavior
- **Done when:** read_file/list_files/search/read_image plus existing write/edit/process tools share one typed policy and audit seam
- **Status:** done

## 5. Chinese settings and workbench surface

- **Files:** `apps/web/index.html`, `apps/web/app.mjs`, `apps/web/styles.css`, `sdk/src/browser.mjs`
- **Verify:** Web smoke, static locale check, live settings get/save, model selector and provider form checks at desktop/mobile widths
- **Done when:** 用户能在工作台设置中管理 provider/model、选择模型、查看工具/事件/图片 artifact 状态，且所有产品文案为简体中文
- **Status:** done

## 6. Compatibility, review, and handoff

- **Files:** `sdk/test/**`, `docs/changes/fnfyu-harness-productization/**`, `docs/session/HANDOFF.md`, `README.md`
- **Verify:** targeted Node/Rust tests, SDK build, Web smoke, Compose config, live `fnfyuh` health check, one Spec/Standards review
- **Done when:** 旧 session/event/recovery 客户端行为保持，验证证据记录完整，未授权不提交 Git
- **Status:** done
