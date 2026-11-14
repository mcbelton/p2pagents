# p2pagents

Peer-to-peer agents in Rust that run and delegate LLM coding tasks. Agents connect over
TCP, exchange newline-delimited JSON messages, discover each other by gossiping peer
lists, and advertise which LLM providers they serve. Any agent can run a task locally or
delegate it to a peer:

| Provider | SDK | How it is used |
| --- | --- | --- |
| `copilot` | [GitHub Copilot SDK for Rust](https://github.com/github/copilot-sdk/tree/main/rust) (`github-copilot-sdk` crate, official) | Native Rust, drives the Copilot CLI over JSON-RPC |
| `claude` | [Claude Agent SDK](https://github.com/anthropics/claude-agent-sdk-typescript) (`@anthropic-ai/claude-agent-sdk`, official) | Via the Node bridge in [bridge/](bridge/) |
| `codex` | [OpenAI Codex SDK](https://github.com/openai/codex/tree/main/sdk/typescript) (`@openai/codex-sdk`, official) | Via the Node bridge in [bridge/](bridge/) |

Claude and Codex publish official SDKs only for TypeScript and Python, so they are reached
through a small Node process ([bridge/index.mjs](bridge/index.mjs)) instead of unofficial
Rust ports.

**Every agent has its own provider instances and accounts.** An agent runs under a
*profile* (default: its `--name`) with private Copilot, Claude and Codex homes and its own
credentials, so different agents can use different GitHub, Anthropic and OpenAI accounts.
An agent never uses the machine's global Copilot/Claude/Codex sign-in. See
[Accounts and profiles](#accounts-and-profiles).

## Quick start (Windows, after setup)

Run these from the repo folder in any PowerShell window. The script reloads `PATH` itself,
so it also works in terminals opened before Rust was installed:

```powershell
cd D:\dev\p2pagents
.\scripts\p2p.ps1 doctor      # checks every prerequisite, prints the fix for anything missing
.\scripts\p2p.ps1 test        # automated tests (no LLM sign-in needed)
.\scripts\p2p.ps1 demo        # signs profile "worker" in if needed, opens "worker" and "boss" windows
.\scripts\p2p.ps1 live-test   # 3 agents, 2 profiles on 2 accounts, real Copilot delegation (needs Python)
```

In the **boss** window type `/peers`, then `/delegate copilot Summarize README.md in 3 bullets`.
The answer is printed in the boss window, and the worker window shows the task starting and
finishing. Type `/quit` in each window to exit.

`demo` signs the `worker` profile in to Copilot with your first GitHub CLI account. To pick
the account yourself: `.\scripts\p2p.ps1 agent auth login copilot --profile worker --gh-user <login>`.
To run any p2pagents command through the script: `.\scripts\p2p.ps1 agent <args>`, e.g.
`.\scripts\p2p.ps1 agent auth status`.
If PowerShell refuses to run the script, use
`powershell -ExecutionPolicy Bypass -File .\scripts\p2p.ps1 doctor`.

## Requirements

| Tool | Version | Needed for |
| --- | --- | --- |
| Rust toolchain (`rustc`, `cargo`) via rustup | **1.94+** (Copilot SDK minimum); tested on 1.100 | Build, run, test |
| `clippy`, `rustfmt` rustup components | matches toolchain | Lint and format |
| Linker toolchain: MinGW-w64 **or** MSVC Build Tools (Windows only) | any recent | Linking; the GNU target also needs `dlltool` |
| Node.js + npm | 18+; tested on 24 | `claude` and `codex` providers (bridge), bridge tests |
| GitHub Copilot CLI + a Copilot plan | recent; tested on 1.0.102 | `copilot` provider (each agent runs its own instance) |
| GitHub CLI (`gh`) | 2.40+ | Recommended: easiest way to give agents different GitHub accounts |
| `curl` | any (built into Windows 10+) | Shows which GitHub login a Copilot token belongs to |
| A Claude account or Anthropic API key per profile | | `claude` provider |
| A ChatGPT account or OpenAI API key per profile | | `codex` provider |
| Git | any | Source control |
| Python 3 | any | Only for `.\scripts\p2p.ps1 live-test` |

You only need the tools and credentials for the providers you enable. Rust crates
(`tokio`, `serde`, `clap`, `github-copilot-sdk`, ...) are downloaded by Cargo, and the
Claude and Codex CLIs are bundled inside their npm packages.

## Setup on Windows

### 1. Rust toolchain

Pick **one** option. Option A is lighter (no Visual Studio) and is what this project was
developed with.

#### Option A: GNU toolchain + MinGW-w64 (recommended, no Visual Studio)

1. Install Rust with the GNU host as default (PowerShell):

   ```powershell
   Invoke-WebRequest https://win.rustup.rs/x86_64 -OutFile "$env:TEMP\rustup-init.exe"
   & "$env:TEMP\rustup-init.exe" -y --default-host x86_64-pc-windows-gnu --default-toolchain stable --profile minimal
   Remove-Item "$env:TEMP\rustup-init.exe"
   ```

   This installs to `%USERPROFILE%\.cargo\bin` and adds it to your user `PATH`.

2. Add lint/format components:

   ```powershell
   rustup component add clippy rustfmt
   ```

3. Install MinGW-w64 (provides `dlltool`, `gcc`, `ld`):

   ```powershell
   winget install --id BrechtSanders.WinLibs.POSIX.UCRT -e
   ```

   winget adds `...\WinGet\Packages\BrechtSanders.WinLibs.POSIX.UCRT_*\mingw64\bin` to your
   user `PATH`.

#### Option B: MSVC toolchain

1. Install [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
   with the **Desktop development with C++** workload (MSVC + Windows SDK):

   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```

2. Install Rust with the default (MSVC) host, then add components:

   ```powershell
   winget install --id Rustlang.Rustup -e
   rustup default stable-x86_64-pc-windows-msvc
   rustup component add clippy rustfmt
   ```

### 2. Node.js (for `claude` / `codex`)

```powershell
winget install --id OpenJS.NodeJS.LTS -e
```

### 3. GitHub Copilot CLI and GitHub CLI (for `copilot`)

```powershell
winget install --id GitHub.Copilot -e
winget install --id GitHub.cli -e
gh auth login                       # once per GitHub account you want agents to use
gh auth login --hostname mycorp.ghe.com   # e.g. a GitHub Enterprise Cloud account
```

You don't need to sign in to the `copilot` CLI itself; each agent signs in its own instance
(see [Accounts and profiles](#accounts-and-profiles)). p2pagents finds `copilot` on `PATH`;
set `COPILOT_CLI_PATH` to use a different binary.

### 4. Open a new terminal

Open a new terminal (or restart VS Code) so the updated `PATH` is picked up.

## Setup on Linux / macOS

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup component add clippy rustfmt
```

You also need a C linker (`build-essential` on Debian/Ubuntu, `xcode-select --install` on
macOS), Node.js 18+, and the Copilot CLI (`npm install -g @github/copilot` or your package
manager). On Linux the Copilot SDK also needs OpenSSL 3 development files (`libssl-dev`).

## Install the bridge (Claude and Codex SDKs)

```powershell
cd bridge
npm install
```

## Accounts and profiles

Each agent runs under a **profile**: `--profile <name>`, default the agent's `--name`. A
profile is a private identity:

```text
%USERPROFILE%\.p2pagents\profiles\<profile>\     (or $P2PAGENTS_HOME/profiles/<profile>)
  profile.json   how each provider is signed in (no secrets)
  copilot\       COPILOT_HOME for this agent's own Copilot CLI instance
  claude\        CLAUDE_CONFIG_DIR for this agent's own Claude instance (+ its login)
  codex\         CODEX_HOME for this agent's own Codex instance (+ auth.json)
```

Tokens and API keys are stored in the OS credential store (Windows Credential Manager,
macOS Keychain, Linux Secret Service) under `p2pagents` / `<profile>/<provider>`.

**Isolation guarantees**

- Each agent starts its **own** Copilot CLI process and its own Node bridge (which runs its
  own Claude and Codex processes), each pointed at the profile's private home.
- Copilot is started with the profile's token and `--no-auto-login`, so it can't fall back to
  the machine's signed-in Copilot/`gh` user or to `GH_TOKEN`/`GITHUB_TOKEN`.
- Inherited `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `CODEX_API_KEY`, `GH_TOKEN`,
  `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `COPILOT_HOME` and similar variables are removed from
  every provider process.
- An agent whose profile isn't signed in to an enabled provider **refuses to start** and
  prints the `auth login` command to run.
- At startup each agent prints the account each provider uses, e.g.
  `copilot: alice@github.com [profile 'worker1']`.
- Peers that delegate tasks never see any credentials; the agent doing the work uses its own.

### Sign in

| Provider | Command | What it does |
| --- | --- | --- |
| `copilot` | `p2pagents auth login copilot --profile P --gh-user <login> [--host <ghe-host>]` | Uses that GitHub CLI account's token (fetched from `gh` each time the agent starts) |
| `copilot` | `p2pagents auth login copilot --profile P --token [--host <ghe-host>]` | Prompts for a GitHub token (fine-grained PAT with *Copilot Requests*, or a `gho_` OAuth token) and stores it in the OS credential store |
| `claude` | `p2pagents auth login claude --profile P` | Browser sign-in (`claude auth login`) into the profile's own Claude config dir |
| `claude` | `p2pagents auth login claude --profile P --api-key` | Prompts for an Anthropic API key |
| `claude` | `p2pagents auth login claude --profile P --token` | Prompts for a long-lived OAuth token from `claude setup-token` |
| `codex` | `p2pagents auth login codex --profile P` | Browser sign-in (`codex login`) into the profile's own `CODEX_HOME` |
| `codex` | `p2pagents auth login codex --profile P --api-key` | Prompts for an OpenAI API key (saved by `codex login --with-api-key`) |

Secrets are read from a hidden prompt, or from stdin when piped. Every login is verified
right away; if verification fails, that provider is signed out again.

```powershell
p2pagents auth status                    # every profile: which account each provider uses
p2pagents auth status --profile worker1
p2pagents auth logout codex --profile worker1
```

### Example: two workers on two accounts

```powershell
p2pagents auth login copilot --profile worker1 --gh-user alice
p2pagents auth login copilot --profile worker2 --gh-user alice-corp --host mycorp.ghe.com
p2pagents auth login claude  --profile worker2 --api-key

p2pagents --name worker1 --listen 127.0.0.1:7001 --providers copilot --serve
p2pagents --name worker2 --listen 127.0.0.1:7002 --providers copilot,claude --serve --peer 127.0.0.1:7001
```

Several agents can share one identity by passing the same `--profile`. Each still runs its
own provider processes.

## Verify the setup

```powershell
rustc --version          # 1.94 or newer
rustup show              # active toolchain, e.g. stable-x86_64-pc-windows-gnu
cargo clippy --version
dlltool --version        # Windows Option A only
node --version           # v18 or newer
copilot --version
cargo build              # first build downloads crates (several minutes)
cargo test
```

## Run

Two agents, one of which serves Copilot and Claude to its peers. Sign its profile in first
(see [Accounts and profiles](#accounts-and-profiles)):

```powershell
cargo run -- auth login copilot --profile worker --gh-user <login>
cargo run -- auth login claude --profile worker

# terminal 1: worker that runs tasks for peers, may edit files in D:\src\myrepo
cargo run -- --name worker --listen 127.0.0.1:7000 --providers copilot,claude --serve --workspace D:\src\myrepo --access workspace-write

# terminal 2: a peer that delegates work to it (needs no accounts)
cargo run -- --name boss --peer 127.0.0.1:7000
```

In terminal 2:

```text
/peers
  worker (127.0.0.1:7000) serves [Copilot, Claude]
/delegate copilot Add unit tests for the parser module and make sure they pass
/delegate claude@worker Review src/lib.rs for bugs
```

### Commands

| Command | Description |
| --- | --- |
| `<text>` | Broadcast a chat message |
| `/ask <provider> <prompt>` | Run a task locally (needs `--providers`) |
| `/delegate <provider>[@peer] <prompt>` | Run a task on a peer that serves `<provider>` (first match, or the named peer) |
| `/peers` | List peers and the providers they serve |
| `/help`, `/quit` | Help, exit |

Results print when they arrive, so you can keep typing while tasks run.

### Options

| Option | Default | Description |
| --- | --- | --- |
| `--name <NAME>` | required | Agent name shown to peers |
| `--profile <NAME>` | `--name` | Profile whose provider instances and accounts this agent uses |
| `--listen <ADDR>` | `127.0.0.1:0` (random port) | Listen address. Use a concrete IP (not `0.0.0.0`); it is advertised to peers |
| `--peer <ADDR>` | none | Peer to connect to on startup; repeatable |
| `--providers <LIST>` | none | Comma-separated `copilot,claude,codex` to enable on this agent |
| `--serve` | off | Run tasks delegated by peers with the enabled providers |
| `--workspace <DIR>` | `.` | Directory the LLM agents work in |
| `--access <MODE>` | `read-only` | `read-only` or `workspace-write` (see below) |
| `--bridge <PATH>` | `bridge/index.mjs` in this repo (env `P2PAGENTS_BRIDGE`) | Node bridge script |

Subcommands: `p2pagents auth login|status|logout` (see [Accounts and profiles](#accounts-and-profiles)).
`P2PAGENTS_HOME` moves the profiles folder.

For agents on different machines, listen on the machine's LAN IP, e.g. `--listen 192.168.1.10:7000`.

## Security model

- An agent runs peer tasks **only** with `--serve`, only for the providers it enabled, and
  at most 2 at a time (others queue).
- `--workspace` and `--access` are decided by the agent that runs the task. A peer cannot
  choose the directory or raise the access level.
- What each access level allows:

  | | `read-only` | `workspace-write` |
  | --- | --- | --- |
  | Copilot | approves `read` permission requests only | approves `read`, `write`, `shell`; rejects URL, MCP, memory, custom tools |
  | Claude | `dontAsk` mode, tools `Read`, `Glob`, `Grep`; user/project settings not loaded | adds `Edit`, `Write`, `NotebookEdit`, `Bash` |
  | Codex | sandbox `read-only`, approvals `never` | sandbox `workspace-write`, approvals `never` |

- Each agent uses only its own profile's accounts and provider instances (see
  [Isolation guarantees](#accounts-and-profiles)). Delegating peers never get credentials.
- Peer traffic is **unauthenticated and unencrypted** TCP. Only use `--serve` on trusted
  networks (or localhost), especially with `workspace-write`.

## Test

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

The tests use a fake executor and a fake bridge ([tests/fixtures/fake_bridge.mjs](tests/fixtures/fake_bridge.mjs)),
so they need Node.js but no LLM credentials.

## Troubleshooting

| Symptom | Fix |
| --- | --- |
| `cargo` / `rustc` is not recognized | The terminal was opened before Rust was installed. VS Code terminals keep VS Code's old `PATH`, so **fully quit and reopen VS Code** (a new terminal tab is not enough), or use `.\scripts\p2p.ps1`, which reloads `PATH` itself. |
| `error calling dlltool 'dlltool.exe': program not found` | GNU toolchain without MinGW-w64: do Option A step 3, then open a new terminal. |
| `linker 'link.exe' not found` | MSVC toolchain without Build Tools: do Option B step 1, or switch with `rustup default stable-x86_64-pc-windows-gnu` and use Option A. |
| `github-copilot-sdk` requires rustc 1.94 | `rustup update stable` |
| `Copilot CLI not found` | Install the Copilot CLI and open a new terminal, or set `COPILOT_CLI_PATH`. |
| `bridge script ... not found` / `Cannot find package '@anthropic-ai/claude-agent-sdk'` | Run `npm install` in `bridge\`, or pass `--bridge`. |
| `npm install` fails with `ERR_SSL_..._HANDSHAKE_FAILURE` | Your network blocks the public npm registry. Use your internal feed, e.g. `npm install --registry <feed-url>`. |
| `Not logged in · Please run /login` (claude) | Sign the agent's profile in: `p2pagents auth login claude --profile <profile>`. |
| `401 Unauthorized` (codex) | Sign the agent's profile in: `p2pagents auth login codex --profile <profile>`. |
| `<provider> is not signed in for profile '<p>'` | The agent's profile has no account for that provider; run the printed `auth login` command. Agents never borrow the machine's global sign-in. |
| `no GitHub CLI token for <user> on <host>` | Sign that account in to gh: `gh auth login --hostname <host>`, then check `gh auth status`. |
| `Copilot rejected this profile's token` | The token is invalid, expired, or the account has no Copilot access. Sign in again with `auth login copilot`. |
| Windows Firewall prompt on first run | Allow it if peers run on other machines; not needed for `127.0.0.1`. |

## Layout

- [src/protocol.rs](src/protocol.rs): wire messages (`hello` with capabilities, `peers`, `chat`, `task_request`, `task_result`)
- [src/agent.rs](src/agent.rs): `Agent` node: listener, dialing, handshake, gossip, broadcast, task delegation, shutdown
- [src/llm/mod.rs](src/llm/mod.rs): `Provider`, `Access`, `LlmTask`, the `Executor` trait and `SdkExecutor`
- [src/llm/copilot.rs](src/llm/copilot.rs): per-agent Copilot instance on the official Rust SDK (own token and `COPILOT_HOME`), with the access-based permission handler
- [src/profile.rs](src/profile.rs): profiles: private provider homes, OS credential store secrets, environment isolation
- [src/auth.rs](src/auth.rs): `auth login/status/logout` for Copilot, Claude and Codex
- [src/llm/bridge.rs](src/llm/bridge.rs): client for the Node bridge (multiplexed JSON lines over stdio)
- [bridge/index.mjs](bridge/index.mjs): Node bridge calling the Claude Agent SDK and Codex SDK
- [src/main.rs](src/main.rs): interactive CLI
- [scripts/p2p.ps1](scripts/p2p.ps1): doctor / build / test / demo / live-test helper
- [scripts/live_test.py](scripts/live_test.py): multi-process live P2P test: 2 profiles on 2 accounts, real Copilot
- [tests/](tests/): multi-agent delegation, bridge and credential-isolation tests
