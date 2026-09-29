#!/usr/bin/env node

import { randomUUID } from "node:crypto";
import {
  JsonRpcClient,
  StdioJsonRpcTransport,
} from "../../sdk/src/runtime.mjs";

function usage() {
  console.error(`Usage:
  pnpm cli health
  pnpm cli create [workspace-root ...]
  pnpm cli replay <session-id>
  pnpm cli fork <session-id> <sequence>
  pnpm cli inspect <session-id> <operation-id>
  pnpm cli continue <session-id> <operation-id>`);
}

const [command, ...args] = process.argv.slice(2);
if (!command || command === "help" || command === "--help") {
  usage();
  process.exit(command ? 0 : 1);
}

const transport = new StdioJsonRpcTransport(process.env.HARNESSD ?? "harnessd", [], {
  cwd: process.cwd(),
  env: process.env,
});
const client = new JsonRpcClient(transport);
client.onEvent((event) => {
  process.stderr.write(`[event ${event.sequence ?? "?"}] ${event.payload?.type ?? event.method ?? "unknown"}\n`);
});

try {
  let result;
  if (command === "health") {
    result = await client.request("runtime.v1.health");
  } else if (command === "create") {
    result = await client.request("runtime.v1.session.create", {
      command_id: randomUUID(),
      workspace_roots: args.length ? args : [process.cwd()],
    });
  } else if (command === "replay" && args.length === 1) {
    result = await client.request("runtime.v1.session.replay", { session_id: args[0] });
  } else if (command === "fork" && args.length === 2 && Number.isInteger(Number(args[1]))) {
    result = await client.request("runtime.v1.session.fork", {
      source_session_id: args[0],
      source_sequence: Number(args[1]),
    });
  } else if (command === "inspect" && args.length === 2) {
    result = await client.inspectOperation({ session_id: args[0], operation_id: args[1] });
  } else if (command === "continue" && args.length === 2) {
    result = await client.continueOperation({
      command_id: randomUUID(),
      session_id: args[0],
      operation_id: args[1],
      principal: "cli",
    });
  } else {
    usage();
    process.exitCode = 1;
    transport.close();
    process.exit();
  }
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
} finally {
  transport.close();
}
