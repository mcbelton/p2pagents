// Bridge between p2pagents (Rust) and the official Claude Agent SDK and OpenAI Codex SDK.
//
// Protocol: one JSON object per line.
//   stdin : {"id":1,"provider":"claude"|"codex","prompt":"...","cwd":"...","access":"read-only"|"workspace-write","model":null}
//   stdout: {"id":1,"ok":true,"text":"..."} or {"id":1,"ok":false,"error":"..."}
// stdout is reserved for protocol lines; all logging goes to stderr.
import { createInterface } from "node:readline";

console.log = console.info = console.debug = (...args) => console.error(...args);

const CLAUDE_TOOLS = {
  "read-only": ["Read", "Glob", "Grep"],
  "workspace-write": ["Read", "Glob", "Grep", "Edit", "Write", "NotebookEdit", "Bash"],
};

async function runClaude({ prompt, cwd, model, access }) {
  const { query } = await import("@anthropic-ai/claude-agent-sdk");
  let result;
  for await (const message of query({
    prompt,
    options: {
      cwd,
      model: model ?? undefined,
      // Deny anything not pre-approved; no user/project settings can widen it.
      permissionMode: "dontAsk",
      allowedTools: CLAUDE_TOOLS[access],
      settingSources: [],
    },
  })) {
    if (message.type === "result") result = message;
  }
  if (!result) throw new Error("Claude returned no result");
  if (result.subtype !== "success" || result.is_error) {
    const detail = result.result ?? (result.errors ?? []).join("; ");
    throw new Error(`Claude ${result.subtype}: ${detail}`);
  }
  return result.result;
}

let codex;
async function runCodex({ prompt, cwd, model, access }) {
  const { Codex } = await import("@openai/codex-sdk");
  codex ??= new Codex({
    // Credentials live in this agent's own CODEX_HOME (set by p2pagents), never the OS keyring.
    config: { cli_auth_credentials_store: "file" },
  });
  const thread = codex.startThread({
    workingDirectory: cwd,
    model: model ?? undefined,
    sandboxMode: access,
    approvalPolicy: "never",
    skipGitRepoCheck: true,
  });
  const turn = await thread.run(prompt);
  return turn.finalResponse;
}

const PROVIDERS = { claude: runClaude, codex: runCodex };

function reply(message) {
  process.stdout.write(JSON.stringify(message) + "\n");
}

async function handle(line) {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    console.error(`bridge: ignoring malformed line: ${line}`);
    return;
  }
  const { id, provider, access } = request;
  try {
    const run = PROVIDERS[provider];
    if (!run) throw new Error(`unknown provider: ${provider}`);
    if (!CLAUDE_TOOLS[access]) throw new Error(`unknown access: ${access}`);
    reply({ id, ok: true, text: await run(request) });
  } catch (err) {
    reply({ id, ok: false, error: String(err?.message ?? err) });
  }
}

createInterface({ input: process.stdin }).on("line", (line) => {
  if (line.trim()) handle(line);
});
