import { randomUUID } from "node:crypto";
import { JsonRpcClient, StdioJsonRpcTransport } from "../../sdk/src/runtime.mjs";

export class HarnessIdeClient {
  constructor(options = {}) {
    this.transport = options.transport ?? new StdioJsonRpcTransport(options.command ?? process.env.HARNESSD ?? "harnessd", [], {
      cwd: options.cwd ?? process.cwd(),
      env: options.env ?? process.env,
    });
    this.client = new JsonRpcClient(this.transport);
  }

  onEvent(listener) {
    return this.client.onEvent(listener);
  }

  createSession(workspaceRoot = process.cwd(), model = undefined) {
    return this.client.request("runtime.v1.session.create", {
      command_id: randomUUID(),
      workspace_roots: [workspaceRoot],
      ...(model ? { model } : {}),
    });
  }

  sendTurn(sessionId, content, model = undefined, parameters = undefined) {
    return this.client.request("runtime.v1.turn.start", {
      command_id: randomUUID(),
      session_id: sessionId,
      content,
      ...(model ? { model } : {}),
      ...(parameters ? { parameters } : {}),
    });
  }

  async subscribeSession(sessionId, afterGlobalSequence = 0) {
    const subscription = await this.client.subscribeSession({
      session_id: sessionId,
      after_global_sequence: afterGlobalSequence,
      limit: 100,
    });
    return {
      ...subscription,
      pull: (options = {}) => subscription.pull(options),
      ack: (cursor) => subscription.ack(cursor),
      nack: (messageId, error, retryAfterMs = 0) => subscription.nack(messageId, error, retryAfterMs),
    };
  }

  inspectOperation(sessionId, operationId) {
    return this.client.inspectOperation({ session_id: sessionId, operation_id: operationId });
  }

  continueOperation(sessionId, operationId, principal = "ide") {
    return this.client.continueOperation({
      command_id: randomUUID(),
      session_id: sessionId,
      operation_id: operationId,
      principal,
    });
  }

  close() {
    this.transport.close?.();
  }
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const client = new HarnessIdeClient();
  client.onEvent((event) => process.stderr.write(`[${event.global_sequence ?? "?"}] ${event.payload?.type ?? "event"}\n`));
  const [command, ...args] = process.argv.slice(2);
  try {
    if (command === "create") console.log(JSON.stringify(await client.createSession(args[0]), null, 2));
    else if (command === "turn" && args.length >= 2) console.log(JSON.stringify(await client.sendTurn(args[0], args.slice(1).join(" ")), null, 2));
    else if (command === "inspect" && args.length === 2) console.log(JSON.stringify(await client.inspectOperation(args[0], args[1]), null, 2));
    else throw new Error("Usage: node apps/ide/client.mjs create [workspace] | turn <session> <message> | inspect <session> <operation>");
  } finally {
    client.close();
  }
}
