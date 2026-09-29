import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

test("Codex login keeps a synchronous popup handle and exposes a link fallback", async () => {
  const source = await readFile(new URL("../../apps/web/app.mjs", import.meta.url), "utf8");
  const html = await readFile(new URL("../../apps/web/index.html", import.meta.url), "utf8");
  assert.match(source, /window\.open\("about:blank", "_blank"\)/);
  assert.match(source, /codex-auth-url/);
  assert.match(source, /startCodexLogin\("device_code"\)/);
  assert.match(source, /copyCodexDeviceCode/);
  assert.match(source, /state\.sessions\.length === 1/);
  assert.match(source, /暂无消息，输入即可开始/);
  assert.match(html, /id="codex-device-login-button"/);
  assert.match(html, /id="codex-copy-device-code"/);
  assert.doesNotMatch(source, /authWindow\.close\(\)/);
});
