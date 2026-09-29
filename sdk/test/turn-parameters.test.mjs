import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

test("Workbench exposes persistent per-turn model parameters", async () => {
  const source = await readFile(new URL("../../apps/web/app.mjs", import.meta.url), "utf8");
  const html = await readFile(new URL("../../apps/web/index.html", import.meta.url), "utf8");
  assert.match(html, /id="thinking-effort"/);
  assert.match(html, /id="temperature"/);
  assert.match(html, /id="max-output-tokens"/);
  assert.match(source, /TURN_PARAMETERS_KEY/);
  assert.match(source, /parameters: turnParametersPayload\(\)/);
  assert.match(source, /persistTurnParameters\(next\)/);
});
