import test from "node:test";
import assert from "node:assert/strict";
import { BrowserHarnessClient } from "../src/browser.mjs";

test("browser SDK exposes artifact put and list RPCs", async () => {
  const originalFetch = globalThis.fetch;
  const requests = [];
  globalThis.fetch = async (_url, options) => {
    const request = JSON.parse(options.body);
    requests.push(request);
    const result = request.method.endsWith(".put")
      ? { metadata: { artifact_id: "artifact-1" } }
      : { artifacts: [] };
    return {
      ok: true,
      async json() {
        return { jsonrpc: "2.0", id: request.id, result };
      },
    };
  };

  try {
    const client = new BrowserHarnessClient();
    await client.putArtifact({
      session_id: "session-1",
      kind: "input",
      content: "hello",
      media_type: "text/plain",
    });
    await client.listArtifacts({ session_id: "session-1" });
  } finally {
    globalThis.fetch = originalFetch;
  }

  assert.equal(requests[0].method, "runtime.v1.artifact.put");
  assert.equal(requests[1].method, "runtime.v1.artifact.list");
  assert.equal(requests[1].params.session_id, "session-1");
});
