# p2pagents

Peer-to-peer agents in Rust that run and delegate LLM coding tasks **over the internet**.
Agents connect with [iroh](https://iroh.computer) (encrypted QUIC, NAT hole punching, relay
fallback), are addressed by public key instead of IP, only talk to peers on their
allowlist, discover each other by gossip, and advertise which LLM providers they serve.
Any agent can run a task locally or delegate it to a peer:

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
[Accounts and profiles](#accounts-and-profiles) and [Internet P2P](#internet-p2p).

## Install (no source code or Rust needed)

Prebuilt binaries for Windows x64, Linux x64 and macOS (Apple Silicon) are published as
[GitHub releases](https://github.com/mcbelton/p2pagents/releases). Everything an agent
needs at runtime is in the one `p2pagents` executable, except the provider CLIs below.

**1. Install p2pagents**

```powershell
# Windows (the repo is private: you need read access and the GitHub CLI signed in to that account)
gh release download -R mcbelton/p2pagents -p install.ps1 -O install.ps1; .\install.ps1
```

```sh
# Linux / macOS
gh release download -R mcbelton/p2pagents -p install.sh -O - | sh
```

The installer puts `p2pagents` in `%LOCALAPPDATA%\p2pagents\bin` (Windows) or
`~/.local/bin`, and adds it to `PATH`.

> **"release not found"?** The repo is private, and GitHub shows private repos as "not
> found" to accounts without access. If `gh` has several accounts, the active one may be
> wrong (check `gh auth status`). Download with the account that has access:
>
> ```powershell
> $env:GH_TOKEN = gh auth token --user <login-with-access>
> gh release download -R mcbelton/p2pagents -p install.ps1 -O install.ps1 --clobber
> Remove-Item Env:GH_TOKEN
> .\install.ps1 -GhUser <login-with-access>      # Linux/macOS: P2PAGENTS_GH_USER=<login> sh install.sh
> ```
>
> Otherwise ask the owner to add your account as a collaborator. In a browser, sign in to
> github.com with that account to see the releases page.

If the repo is made public, no GitHub CLI is needed:
`irm https://github.com/mcbelton/p2pagents/releases/latest/download/install.ps1 | iex`
(or `curl -fsSL .../install.sh | sh`). You can also download the archive for your platform
from the release page and put `p2pagents` anywhere on `PATH`.

**2. Install what your agent uses**

| You want to | Install |
| --- | --- |
| Run Copilot tasks | Copilot CLI: `winget install GitHub.Copilot` (or `npm install -g @github/copilot`) and a GitHub account with Copilot |
| Sign agents in to GitHub accounts easily | GitHub CLI: `winget install GitHub.cli`, then `gh auth login` per account |
| Run Claude or Codex tasks | Node.js 18+, then `p2pagents setup` (installs the Claude Agent SDK and Codex SDK into `~/.p2pagents/bridge`; add `--registry <mirror>` if public npm is blocked) |
| Only delegate tasks to others | Nothing else |

**3. Check, sign in, connect**

```powershell
p2pagents doctor                                         # checks CLIs, bridge, internet relay, profiles
p2pagents auth login copilot --profile me --gh-user <github-login>
p2pagents peers id --profile me                          # send your ticket to your peers
p2pagents peers allow <their-ticket> --profile me [--delegate]
p2pagents --name me --providers copilot --serve
```

See [Internet P2P](#internet-p2p) for how peers find each other and
[Accounts and profiles](#accounts-and-profiles) for sign-in options. Building from source
is described in the rest of this README.

## Quick start (Windows, after setup)

Run these from the repo folder in any PowerShell window. The script reloads `PATH` itself,
so it also works in terminals opened before Rust was installed:

```powershell
cd D:\dev\p2pagents
.\scripts\p2p.ps1 doctor      # checks every prerequisite, prints the fix for anything missing
.\scripts\p2p.ps1 test        # automated tests (no LLM sign-in needed)
.\scripts\p2p.ps1 demo        # worker + boss windows, allowlisted to each other, connected over the internet
.\scripts\p2p.ps1 live-test   # 4 agents over the internet, 2 accounts, allowlist + real Copilot delegation (needs Python)
```

In the **boss** window wait for `+ worker`, then type `/peers` and
`/delegate copilot Summarize README.md in 3 bullets`.
The answer is printed in the boss window, and the worker window shows the task starting and
finishing. Type `/quit` in each window to exit.

`demo` signs the `worker` profile in to Copilot with your first GitHub CLI account and
allowlists `worker` and `boss` to each other. To pick the account yourself:
`.\scripts\p2p.ps1 agent auth login copilot --profile worker --gh-user <login>`.
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
| Internet access: outbound UDP, or at least HTTPS (443) | | Internet mode: direct QUIC when UDP works, otherwise via the iroh relays over HTTPS |

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

(From a source checkout the bridge is used in place. Installed binaries use
`p2pagents setup` instead, which writes the same bridge to `~/.p2pagents/bridge`.)

## Releasing

Push a version tag; GitHub Actions ([.github/workflows/release.yml](.github/workflows/release.yml))
builds Windows, Linux and macOS binaries and publishes them with the install scripts:

```powershell
# bump version in Cargo.toml first
git tag v0.1.1; git push origin v0.1.1
```

[.github/workflows/ci.yml](.github/workflows/ci.yml) runs fmt, clippy and the tests on every push.

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
macOS Keychain, Linux Secret Service) under `p2pagents` / `<profile>/<provider>`, next to
the profile's identity key (`<profile>/identity`). The allowlist is `<profile>\peers.json`.

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
# allowlist them to each other (see Internet P2P), then:
p2pagents --name worker1 --providers copilot --serve
p2pagents --name worker2 --providers copilot,claude --serve
```

Several agents can share one identity by passing the same `--profile`. Each still runs its
own provider processes.

`p2pagents profile delete --profile P` removes a profile: its identity key, stored
credentials, allowlist and provider sign-ins.

## Internet P2P

Agents connect with [iroh](https://iroh.computer):

- **Identity:** each profile has an ed25519 key, created on first use and kept in the OS
  credential store. Its public half, the **endpoint id**, is the agent's address. QUIC/TLS
  proves every connection really comes from that key.
- **Reachability:** agents behind home or office NAT connect directly using UDP hole
  punching; if that fails, traffic goes through public iroh relays over HTTPS (still
  end-to-end encrypted; the relay can't read it). Agents are found by endpoint id through
  iroh's DNS address lookup, so nobody needs a public IP or open ports.
- **Allowlist (mutual):** an agent only accepts connections from, and only dials, keys on
  its profile's allowlist (`<profile>\peers.json`). Unknown keys are refused
  (`! refused connection from <id>`). Both sides must allowlist each other.
- **Delegation rights:** a peer may make your agent run LLM tasks (on your accounts) only if
  you allowed it with `--delegate`, and at most `--tasks-per-hour` (default 20) per hour.
- **Discovery:** connected agents share each other's addresses; an agent auto-dials every
  allowlisted peer at startup and every 30 seconds, so the mesh reforms after restarts.

### Connect two machines

```powershell
# Alice (machine A), runs a worker that serves Copilot
p2pagents auth login copilot --profile alice --gh-user alice
p2pagents peers id --profile alice          # prints alice's endpoint id + ticket; send it to Bob

# Bob (machine B, anywhere on the internet)
p2pagents peers id --profile bob            # send bob's ticket to Alice
p2pagents peers allow <alice-ticket> --profile bob

# Alice allowlists Bob and lets him delegate up to 10 tasks per hour
p2pagents peers allow <bob-ticket> --profile alice --delegate --tasks-per-hour 10

# start both; they find each other by key, no IPs or --peer needed
p2pagents --name alice --providers copilot --serve        # machine A
p2pagents --name bob                                      # machine B
```

Then on Bob's agent: `/delegate copilot@alice <prompt>`.

A ticket (`p2pa1...`) contains the endpoint id, a suggested name and known addresses. A
bare endpoint id works too (add `--name`). A running agent prints its ticket at startup and
with `/ticket`.

| Peers command | Description |
| --- | --- |
| `p2pagents peers id --profile P` | Show P's endpoint id and ticket |
| `p2pagents peers allow <ticket-or-id> --profile P [--name N] [--delegate] [--tasks-per-hour N]` | Allowlist a peer (re-run to change its rights) |
| `p2pagents peers list --profile P` | List allowlisted peers and their rights |
| `p2pagents peers remove <name-or-id> --profile P` | Remove a peer |

**LAN or VPN without internet services:** `--network local --listen <IP:PORT>` uses direct
IP connections only (no relays, no DNS lookup); exchange tickets from running agents
(`/ticket`), since they contain the IP addresses. The allowlist works the same.

Internet mode uses the public relay and DNS lookup services run by n0 (iroh's maintainers)
for connection setup. They see endpoint ids and IP addresses, never message contents.

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

Two agents, one of which serves Copilot and Claude to its peers. Sign its profile in and
allowlist the two profiles to each other first (see [Accounts and profiles](#accounts-and-profiles)
and [Internet P2P](#internet-p2p)):

```powershell
cargo run -- auth login copilot --profile worker --gh-user <login>
cargo run -- auth login claude --profile worker
cargo run -- peers id --profile boss                              # copy boss's ticket
cargo run -- peers allow <boss-ticket> --profile worker --delegate
cargo run -- peers id --profile worker                            # copy worker's ticket
cargo run -- peers allow <worker-ticket> --profile boss

# terminal 1 (any machine): worker that runs tasks for boss, may edit files in D:\src\myrepo
cargo run -- --name worker --providers copilot,claude --serve --workspace D:\src\myrepo --access workspace-write

# terminal 2 (any machine): a peer that delegates work to it (needs no LLM accounts)
cargo run -- --name boss
```

In terminal 2, once `+ worker` appears:

```text
/peers
  worker (ee5c95828c) serves [Copilot, Claude]
/delegate copilot Add unit tests for the parser module and make sure they pass
/delegate claude@worker Review src/lib.rs for bugs
```

### Commands

| Command | Description |
| --- | --- |
| `<text>` | Broadcast a chat message |
| `/ask <provider> <prompt>` | Run a task locally (needs `--providers`) |
| `/delegate <provider>[@peer] <prompt>` | Run a task on a peer that serves `<provider>` (first match, or the named peer) |
| `/peers` | List connected peers and the providers they serve |
| `/ticket` | Print this agent's ticket (endpoint id + current addresses) |
| `/help`, `/quit` | Help, exit |

Results print when they arrive, so you can keep typing while tasks run.

### Options

| Option | Default | Description |
| --- | --- | --- |
| `--name <NAME>` | required | Agent name shown to peers |
| `--profile <NAME>` | `--name` | Profile whose identity, allowlist and provider accounts this agent uses |
| `--network <MODE>` | `internet` | `internet` (relays, NAT traversal, lookup by id) or `local` (direct IPs only) |
| `--listen <ADDR>` | any port | UDP address to bind; required with `--network local` (use a concrete IP) |
| `--peer <TICKET-OR-ID>` | none | Allowlisted peer to dial at startup; repeatable. Allowlisted peers are also dialed automatically |
| `--providers <LIST>` | none | Comma-separated `copilot,claude,codex` to enable on this agent |
| `--serve` | off | Run tasks that allowlisted peers with delegation rights send |
| `--workspace <DIR>` | `.` | Directory the LLM agents work in |
| `--access <MODE>` | `read-only` | `read-only` or `workspace-write` (see below) |
| `--bridge <PATH>` | `bridge/index.mjs` in this repo (env `P2PAGENTS_BRIDGE`) | Node bridge script |

Subcommands: `p2pagents auth login|status|logout`, `p2pagents peers id|allow|list|remove`,
`p2pagents profile delete`. `P2PAGENTS_HOME` moves the profiles folder (and uses a separate
credential store namespace).

## Security model

- Connections are encrypted and authenticated (QUIC/TLS with each agent's ed25519 key).
  Only allowlisted keys can connect; everyone else is refused before any message is read.
- An agent runs peer tasks **only** with `--serve`, only for the providers it enabled, only
  from peers allowlisted with `--delegate`, within their hourly limit, and at most 2 at a
  time (others queue).
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
- Prompts from a peer you allow to delegate run with your accounts and in your workspace.
  Only grant `--delegate` (and especially `--access workspace-write`) to people you trust.

## Test

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

The tests use a fake executor and a fake bridge ([tests/fixtures/fake_bridge.mjs](tests/fixtures/fake_bridge.mjs)),
so they need Node.js but no LLM credentials. The multi-agent tests run real iroh QUIC
endpoints on localhost in local mode, so they need no internet.
`.\scripts\p2p.ps1 live-test` runs the end-to-end test over the internet.

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
| Windows Firewall prompt on first run | Allow it for direct (faster) connections; if blocked, agents still connect through the relays. |
| `! refused connection from <id>` | That key is not on this profile's allowlist. If it should be: `p2pagents peers allow <id> --profile <p> --name <n>`. |
| `<id> is not on the allowlist` when dialing | Allowlist the peer first; agents only dial allowlisted keys. |
| Peers never connect (`+ name` doesn't appear) | Both sides must allowlist each other; check `peers list` on both. Check `warning: no relay reachable` at startup: the network must allow outbound HTTPS to `*.iroh.link` (or UDP). On an isolated LAN use `--network local`. |
| `does not accept tasks from <name> (not allowed to delegate)` | The worker's owner must allow you with `peers allow ... --delegate`. |
| `reached its limit of N tasks per hour` | Wait, or the worker's owner raises it with `peers allow ... --delegate --tasks-per-hour M`. |
| `export ordinal too large` when linking `iroh` (Windows GNU) | Keep the `[patch.crates-io]` entry for `vendor/iroh` in `Cargo.toml` (see [vendor/iroh/VENDORED.md](vendor/iroh/VENDORED.md)). |

## Layout

- [src/protocol.rs](src/protocol.rs): wire messages (`hello` with addresses and capabilities, `peers`, `chat`, `task_request`, `task_result`) and the `p2pagents/1` ALPN
- [src/agent.rs](src/agent.rs): `Agent` node on an iroh endpoint: allowlist enforcement, dialing, gossip, auto-reconnect, broadcast, task delegation with permissions and hourly limits, shutdown
- [src/trust.rs](src/trust.rs): allowlist (`peers.json`) and `p2pa1...` tickets
- [src/llm/mod.rs](src/llm/mod.rs): `Provider`, `Access`, `LlmTask`, the `Executor` trait and `SdkExecutor`
- [src/llm/copilot.rs](src/llm/copilot.rs): per-agent Copilot instance on the official Rust SDK (own token and `COPILOT_HOME`), with the access-based permission handler
- [src/profile.rs](src/profile.rs): profiles: private provider homes, OS credential store secrets, environment isolation
- [src/auth.rs](src/auth.rs): `auth login/status/logout` for Copilot, Claude and Codex
- [src/llm/bridge.rs](src/llm/bridge.rs): client for the Node bridge (multiplexed JSON lines over stdio)
- [bridge/index.mjs](bridge/index.mjs): Node bridge calling the Claude Agent SDK and Codex SDK
- [src/setup.rs](src/setup.rs): `setup` (installs the embedded bridge) and `doctor`
- [install.ps1](install.ps1), [install.sh](install.sh): install prebuilt binaries from GitHub releases
- [src/main.rs](src/main.rs): interactive CLI
- [scripts/p2p.ps1](scripts/p2p.ps1): doctor / build / test / demo / live-test helper
- [scripts/live_test.py](scripts/live_test.py): live internet test: 4 agents found by key, allowlist, 2 accounts, real Copilot
- [tests/](tests/): multi-agent (iroh on localhost), allowlist, delegation-rights, bridge and credential-isolation tests
- [vendor/iroh/](vendor/iroh/): iroh 1.4.1 without its `cdylib` crate type (Windows GNU link fix)

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. The vendored iroh copy in [vendor/iroh/](vendor/iroh/) keeps its own
license (MIT OR Apache-2.0).
