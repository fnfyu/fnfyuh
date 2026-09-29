import test from "node:test";
import assert from "node:assert/strict";
import { JsonRpcClient, createInMemoryTransport } from "../src/runtime.mjs";

test("SDK exposes continuation, leased outbox, and artifact helpers", async () => {
  const methods = [];
  const transport = createInMemoryTransport(async (request) => {
    methods.push(request.method);
    if (request.method === "runtime.v1.events.subscribe") {
      return { jsonrpc: "2.0", id: request.id, result: {
        subscription_id: "sub-1",
        session_id: "session-1",
        deliveries: [{ message_id: 7, event: { event_id: "event-1", session_id: "session-1" }, delivery_attempts: 1 }],
      } };
    }
    return { jsonrpc: "2.0", id: request.id, result: { ok: true } };
  });
  const client = new JsonRpcClient(transport);
  const subscription = await client.subscribeSession({ session_id: "session-1" });
  assert.equal(subscription.backlog[0].event_id, "event-1");
  await subscription.pull();
  await subscription.ack(1);
  await subscription.nack(7, "editor failed", 10);
  await client.continueOperation({ session_id: "session-1", operation_id: "op-1" });
  await client.getArtifact({ session_id: "session-1", artifact_id: "artifact-1" });
  assert.deepEqual(methods, [
    "runtime.v1.events.subscribe",
    "runtime.v1.events.pull",
    "runtime.v1.events.ack",
    "runtime.v1.events.nack",
    "runtime.v1.session.operation.continue",
    "runtime.v1.artifact.get",
  ]);
});
