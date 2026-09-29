import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";

const credentialFile = join(tmpdir(), `fnfyuh-codex-auth-${process.pid}.json`);
process.env.HARNESS_CODEX_OAUTH_FILE = credentialFile;
const { handleCodexRpc, modelOptionsFromParameters } = await import("../../apps/gateway/codex-auth.mjs");
const { listProviderModels } = await import("../../apps/gateway/provider-models.mjs");

test("gateway handles Codex auth status locally", async () => {
  const response = await handleCodexRpc({
    jsonrpc: "2.0",
    id: "codex-status-test",
    method: "runtime.v1.codex.auth.status",
    params: {},
  });

  assert.equal(response.jsonrpc, "2.0");
  assert.equal(response.id, "codex-status-test");
  assert.equal(response.result.authenticated, false);
  assert.equal(response.result.expires_at_ms, undefined);
});

test("gateway leaves unrelated RPC methods for the daemon", async () => {
  assert.equal(
    await handleCodexRpc({ method: "runtime.v1.health", params: {} }),
    null,
  );
});

test("Codex bridge maps turn parameters to pi-ai options", () => {
  const options = modelOptionsFromParameters({ reasoning_effort: "high", temperature: 0.7, max_output_tokens: 4096 });
  const { onPayload, ...plainOptions } = options;
  assert.deepEqual(plainOptions, { reasoning: "high", temperature: 0.7, maxTokens: 4096 });
  assert.deepEqual(onPayload({}), { max_output_tokens: 4096 });
  const disabled = modelOptionsFromParameters({ reasoning_effort: "off" });
  assert.equal(disabled.reasoning, "off");
  assert.deepEqual(disabled.onPayload({}), { reasoning: { effort: "none", summary: "auto" } });
  assert.deepEqual(modelOptionsFromParameters({ reasoning_effort: "auto" }), {});
});

test("Docker Web profile exposes the OAuth callback on localhost", async () => {
  const compose = await readFile(new URL("../../docker-compose.yml", import.meta.url), "utf8");
  assert.match(compose, /network_mode:\s*host/);
  assert.match(compose, /1455/);
});

test("Codex provider model discovery returns the installed catalog", async () => {
  const result = await listProviderModels({ provider_id: "openai-codex", kind: "openai_codex" });
  assert.ok(result.models.some((model) => model.id === "gpt-5.4"));
});

test("OpenAI-compatible model discovery reads the provider model endpoint", async () => {
  const server = createServer((request, response) => {
    assert.equal(request.url, "/v1/models");
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ data: [{ id: "model-a", name: "Model A" }] }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  try {
    const result = await listProviderModels({
      provider_id: "fake-provider",
      kind: "open_ai_compatible",
      endpoint: `http://127.0.0.1:${port}/v1/chat/completions`,
    });
    assert.deepEqual(result.models, [{ id: "model-a", label: "Model A", supports_vision: false }]);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
