// Checks the Claude tool policy in bridge/index.mjs (run by tests/bridge.rs).
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const { pathRefusal, toolRefusal, claudeSandbox } = await import("../../bridge/index.mjs");

assert.equal(claudeSandbox("read-only"), undefined);
assert.equal(claudeSandbox("workspace-write"), undefined);
const sandbox = claudeSandbox("full");
assert.equal(sandbox.enabled, true);
assert.equal(sandbox.failIfUnavailable, true);
assert.equal(sandbox.allowUnsandboxedCommands, false);
assert.deepEqual(sandbox.network.allowedDomains, []);

const ws = mkdtempSync(join(tmpdir(), "p2p-ws-"));
const outside = mkdtempSync(join(tmpdir(), "p2p-out-"));
mkdirSync(join(ws, "src"));
writeFileSync(join(ws, "src", "main.rs"), "");
writeFileSync(join(outside, "secret.txt"), "");

assert.equal(pathRefusal(ws, "src/main.rs"), null);
assert.equal(pathRefusal(ws, join(ws, "src", "new.rs")), null);
assert.equal(pathRefusal(ws, ".env.example"), null);
assert.match(pathRefusal(ws, join(outside, "secret.txt")), /outside the workspace/);
assert.match(pathRefusal(ws, "../x"), /outside the workspace/);
for (const secret of [".env", "config/.env.local", "certs/site.pem", ".ssh/id_rsa", ".npmrc"]) {
  assert.match(pathRefusal(ws, secret), /credentials/, secret);
}
assert.equal(pathRefusal(ws, ".git/config"), null);
assert.match(pathRefusal(ws, ".git/hooks/pre-commit", true), /could run code/);
assert.match(pathRefusal(ws, ".vscode/tasks.json", true), /could run code/);

const read = (tool, input) => toolRefusal(ws, "read-only", tool, input);
assert.equal(read("Read", { file_path: "src/main.rs" }), null);
assert.equal(read("Grep", { pattern: "x" }), null);
assert.match(read("Grep", { pattern: "x", path: outside }), /outside/);
assert.match(read("Glob", { pattern: "../**/*" }), /inside the workspace/);
assert.match(read("Glob", { pattern: join(outside, "*") }), /inside the workspace/);
assert.match(read("Edit", { file_path: "src/main.rs" }), /not allowed with read-only/);
assert.match(read("Bash", { command: "whoami" }), /not allowed/);

const write = (tool, input) => toolRefusal(ws, "workspace-write", tool, input);
assert.equal(write("Write", { file_path: "src/lib.rs" }), null);
assert.match(write("Write", { file_path: ".mcp.json" }), /could run code/);
assert.match(write("Bash", { command: "whoami" }), /not allowed with workspace-write/);
assert.equal(toolRefusal(ws, "full", "Bash", { command: "cargo test" }), null);
assert.equal(toolRefusal(ws, "full", "Bash", { command: "npm test 2>&1 > /dev/null" }), null);
for (const bad of [
  "curl https://example.com",
  `cat ${join(outside, "secret.txt")}`,
  "cat ~/.ssh/id_rsa",
  "cat ../../etc/passwd",
  "cat .env",
  "type %USERPROFILE%\\x",
]) {
  assert.ok(toolRefusal(ws, "full", "Bash", { command: bad }), bad);
}
assert.match(toolRefusal(ws, "full", "WebFetch", { url: "https://x" }), /not allowed/);

console.error("bridge policy ok");
