// Run with PI_PACKAGE_DIR pointing to the installed @earendil-works/pi-coding-agent directory.
import assert from "node:assert/strict";
import { execFile, execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { test } from "node:test";

const piPackage = process.env.PI_PACKAGE_DIR;
if (!piPackage) throw new Error("Set PI_PACKAGE_DIR to the installed Pi package directory");
const requirePi = createRequire(join(piPackage, "package.json"));
const { createJiti } = requirePi("jiti");
const jiti = createJiti(import.meta.url, { alias: {
  "typebox": requirePi.resolve("typebox"),
  "@earendil-works/pi-ai": resolve(piPackage, "../pi-ai/dist/index.js"),
}});
const extension = await jiti.import("./todo-focus.ts", { default: true });

test("create, reuse, display, pause and complete via the actual todo-tui binary", async () => {
  const dir = mkdtempSync(join(tmpdir(), "pi-todo-focus-"));
  const env = { ...process.env, TODO_TUI_DB: join(dir, "todos.sqlite") };
  delete process.env.PI_TODO_AGENT;
  const binary = process.env.PI_TODO_BIN || "todo-tui";
  const cli = (...args) => JSON.parse(execFileSync(binary, ["--json", ...args], { env, encoding: "utf8" }));
  const handlers = new Map();
  let tool;
  let command;
  let status;
  let notification;
  const ctx = {
    cwd: "/work/project",
    hasUI: true,
    sessionManager: { getSessionId: () => "123456789000" },
    ui: { setStatus: (_key, value) => { status = value; }, notify: (message) => { notification = message; } },
  };
  extension({
    on: (name, handler) => handlers.set(name, handler),
    registerTool: (definition) => { tool = definition; },
    registerCommand: (_name, definition) => { command = definition; },
    exec: (name, args, { signal, timeout, maxBuffer }) => new Promise((done) => {
      execFile(name, args, { env, signal, timeout, maxBuffer }, (err, stdout, stderr) => {
        done({ code: err ? 1 : 0, stdout, stderr });
      });
    }),
  });
  const execute = (params) => tool.execute("test", params, undefined, undefined, ctx);
  const agent = "Pi/project/12345678";
  try {
    await handlers.get("session_start")({}, ctx);
    assert.equal(status, undefined);
    const first = await execute({ action: "start", title: "Implement focus" });
    assert.equal(first.isError, undefined, first.content[0].text);
    const [id] = first.details.ids;
    assert.match(status, /Focus: project: Implement focus/);
    const duplicate = await execute({ action: "start", title: "Implement focus" });
    assert.deepEqual(duplicate.details.ids, [id]);
    assert.equal(cli("list").length, 1);
    assert.deepEqual(cli("pinned", "--agent", agent).map((task) => task.id), [id]);
    await command.handler("", ctx);
    assert.match(notification, new RegExp(id));
    assert.equal((await execute({ action: "pause", id })).isError, undefined);
    assert.equal(status, undefined);
    assert.equal(cli("show", id).completed, false);
    await execute({ action: "start", id });
    cli("pin", id, "--agent", "Another agent");
    assert.equal((await execute({ action: "finish" })).isError, true);
    cli("unpin", id, "--agent", "Another agent");
    assert.equal((await execute({ action: "finish", note: "Verified" })).isError, undefined);
    const done = cli("show", id);
    assert.equal(done.completed, true);
    assert.equal(done.completion_note, "Verified");
    assert.equal(status, undefined);
    cli("reopen", id);
    cli("pin", id, "--agent", agent);
    const deadline = Date.now() + 5000;
    while (!status && Date.now() < deadline) await new Promise((done) => setTimeout(done, 50));
    assert.match(status, /Implement focus/);
  } finally {
    handlers.get("session_shutdown")({}, ctx);
    rmSync(dir, { recursive: true, force: true });
  }
});
