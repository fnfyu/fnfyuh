import test from "node:test";
import assert from "node:assert/strict";
import { JsonRpcClient, createInMemoryTransport } from "../src/runtime.mjs";

test("SDK exposes durable operation inspection without claiming backend state", async () => {
  const transport = createInMemoryTransport(async (request) => {
    assert.equal(request.method, "runtime.v1.session.operation.inspect");
    return {
      jsonrpc: "2.0",
      id: request.id,
      result: {
        operation_id: request.params.operation_id,
        external_backend_checked: false,
        operation: { status: "recovery_required" },
      },
    };
  });
  const client = new JsonRpcClient(transport);
  const result = await client.inspectOperation({
    session_id: "session-1",
    operation_id: "operation-1",
  });
  assert.equal(result.external_backend_checked, false);
  assert.equal(result.operation.status, "recovery_required");
});
