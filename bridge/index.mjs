// Bridge between p2pagents (Rust) and the official Claude Agent SDK and OpenAI Codex SDK.
//
// Protocol: one JSON object per line.
//   stdin : {"id":1,"provider":"claude"|"codex","prompt":"...","cwd":"...","access":"read-only"|"workspace-write"|"full","model":null,
//            "mcp":{"name":{"type":"stdio","command":"...","args":[],"env":{},"tools":[]}}}  (mcp optional)
//   stdout: {"id":1,"ok":true,"text":"..."} or {"id":1,"ok":false,"error":"..."}
// stdout is reserved for protocol lines; all logging goes to stderr.
import { existsSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { basename, extname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { createInterface } from "node:readline";

console.log = console.info = console.debug = (...args) => console.error(...args);

const READ = ["Read", "Glob", "Grep"];
const WRITE = ["Edit", "Write", "NotebookEdit"];
export const CLAUDE_TOOLS = {
  "read-only": READ,
  "workspace-write": [...READ, ...WRITE],
  full: [...READ, ...WRITE, "Bash"],
};
// Codex runs commands in its own sandbox; "full" never means danger-full-access.
const CODEX_SANDBOX = {
  "read-only": "read-only",
  "workspace-write": "workspace-write",
  full: "workspace-write",
};

// Same rules as src/llm/policy.rs.
const SENSITIVE_DIRS = [".ssh", ".gnupg", ".aws", ".azure", ".kube", ".docker", ".p2pagents", ".copilot"];
const SENSITIVE_FILES = [
  ".env", ".envrc", ".npmrc", ".yarnrc.yml", ".pypirc", ".netrc", "_netrc", ".pgpass",
  ".git-credentials", ".htpasswd", "credentials", "credentials.json", "secrets.json",
  "secrets.yaml", "secrets.yml", "secrets.toml",
];
const SENSITIVE_EXTENSIONS = ["pem", "key", "p12", "pfx", "jks", "keystore", "kdbx", "ppk", "gpg", "asc"];
const PROTECTED_DIRS = [".git", ".vscode", ".idea", ".claude", ".codex", ".copilot", ".cursor"];
const PROTECTED_FILES = [".mcp.json", ".gitmodules"];
const TEMPLATE_SUFFIXES = [".example", ".sample", ".template", ".dist"];

/** Resolves symlinks on the existing part of `path`. */
function real(path) {
  const missing = [];
  let current = path;
  for (;;) {
    try {
      return resolve(realpathSync.native(current), ...missing.reverse());
    } catch {
      const parent = resolve(current, "..");
      if (parent === current) return path;
      missing.push(basename(current));
      current = parent;
    }
  }
}

function sensitive(parts) {
  const names = parts.map((p) => p.toLowerCase());
  if (names.some((n) => SENSITIVE_DIRS.includes(n) || n.startsWith("id_rsa"))) return true;
  const file = names.at(-1) ?? "";
  if (SENSITIVE_FILES.includes(file)) return true;
  if (file.startsWith(".env.") && !TEMPLATE_SUFFIXES.some((s) => file.endsWith(s))) return true;
  if (["id_dsa", "id_ecdsa", "id_ed25519"].some((k) => file.startsWith(k))) return true;
  return SENSITIVE_EXTENSIONS.includes(extname(file).slice(1));
}

function protectedPath(parts) {
  const names = parts.map((p) => p.toLowerCase());
  return names.some((n) => PROTECTED_DIRS.includes(n))
    || PROTECTED_FILES.includes(names.at(-1) ?? "")
    || (names[0] === ".github" && names[1] === "hooks");
}

/** Why `path` may not be read (or written) in workspace `cwd`, or null. */
export function pathRefusal(cwd, path, write = false) {
  if (typeof path !== "string" || !path) return "missing path";
  const root = real(resolve(cwd));
  const target = real(resolve(root, path));
  const rel = relative(root, target);
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    return `${path} is outside the workspace ${root}`;
  }
  const parts = rel === "" ? [] : rel.split(sep);
  if (sensitive(parts)) return `${path} may contain credentials`;
  if (write && protectedPath(parts)) return `${path} is tool configuration that could run code; edit it yourself`;
  return null;
}

const HOME_REFS = [
  "$home", "${home}", "$env:userprofile", "$env:home", "$env:appdata", "$env:localappdata",
  "%userprofile%", "%appdata%", "%localappdata%", "~/", "~\\",
];
const NULL_DEVICES = ["/dev/null", "/dev/stdout", "/dev/stderr", "nul"];

/** Best-effort scan of a shell command (same rules as Workspace::check_command); not a sandbox. */
export function commandRefusal(cwd, text) {
  if (typeof text !== "string" || !text) return "missing command";
  const lower = text.toLowerCase();
  if (lower.includes("://")) return "shell commands may not access URLs";
  if (HOME_REFS.some((r) => lower.includes(r))) return "shell commands may not reference the home folder";
  for (const token of text.split(/[\s'"`;|&()<>,={}]+/)) {
    if (!token || token.startsWith("-") || NULL_DEVICES.includes(token)) continue;
    if (process.platform === "win32" && token.startsWith("/")) continue;
    if (token === "~") return "shell commands may not reference the home folder";
    const reason = pathRefusal(cwd, token);
    if (reason) return reason;
  }
  return null;
}

/** Why Claude may not run `tool` with `input` at `access`, or null. `mcp`: the task's MCP servers. */
export function toolRefusal(cwd, access, tool, input = {}, mcp = {}) {
  if (typeof tool === "string" && tool.startsWith("mcp__")) return mcpRefusal(mcp, tool);
  if (!CLAUDE_TOOLS[access]?.includes(tool)) return `${tool} is not allowed with ${access} access`;
  switch (tool) {
    case "Read":
      return pathRefusal(cwd, input.file_path);
    case "Glob":
    case "Grep": {
      const pattern = tool === "Glob" ? input.pattern : input.glob;
      if (typeof pattern === "string" && (isAbsolute(pattern) || pattern.split(/[\\/]/).includes(".."))) {
        return "search patterns must stay inside the workspace";
      }
      return input.path ? pathRefusal(cwd, input.path) : null;
    }
    case "Edit":
    case "Write":
      return pathRefusal(cwd, input.file_path, true);
    case "NotebookEdit":
      return pathRefusal(cwd, input.notebook_path, true);
    case "Bash":
      return commandRefusal(cwd, input.command);
    default:
      return null;
  }
}

// Same list as HOME_SECRETS in src/llm/policy.rs.
const HOME_SECRETS = [
  ".ssh", ".aws", ".azure", ".gnupg", ".kube", ".docker", ".p2pagents", ".copilot",
  ".config/gh", ".git-credentials", ".netrc", ".npmrc", ".pypirc",
];

/** How Claude names MCP servers and tools in `mcp__<server>__<tool>`. */
const mcpName = (name) => String(name).replace(/[^a-zA-Z0-9_-]/g, "_");

/**
 * The server and tool of Claude tool name `mcp__<server>__<tool>`. p2pagents' server
 * names have no `__` and don't start or end with `_`, so the first `__` after
 * `mcp__` ends the server's name.
 */
function mcpParts(tool) {
  if (typeof tool !== "string" || !tool.startsWith("mcp__")) return null;
  const rest = tool.slice(5);
  const end = rest.indexOf("__");
  if (end <= 0) return null;
  return { server: rest.slice(0, end), tool: rest.slice(end + 2) };
}

/** Why Claude may not call MCP tool `tool` (`mcp__<server>__<tool>`), or null. */
export function mcpRefusal(mcp, tool) {
  const parts = mcpParts(tool);
  const entry = parts && Object.entries(mcp ?? {}).find(([name]) => mcpName(name) === parts.server);
  if (!entry) return `${tool} is not from an MCP server this task may use`;
  const [name, server] = entry;
  const allowed = server.tools ?? [];
  if (allowed.length === 0 || allowed.some((t) => mcpName(t) === parts.tool)) return null;
  return `${tool} is not one of the tools the owner allows on MCP server ${name}`;
}

// Same envelope as src/boundary.rs: MCP results are external content.
const TAG = "external_content";

/** `body` in an `<external_content source="...">` envelope it can't close or reopen. */
export function externalContent(source, body) {
  const label = String(source).replace(/[^\p{L}\p{N} :._\-/@]/gu, "_").slice(0, 120);
  const text = String(body)
    .replace(/<\s*\/\s*external_content\b[^>]*>/gi, `<\\/${TAG}>`)
    .replace(/<\s*external_content\b/gi, `<\\${TAG}`);
  return `<${TAG} source="${label}">\n${text.trimEnd()}\n</${TAG}>`;
}

/** A tool's output with every text the model reads in an envelope. */
export function wrapToolOutput(source, output) {
  if (typeof output === "string") return externalContent(source, output);
  if (Array.isArray(output)) {
    return output.map((item) => {
      if (typeof item === "string") return externalContent(source, item);
      if (item && typeof item === "object" && typeof item.text === "string") {
        return { ...item, text: externalContent(source, item.text) };
      }
      return item;
    });
  }
  if (output && typeof output === "object") {
    if (Array.isArray(output.content)) return { ...output, content: wrapToolOutput(source, output.content) };
    if (typeof output.text === "string") return { ...output, text: externalContent(source, output.text) };
  }
  return externalContent(source, JSON.stringify(output ?? null));
}

/** The task's MCP servers in the Claude Agent SDK's form, and the tools to pre-approve. */
export function claudeMcp(mcp) {
  const servers = {};
  const allowed = [];
  for (const [name, server] of Object.entries(mcp ?? {})) {
    const { tools = [], ...config } = server;
    // Tool search isn't pre-approved, so the server's tools must be loaded up front.
    servers[name] = { ...config, alwaysLoad: true };
    if (tools.length === 0) allowed.push(`mcp__${mcpName(name)}`);
    else allowed.push(...tools.map((t) => `mcp__${mcpName(name)}__${mcpName(t)}`));
  }
  return { servers, allowed };
}

/**
 * Claude's OS sandbox for Bash at full access: no network, no unsandboxed escape,
 * credential folders unreadable, and the task fails if the sandbox can't start.
 */
export function claudeSandbox(access) {
  if (access !== "full") return undefined;
  const home = homedir();
  const denyRead = HOME_SECRETS.map((rel) => join(home, rel)).filter((p) => existsSync(p));
  if (process.env.P2PAGENTS_HOME && existsSync(process.env.P2PAGENTS_HOME)) {
    denyRead.push(process.env.P2PAGENTS_HOME);
  }
  return {
    enabled: true,
    failIfUnavailable: true,
    allowUnsandboxedCommands: false,
    autoAllowBashIfSandboxed: true,
    network: { allowedDomains: [] },
    filesystem: { denyRead },
  };
}

async function runClaude({ prompt, cwd, model, access, mcp }, abortController) {
  const { query } = await import("@anthropic-ai/claude-agent-sdk");
  const servers = claudeMcp(mcp);
  const guard = async (input) => {
    const reason = toolRefusal(cwd, access, input.tool_name, input.tool_input, mcp);
    if (!reason) return {};
    console.error(`  claude: denied ${input.tool_name} (${reason})`);
    return {
      hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: `denied by p2pagents: ${reason}`,
      },
    };
  };
  // MCP results come from outside the task: the model gets them in an envelope.
  const spotlight = async (input) => {
    const parts = mcpParts(input.tool_name);
    if (!parts) return {};
    return {
      hookSpecificOutput: {
        hookEventName: "PostToolUse",
        updatedToolOutput: wrapToolOutput(`mcp:${parts.server}/${parts.tool}`, input.tool_response),
      },
    };
  };
  let result;
  for await (const message of query({
    prompt,
    options: {
      cwd,
      model: model ?? undefined,
      // Deny anything not pre-approved; no user/project settings can widen it.
      permissionMode: "dontAsk",
      allowedTools: [...CLAUDE_TOOLS[access], ...servers.allowed],
      disallowedTools: ["WebFetch", "WebSearch", "Task"],
      settingSources: [],
      // Only the owner's servers for this task; never a project's .mcp.json.
      mcpServers: servers.servers,
      strictMcpConfig: true,
      sandbox: claudeSandbox(access),
      abortController,
      // Checks every tool call's paths, including pre-approved tools.
      hooks: { PreToolUse: [{ hooks: [guard] }], PostToolUse: [{ hooks: [spotlight] }] },
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
async function runCodex({ prompt, cwd, model, access, mcp }, abortController) {
  if (Object.keys(mcp ?? {}).length > 0) throw new Error("Codex tasks can't use MCP servers here");
  const { Codex } = await import("@openai/codex-sdk");
  codex ??= new Codex({
    // Credentials live in this agent's own CODEX_HOME (set by p2pagents), never the OS keyring.
    config: { cli_auth_credentials_store: "file" },
  });
  const thread = codex.startThread({
    workingDirectory: cwd,
    model: model ?? undefined,
    sandboxMode: CODEX_SANDBOX[access],
    approvalPolicy: "never",
    networkAccessEnabled: false,
    webSearchMode: "disabled",
    skipGitRepoCheck: true,
  });
  const turn = await thread.run(prompt, { signal: abortController.signal });
  return turn.finalResponse;
}

const PROVIDERS = { claude: runClaude, codex: runCodex };
/** Abort controllers of running requests, by id. */
const running = new Map();

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
  if (request.cancel !== undefined) {
    // {"cancel": id}: the agent dropped the task; stop the model run.
    running.get(request.cancel)?.abort();
    return;
  }
  const { id, provider, access } = request;
  const controller = new AbortController();
  running.set(id, controller);
  try {
    const run = PROVIDERS[provider];
    if (!run) throw new Error(`unknown provider: ${provider}`);
    if (!CLAUDE_TOOLS[access]) throw new Error(`unknown access: ${access}`);
    reply({ id, ok: true, text: await run(request, controller) });
  } catch (err) {
    reply({ id, ok: false, error: controller.signal.aborted ? "cancelled" : String(err?.message ?? err) });
  } finally {
    running.delete(id);
  }
}

// Tests import this module for its policy functions and set P2PAGENTS_BRIDGE_IMPORT_ONLY.
if (!process.env.P2PAGENTS_BRIDGE_IMPORT_ONLY) {
  createInterface({ input: process.stdin }).on("line", (line) => {
    if (line.trim()) handle(line);
  });
}
