// Stand-in for bridge/index.mjs speaking the same JSON-lines protocol, so the
// Rust bridge client can be tested without Claude/Codex credentials.
import { createInterface } from "node:readline";

createInterface({ input: process.stdin }).on("line", (line) => {
  const { id, provider, prompt, access, cwd } = JSON.parse(line);
  if (prompt === "exit") process.exit(0);
  if (prompt.startsWith("env:")) {
    const names = prompt.slice(4).split(",");
    const text = names.map((n) => `${n}=${process.env[n] ?? "<unset>"}`).join(";");
    process.stdout.write(JSON.stringify({ id, ok: true, text }) + "\n");
    return;
  }
  const delay = prompt === "slow" ? 200 : 0;
  setTimeout(() => {
    const reply = prompt === "fail"
      ? { id, ok: false, error: "boom" }
      : { id, ok: true, text: `${provider}|${access}|${cwd.length > 0}|${prompt}` };
    process.stdout.write(JSON.stringify(reply) + "\n");
  }, delay);
});
