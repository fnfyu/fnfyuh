# Design

## Product-facing compatibility

- `fnfyu harness` is the product identity and `fnfyuh` is the preferred launcher.
- `local-first-harness.v1`, `harnessd`, `HARNESS_*`, and existing RPC names remain compatibility names for this migration. They are not silently renamed in the event log or wire protocol.

## Deep modules and seams

### SettingsStore

A small settings interface owns an atomic local JSON configuration file:

- `load()` returns validated provider profiles, models, and the selected default model.
- `save(settings)` validates ids, endpoints, provider kinds, model ownership, and enabled state before an atomic replace.
- `public_view()` removes secrets and exposes only secret-reference names plus configured booleans.

The store persists metadata only. The daemon composition root resolves a secret reference from the environment before constructing a provider adapter; the adapter receives the credential value only for the outbound request, and the value never crosses JSON-RPC or enters events.

### Provider resolution seam

The current composition seam is `configured_provider` in the daemon. It resolves one model id to one provider adapter and keeps settings-file parsing and provider-kind selection in one place. There is no silent provider fallback: an unknown or unavailable model fails explicitly. The adapters hide header/body differences, OAuth bridge calls, response normalization, tool-call normalization, timeouts, and bounded response reads behind the existing `ModelProvider` interface.

Adapters:

- `OpenAiCompatibleProvider` for custom endpoints and OpenAI/DeepSeek/Ollama presets.
- `AnthropicProvider` for the Messages protocol.
- `CodexOAuthProvider` for pi-ai's OpenAI Codex OAuth transport.

Unknown model/provider ids fail explicitly. Settings are the only routing source; no environment-only provider fallback exists.

### Built-in tool seam

The current tool seam has two deliberate halves: `default_tools` in the daemon owns the model-visible schemas, while `parse_tool_call` in agent-runtime owns typed argument validation and alias normalization. Both produce the same typed `ToolIntent` variants:

- `read_file`, `list_files`, `search`, `read_image`
- `write_file`, `edit_file`
- `exec`, `test`, `git`

`ToolCoordinator`, `PolicyEngine`, and `ExecutionBroker` remain the single execution seam, so a new read-only tool cannot bypass path, symlink, size, approval, or audit rules. A future refactor can move the two schema/parser halves behind one registry without changing callers.

### Media artifacts

`read_image` is a read-only intent. The backend bounds image bytes, identifies a supported media type, and returns bounded base64 media metadata. `ToolCoordinator` stores the content as a protected `image_base64` artifact and emits its id through the existing artifact event; the browser can preview the immediate result or retrieve the session-scoped artifact. Provider adapters can later map a supported image artifact into their native vision message shape without changing the tool policy seam.

## Storage and wire shape

- Settings are local mutable configuration, not event-sourced facts. A settings save is atomic and validated; session events record only the selected model id and tool operation metadata.
- `runtime.v1.settings.get` and `runtime.v1.settings.save` are additive RPC methods.
- `runtime.v1.models` returns the sanitized registry view and configuration state.
- `runtime.v1.tool.execute` remains the explicit execution method; model-proposed tools continue through proposal/approval/start/terminal events.

## Delivery order

1. Product identity and launcher aliases.
2. SettingsStore, settings RPC, SDK helpers, and Chinese settings surface.
3. ProviderRegistry plus OpenAI-compatible presets and Anthropic adapter.
4. Tool registry, list/image intents, bounded media artifacts, and tests.
5. Workbench model selector, Codex OAuth login, tool/event/artifact presentation, compatibility verification.
