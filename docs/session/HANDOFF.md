# Harness implementation handoff

## Current milestone

The production-completion slice is implemented in the current workspace.

- Rust workspace crates remain the event-sourced core: protocol, session engine,
  prompt compiler, policy engine, execution broker, agent runtime, and plugin host.
- Execution broker now owns an adapter seam with explicit backend binding, durable
  backend inspection/continuation, a Docker container path, fail-closed preflight,
  bounded resources, no network, read-only root, and isolated workspace mapping.
  Trusted-host remains an explicit convenience mode and is not a hostile sandbox.
- Session engine now persists leased subscription outbox rows, durable artifact
  records, artifact events, backend inspection/continuation facts, and atomic tool
  terminal/artifact commits. Replay accepts legacy schema-1 rows and writes schema 2.
- Agent runtime now supports provider-neutral model tool calls, an OpenAI-compatible
  HTTP provider with env-only credentials, normalized request correlation, inspect /
  continuation semantics, and atomic artifact-backed terminal tool recording.
- Client surface includes typed Node SDK helpers, browser-safe fetch/SSE client,
  local HTTP/SSE gateway, dependency-free Web console, and an IDE-oriented Node
  adapter. The gateway never forwards provider secrets to browsers.
- The product-facing identity is now `fnfyu harness`; `pnpm fnfyuh web`, the `fnfyuh`
  bin, and `scripts/fnfyuh.ps1` are the preferred startup paths. The old `dsh` path
  remains a compatibility alias and is not overwritten.
- `harness-settings` persists provider/model metadata and secret references; the
  workbench exposes Chinese provider/model settings for OpenAI-compatible, OpenAI,
  DeepSeek, Ollama/local, Anthropic, and OpenAI Codex OAuth profiles.
- The typed tool seam now includes `list_files` and bounded `read_image` media output
  alongside read/search/write/edit/process tools; the workbench shows a tool queue,
  approval/execute actions, and image previews.

## Verification evidence

- Fresh `pnpm test`: 9 Node contract tests passed.
- Fresh `pnpm build:sdk`: TypeScript build passed with settings/tool helpers.
- Fresh `pnpm run web:smoke`: Web assets present.
- Fresh Node syntax checks passed for launcher, gateway, and Web modules; `pnpm fnfyuh
  --help`, `pnpm fnfyuh web --help`, the PowerShell wrapper help, Web smoke, SDK tests,
  SDK build, and format checks passed.
- WSL Docker `cargo test --workspace` passed, including settings, Anthropic request,
  list-files, and image-read tests; release daemon compilation also passed.
- Fresh WSL Docker smoke started `pnpm fnfyuh web`, returned gateway health 200, served
  the `zh-CN` branded page, and completed a live settings get/save round trip with five
  provider profiles; the verification container was stopped afterward.
- Native Windows shell does not expose `cargo`/`docker`, but the configured WSL2/Docker
  builder completed `cargo test --workspace`, `cargo test --workspace --locked`,
  release compilation, the Node suite, SDK build, Web smoke check, and CLI health
  check. The generated `Cargo.lock` now includes the pinned Rust-1.85-compatible
  provider dependency graph.

## Runtime configuration

- `HARNESS_EXECUTION_BACKEND=local-trusted-host|container|vm`
- `HARNESS_CONTAINER_RUNTIME`, `HARNESS_CONTAINER_IMAGE`, isolation/resource limits
- `HARNESS_MODEL=gpt-5.4` or a configured model name with
  `HARNESS_MODEL_ENDPOINT`, optional `HARNESS_MODEL_API_KEY`, and timeout
- `HARNESS_DB` for the SQLite event store
- `HARNESS_SETTINGS` for the local provider/model metadata file; provider API keys are
  supplied through the configured environment-variable references, never the file.

Do not put real keys in `.env.example`, docs, HANDOFF, or event/artifact payloads.
The standard runtime image is socket-free; a Docker socket is host-admin-equivalent
and must not be mounted in a hostile-agent deployment.
