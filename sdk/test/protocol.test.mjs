import test from "node:test";
import assert from "node:assert/strict";
import {
  JsonRpcClient,
  RpcError,
  createInMemoryTransport,
  requestDigest,
  stableModelHistory,
} from "../src/runtime.mjs";

function event(sequence, type, data, metadata = {}) {
  return {
    protocol: "local-first-harness",
    version: 1,
    schema_version: 1,
    event_id: `event-${sequence}`,
    session_id: "session-1",
    sequence,
    global_sequence: sequence,
    recorded_at_ms: 123456 + sequence,
    prev_hash: "ignored-for-history",
    hash: `hash-${sequence}`,
    ...metadata,
    payload: { type, data },
  };
}

test("model history ignores event ids, timestamps, and hashes", () => {
  const layers = {
    system_rules: "system",
    tool_schema: "tools",
    project_rules: "project",
    frozen_summaries: ["frozen"],
  };
  const first = stableModelHistory(
    [
      event(1, "UserMessage", { turn_id: "t", content: "hello" }),
      event(2, "ModelResponded", { turn_id: "t", content: "world" }),
    ],
    layers,
  );
  const second = stableModelHistory(
    [
      event(101, "UserMessage", { turn_id: "other", content: "hello" }, { event_id: "random" }),
      event(102, "ModelResponded", { turn_id: "other", content: "world" }, { recorded_at_ms: 999 }),
    ],
    layers,
  );
  assert.deepEqual(first.messages, second.messages);
  assert.equal(first.digest, second.digest);
});

test("compaction freezes a summary and removes old dynamic history", () => {
  const prompt = stableModelHistory([
    event(1, "UserMessage", { content: "old" }),
    event(2, "ContextCompacted", { summary: "frozen summary" }),
    event(3, "UserMessage", { content: "new" }),
  ]);
  assert.deepEqual(prompt.messages, [
    { role: "summary", content: "frozen summary" },
    { role: "user", content: "new" },
  ]);
});

test("canonical request digest is independent of object key order", () => {
  assert.equal(
    requestDigest({ b: 2, a: { d: false, c: true } }),
    requestDigest({ a: { c: true, d: false }, b: 2 }),
  );
});

test("JSON-RPC client preserves typed errors and receives committed events", async () => {
  let notify;
  let lastMethod;

  const transport = createInMemoryTransport(async (request, emit) => {
    lastMethod = request.method;
    notify = () => emit({ jsonrpc: "2.0", method: "session/event", params: { sequence: 1 } });
    if (request.method === "fail") {
      return { jsonrpc: "2.0", id: request.id, error: { code: -32004, message: "denied" } };
    }
    return { jsonrpc: "2.0", id: request.id, result: { accepted: true } };
  });
  const client = new JsonRpcClient(transport);
  const received = [];
  client.onEvent((event) => received.push(event));
  assert.deepEqual(await client.request("health"), { accepted: true });
  await client.subscribe({ session_id: "session-1" });
  assert.equal(lastMethod, "runtime.v1.events.subscribe");
  notify();
  assert.deepEqual(received, [{ sequence: 1 }]);
  await assert.rejects(() => client.request("fail"), (error) => {
    assert.ok(error instanceof RpcError);
    assert.equal(error.code, -32004);
    return true;
  });
});
