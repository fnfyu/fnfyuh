import test from "node:test";
import assert from "node:assert/strict";
import { JsonRpcClient, createInMemoryTransport } from "../src/runtime.mjs";

test("session subscription exposes a backlog and filters live events by session", async () => {
  let emitLive;
  const transport = createInMemoryTransport(async (request, emit) => {
    emitLive = emit;
    return {
      jsonrpc: "2.0",
      id: request.id,
      result: {
        delivery: "committed-notifications",
        session_id: request.params.session_id,
        events: [{ session_id: "session-1", sequence: 1 }],
        next_global_sequence: 1,
        has_more: false,
      },
    };
  });
  const client = new JsonRpcClient(transport);
  const subscription = await client.subscribeSession({
    session_id: "session-1",
    after_global_sequence: 0,
  });
  assert.deepEqual(subscription.backlog, [{ session_id: "session-1", sequence: 1 }]);
  const received = [];
  subscription.onEvent((event) => received.push(event));
  emitLive({
    jsonrpc: "2.0",
    method: "session/event",
    params: { session_id: "other", sequence: 2 },
  });
  emitLive({
    jsonrpc: "2.0",
    method: "session/event",
    params: { session_id: "session-1", sequence: 3 },
  });
  assert.deepEqual(received, [{ session_id: "session-1", sequence: 3 }]);
});
