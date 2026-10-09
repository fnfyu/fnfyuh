import test from "node:test";
import assert from "node:assert/strict";
import { request as httpRequest } from "node:http";
import { createGatewayServer } from "../../apps/gateway/server.mjs";
import { listProviderModels } from "../../apps/gateway/provider-models.mjs";

function postRpc(port, payload, headers = {}) {
  return new Promise((resolve, reject) => {
    const body = JSON.stringify(payload);
    const request = httpRequest({
      host: "127.0.0.1",
      port,
      path: "/rpc",
      method: "POST",
      headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body), ...headers },
    }, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve({ status: response.statusCode, body: JSON.parse(Buffer.concat(chunks)) }));
    });
    request.on("error", reject);
    request.end(body);
  });
}

test("loopback gateway accepts browser RPC without a token or console setup", async () => {
  const server = createGatewayServer({
    host: "127.0.0.1",
    token: "",
    daemon: { request: async (request) => ({ jsonrpc: "2.0", id: request.id, result: { ok: true } }) },
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const response = await postRpc(server.address().port, {
      jsonrpc: "2.0", id: "browser-id", method: "runtime.v1.health", params: {},
    });
    assert.equal(response.status, 200);
    assert.equal(response.body.result.ok, true);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("gateway bound to a non-loopback host rejects unauthenticated RPC despite spoofed Host", async () => {
  const server = createGatewayServer({
    host: "0.0.0.0",
    token: "",
    daemon: { request: async () => assert.fail("unauthorized RPC reached daemon") },
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const response = await postRpc(server.address().port, {
      jsonrpc: "2.0", id: "browser-id", method: "runtime.v1.health", params: {},
    }, { host: "localhost" });
    assert.equal(response.status, 401);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("unsaved provider discovery never reads an arbitrary environment secret", async () => {
  process.env.FNFYUH_TEST_SECRET = "must-not-leak";
  let authorization;
  const server = (await import("node:http")).createServer((request, response) => {
    authorization = request.headers.authorization;
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ data: [] }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    await listProviderModels({
      provider_id: "unsaved-provider",
      kind: "open_ai_compatible",
      endpoint: `http://127.0.0.1:${server.address().port}/v1`,
      api_key_env: "FNFYUH_TEST_SECRET",
    });
    assert.equal(authorization, undefined);
  } finally {
    delete process.env.FNFYUH_TEST_SECRET;
    await new Promise((resolve) => server.close(resolve));
  }
});

test("stored provider discovery cannot override its endpoint or secret reference", async () => {
  const { mkdtemp, writeFile, rm } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const { tmpdir } = await import("node:os");
  const directory = await mkdtemp(join(tmpdir(), "fnfyuh-provider-security-"));
  const storedServer = (await import("node:http")).createServer((request, response) => {
    assert.equal(request.headers.authorization, "Bearer stored-secret");
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ data: [{ id: "safe-model" }] }));
  });
  const attackerServer = (await import("node:http")).createServer((_request, response) => {
    response.statusCode = 500;
    response.end();
  });
  await Promise.all([
    new Promise((resolve) => storedServer.listen(0, "127.0.0.1", resolve)),
    new Promise((resolve) => attackerServer.listen(0, "127.0.0.1", resolve)),
  ]);
  const settings = join(directory, "settings.json");
  await writeFile(settings, JSON.stringify({ providers: [{
    id: "stored-provider", kind: "open_ai_compatible",
    endpoint: `http://127.0.0.1:${storedServer.address().port}/v1`, api_key_env: "STORED_SECRET",
  }] }));
  process.env.STORED_SECRET = "stored-secret";
  process.env.ATTACKER_SECRET = "attacker-secret";
  try {
    const result = await listProviderModels({
      provider_id: "stored-provider",
      endpoint: `http://127.0.0.1:${attackerServer.address().port}/steal`,
      api_key_env: "ATTACKER_SECRET",
    }, { settingsPath: settings });
    assert.deepEqual(result.models.map((model) => model.id), ["safe-model"]);
  } finally {
    delete process.env.STORED_SECRET;
    delete process.env.ATTACKER_SECRET;
    await Promise.all([
      new Promise((resolve) => storedServer.close(resolve)),
      new Promise((resolve) => attackerServer.close(resolve)),
    ]);
    await rm(directory, { recursive: true, force: true });
  }
});

test("provider discovery rejects non-HTTP endpoint protocols", async () => {
  await assert.rejects(
    () => listProviderModels({ provider_id: "unsafe", kind: "open_ai_compatible", endpoint: "file:///etc/passwd" }),
    /http or https/i,
  );
});

test("gateway restores browser ids while forwarding globally unique daemon ids", async () => {
  const forwarded = [];
  const server = createGatewayServer({
    host: "127.0.0.1",
    daemon: {
      request: async (request) => {
        forwarded.push(request);
        return { jsonrpc: "2.0", id: request.id, result: { ok: true } };
      },
    },
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const [first, second] = await Promise.all([
      postRpc(server.address().port, { jsonrpc: "2.0", id: 7, method: "one" }),
      postRpc(server.address().port, { jsonrpc: "2.0", id: 7, method: "two" }),
    ]);
    assert.notEqual(forwarded[0].id, forwarded[1].id);
    assert.match(String(forwarded[0].id), /^gateway-/);
    assert.equal(first.body.id, 7);
    assert.equal(second.body.id, 7);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
