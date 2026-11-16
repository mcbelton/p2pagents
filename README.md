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

Prebuilt binaries for Windows (x64, ARM64), Linux (x64, ARM64) and macOS (Apple Silicon) are published as
[GitHub releases](https://github.com/mcbelton/p2pagents/releases). Everything an agent
needs at runtime is in the one `p2pagents` executable, except the provider CLIs below.

**1. Install p2pagents**

```powershell
# Windows (PowerShell)
irm https://github.com/mcbelton/p2pagents/releases/latest/download/install.ps1 | iex
```

```sh
# Linux / macOS
curl -fsSL https://github.com/mcbelton/p2pagents/releases/latest/download/install.sh | sh
```

The installer puts `p2pagents` in `%LOCALAPPDATA%\p2pagents\bin` (Windows) or
`~/.local/bin`, and adds it to `PATH`; open a new terminal afterwards. You can also
download the archive for your platform from the
[releases page](https://github.com/mcbelton/p2pagents/releases) and put `p2pagents`
anywhere on `PATH`. To install a specific version on Windows, download `install.ps1` and
run `.\install.ps1 -Version v0.1.2` (Linux/macOS: `P2PAGENTS_VERSION=v0.1.2 sh install.sh`).

> **Forks or private copies:** for a private repo, the installers download through the
> GitHub CLI. If `gh` has several accounts, pick one with access:
> `.\install.ps1 -Repo <owner>/<repo> -GhUser <login>` (Linux/macOS:
> `P2PAGENTS_REPO=<owner>/<repo> P2PAGENTS_GH_USER=<login> sh install.sh`). GitHub shows
> private repos as "not found" to accounts without access.

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
p2pagents --name me --providers copilot --serve --ui     # same, in a browser UI (light/dark)
```

See [Internet P2P](#internet-p2p) for how peers find each other, [Web UI](#web-ui) for the browser app, and
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
.\scripts\p2p.ps1 evidence    # every claim in evidence/claims.json checked against its tests (add -Live for the internet test)
```

In the **boss** window wait for `+ worker`, then type `/peers` and
`/delegate copilot@worker:repo Summarize README.md in 3 bullets` (`repo` is this folder,
which the demo shares read-only with boss; see [Shared folders](#shared-folders)).
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
the profile's identity key (`<profile>/identity`). Where no OS credential store is
available (headless Linux, WSL, containers), or with `P2PAGENTS_SECRET_STORE=file`, they
are kept in owner-only files (`0600`) in `<profile>\secrets\` instead; `p2pagents doctor`
shows which store is used. The allowlist is `<profile>\peers.json`.

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

### Digital workers

A **digital worker** is an autonomous agent with no identity of its own: it works on the
accounts of the person (profile) who created it, its **owner**.

```powershell
p2pagents worker create tester --owner alice --role "test engineer for the parser"
p2pagents --name tester --providers copilot --serve     # runs on alice's Copilot sign-in
p2pagents worker list --owner alice
```

- The worker is its own peer: its own key, allowlist, [shared folders](#shared-folders),
  hourly limits and [audit log](#audit-log). Peers allowlist it like any agent, and alice
  decides who may delegate to it and which folders it shares.
- It has **no accounts**: Copilot, Claude and Codex run with the owner's sign-in, in the
  worker's own private provider homes. `auth login`/`logout` are refused for workers, and
  `auth status` shows whose accounts they use. A worker can't own another worker.
- Peers see `[digital worker of alice: test engineer for the parser]` next to it, and the
  role is given to the model with every task the worker runs.
- `worker create` allowlists the owner on the worker (read-only tasks in its data folder)
  and the worker on the owner (chat only). Widen either with `peers allow`.
- Work stays private unless shared: a peer reaches only the folders the worker or owner
  explicitly shares with that peer.

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

Then on Bob's agent: `/delegate copilot@alice <prompt>`. It runs in Alice's shared
`.p2pdata` folder, read-only; see [Shared folders](#shared-folders) to share a repository.

A ticket (`p2pa1...`) contains the endpoint id, a suggested name and known addresses. A
bare endpoint id works too (add `--name`). A running agent prints its ticket at startup and
with `/ticket`.

| Peers command | Description |
| --- | --- |
| `p2pagents peers id --profile P` | Show P's endpoint id and ticket |
| `p2pagents peers allow <ticket-or-id> --profile P [--name N] [--delegate [--access LEVEL] [--share data,FOLDER]] [--tasks-per-hour N]` | Allowlist a peer (re-run to change its rights) |
| `p2pagents share add <name> <folder> --profile P [--access LEVEL] [--worktrees] [--check NAME=COMMAND ...]`, `share list`, `share remove <name>` | Folders peers' tasks may use ([Shared folders](#shared-folders)), with optional per-task git worktrees and checks ([EDD with agents](#evidence-driven-development-with-agents)) |
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
cargo run -- share add myrepo D:\src\myrepo --access full --profile worker
cargo run -- peers allow <boss-ticket> --profile worker --delegate --access full --share data,myrepo
cargo run -- peers id --profile worker                            # copy worker's ticket
cargo run -- peers allow <worker-ticket> --profile boss

# terminal 1 (any machine): worker that runs tasks for boss; boss may edit files and run builds/tests in D:\src\myrepo
cargo run -- --name worker --providers copilot,claude --serve

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
| `/delegate <provider>[@peer[:folder]] <prompt>` | Run a task on a peer that serves `<provider>` (first match, or the named peer), in a folder it shares with you (default: its `data` folder) |
| `/plan <file>` | Run a [plan](#squads-and-plans): steps on peers (`@name`) or squads (`#squad`), in dependency order |
| `/squads` | List your squads and which members are online |
| `/runs` | List saved evidence runs ([EDD with agents](#evidence-driven-development-with-agents)) |
| `/approvals` | Show approval requests waiting for you |
| `/approve <n> [note]`, `/reject <n> <why>` | Answer approval request `n` (signed with your key) |
| `/tasks` | List your running tasks with their numbers |
| `/cancel <n>` | Cancel your running task `n`; the peer stops the model run and frees the slot |
| `/peers` | List connected peers and the providers they serve |
| `/ticket` | Print this agent's ticket (endpoint id + current addresses) |
| `/help`, `/quit` | Help, exit |

Results print when they arrive, so you can keep typing while tasks run.

When a task may edit files (`workspace-write` or `full`), its result ends with the files it
changed in the worker's workspace, so you can see what a delegated coding task touched:

```text
=== copilot on worker2 ===
DONE
---
**Files changed in the workspace (1):**
- added `p2p_live.txt`
```

Build output and dependencies (`.git`, `target`, `node_modules`, ...) are not listed. If
two tasks edit the same workspace at once, each lists both tasks' changes.

### Audit log

An agent started with `--serve` records every task a peer asks it to run, accepted or
refused, in the profile's `audit.jsonl`: peer name and key, provider, the access the task
got, outcome (`done`, `failed`, `cancelled`, `refused`), duration and any error. The prompt is **not**
stored, only its SHA-256 and length, so the log holds no peer content.

```text
> p2pagents audit --profile worker
time (UTC)           peer            task provider access           outcome   seconds  prompt
2026-11-15 18:39:55  boss               1 copilot  read-only        done          7.0  43 chars, sha256 7a9139bee1be
```

`--limit N` shows more entries and `--json` prints the raw lines.

### Cancelling tasks

`/cancel <n>` (or **Cancel** in the web UI) stops a task you started. For a delegated task
the peer is told to stop: it aborts the Copilot session (or the Claude/Codex run), frees the
task slot and records the task as `cancelled` in its audit log. If you disconnect, the peer
cancels your running tasks the same way, so nothing keeps spending its accounts for you.

A brief reconnect doesn't lose work: if a connection is replaced (for example both peers
dialed each other at once), running tasks carry on and their results arrive over the new
connection. Tasks are failed or cancelled only if the peer stays away for 2 seconds.

### Squads and plans

A boss (a person, or itself a digital worker) groups peers into **squads** and runs
**plans**: steps that run on a named peer or on any member of a squad, in dependency
order, in parallel where possible.

```powershell
p2pagents squad create qa --member alice,tester --profile boss
p2pagents squad list --profile boss
p2pagents squad remove qa --profile boss
```

A plan is a text file with one step per line (`//` starts a comment):

```text
// id [after a,b] @peer[:folder] | #squad[:folder] [using provider] prompt
api @alice:repo Summarize the public API of src/agent.rs
tests after api #qa:repo using claude Write test ideas for this API
report after api,tests @helper Write a one-paragraph status report
```

- `@peer` runs on that peer; `#squad` runs on the least busy online member that serves the
  provider (and shares the folder, if one is named). `:folder` defaults to the peer's
  `data` folder and `using` to `copilot`.
- A step starts when the steps it waits for (`after`) are done, and gets their results
  (up to 8,000 characters each) ahead of its prompt. Steps without dependencies run at once.
- If a step fails, the steps that depend on it are **skipped**; independent steps still run.
  Unknown steps, duplicate ids and dependency cycles are rejected before anything runs.
- Every step is an ordinary delegated task, so each peer's allowlist, folder grants, hourly
  limit, access level and audit log apply. Squads live only in the boss's profile
  (`squads.json`); members just see tasks from the boss.

Run it with `/plan plan.txt`, or in the web UI's **Plan** mode. Each step appears as a task
that waits, runs, finishes or is skipped, and cancelling any step cancels the whole plan.

### Evidence-driven development with agents

Plans can do evidence-driven development (EDD) across a team: you write down what must be
true (**claims**), agents do the work, and a claim counts as proven only by **evidence that
doesn't rest on a model's word**. That means checks the worker's runtime ran, or a
teammate's sign-off, each signed with the key of the agent that produced it.

**1. Each owner defines the checks for their folder.** Checks are commands such as tests and
linters. Peers see only the check names, never the commands.

```powershell
p2pagents share add app D:\src\app --access full --worktrees `
  --check test="cargo test --locked" --check lint="cargo clippy --all-targets -- -D warnings" --profile alice
p2pagents peers allow <boss-ticket> --profile alice --delegate --access full --share data,app
```

**2. The boss writes a plan with claims.**

```text
claim: PARSE-1 The parser accepts empty input
claim: PARSE-2 A reviewer signed off the change
fix @alice:app verify=test,lint attempts=3 claims=PARSE-1 Make the parser accept empty input, with a test
review after fix #qa:app apply=fix verify=test claims=PARSE-1 Review the change and check it independently
signoff after review approve @carol claims=PARSE-2 Approve the parser change for release
```

| Step option | What it does |
| --- | --- |
| `verify=a,b` | After the model finishes, the **worker's runtime** (not the model) runs those checks in the task's folder. It records exit codes, durations and the end of their output. The step fails if any check fails. |
| `attempts=N` | While checks fail, the model tries again (up to 5 times), each time given the failing output. |
| `claims=ID,...` | The claims this step's evidence counts towards. Claims must be defined with a `claim:` line first. |
| `apply=step` | Applies that earlier step's commit (as a patch) in this step's own worktree first, so a *different* peer can verify the work independently. Needs edit access in a `--worktrees` folder. |
| `skill=a,b` | Puts those skills (instructions: procedure, rules, output format) in the step's prompt. See [The lifecycle](#the-lifecycle-from-a-fresh-project-to-operations) |
| `context=a,b` / `save=topic` | Puts knowledge base topics in the prompt / saves the step's verified result as a topic |
| `approve @person` / `approve @me` | Instead of a model, a team member (or you) approves or rejects, seeing the earlier steps' results. |

**3. Agents do the work, and each result comes back with signed evidence.**

- Every delegated result carries an evidence record: the checks and their results, the
  number of attempts, the commit, and hashes of the prompt and output. The worker **signs**
  it with its identity key. The boss accepts it only if it is signed by the peer the task
  was sent to, for exactly the output received. Results show `Signed ✓` in the web UI.
- An `apply=` patch may not create symlinks or touch files the model itself may not write
  (credentials, git and editor config, agent settings).
- **Worktrees** (`share add --worktrees`, for a git repository): each editing task runs on
  its own branch `p2p/<peer>/<task>-<time>` in its own worktree under
  `~/.p2pworktrees/<profile>`, with git hooks disabled. The work is committed there and
  returned as a patch whose hash is signed. Your checkout is never touched and parallel
  tasks never collide. Review or merge the branch (`git log p2p/boss/...`) when you're
  happy. Removed worktrees keep their branch.
- **Approvals**: the approver sees the request in their web UI (**Waiting for your
  approval**) or terminal (`/approvals`, then `/approve <n> [note]` or `/reject <n> <why>`).
  The answer is signed with their key. Approvers only need to be allowlisted; they need no
  providers and no delegation rights.

**4. Every run is saved with a verdict per claim.**

| Verdict | When |
| --- | --- |
| ✅ proven | Every step that claims it finished, each with verified evidence: all its checks passed, or the approver approved |
| ❌ failed | A step that claims it failed or was skipped (failed checks, rejected, unreachable) |
| ⚠️ unverified | It rests on a model's word only (no checks or sign-off), or no step claims it |
| ⏳ pending | The run was cancelled before its steps finished |

A run **passes** only if every claim is proven. Runs are saved in the boss's profile under
`runs/<id>/`: `run.json`, a Markdown `report.md`, and `<step>.patch` for each commit.

```powershell
p2pagents runs list --profile boss
p2pagents runs show <id> --profile boss       # the report
p2pagents runs verify <id> --profile boss     # re-check every signature and patch hash
p2pagents runs verify --dir \\share\runs\<id> # ... from a run folder anyone sent you
```

Share a run folder with your team as the evidence for a change. `runs verify` doesn't take
the boss's word for anything. It recomputes the verdict from the evidence and checks that
each record:

- is validly signed for exactly its output and patch;
- was requested by the run's boss;
- is signed by the key of the peer the step ran on;
- was produced during the run;
- isn't reused by another step.

It prints the signers' endpoint ids: check them against the keys you know for those people. The
web UI lists runs under **Evidence runs** and opens each report with a fresh signature check.

Checks run your folder's code (tests, builds) with **your** rights and without the OS
sandbox, like a CI job on your machine. So only peers with `full` access to that folder may
request them. Their environment has no inherited credential variables (`GH_TOKEN`,
`ANTHROPIC_API_KEY`, ...). Each check may run for up to 15 minutes, and on a timeout or
cancellation everything it started is killed.

### The lifecycle: from a fresh project to operations

**Playbooks** are ready-made plans for each stage. Fill one in, review it and run it:

```powershell
p2pagents playbook list
p2pagents playbook show bootstrap --set peer=alice --set project=Shop > bootstrap.plan  # fresh project
p2pagents playbook show discover  --set peer=alice --set folder=repo > discover.plan   # existing project
p2pagents playbook show sdlc --set peer=alice --set change="Add CSV export" --set change_id=csv > csv.plan
p2pagents playbook show learn --set peer=alice > learn.plan                           # learn from operations
p2pagents playbook show spec --set peer=alice --set change="Wishlist" --set change_id=wish > wish.plan  # spec first
```

In the web UI: **Plan** mode, pick a playbook, then **Use**.

| Playbook | What it proves |
| --- | --- |
| `bootstrap` | Toolchain and dependencies set up, README, a test and CI, a first `evidence/claims.json`, lead sign-off. All checked by the owner's `build`/`test` checks |
| `discover` | Architecture, components and data flows; how to build, test and run (the `test` check passes as documented); security, privacy and reliability hotspots; a glossary. Saved as knowledge and confirmed by someone who knows the system |
| `sdlc` | Design, then implementation (`test`, `lint`), then independent verification by a reviewer squad applying the patch. Security scan plus accepted review, privacy review accepted by the privacy champion, license and secret scans (`licenses`, `secrets`), then release approval |
| `learn` | Insights about usage, health and recurring failures, plus TSG drafts for failures without one, accepted by a person |
| `spec` | Spec-driven: a feature's spec (user stories, testable acceptance criteria, security and privacy), its REST API contract with examples, a clickable UX prototype and a claim per criterion. Each is proven by built-in checks, then the product owner (who clicks through the prototype) and the tech lead accept it. See [Spec-driven development](#spec-driven-development) |
| `implement-spec` | The accepted spec built as specified, with a test proving each acceptance criterion (`test`, `lint`, `spec:api`, `spec:trace`), verified by a reviewer squad, then signed off |

Checks the playbooks name (`build`, `test`, `lint`, `secscan`, `licenses`, `secrets`) are
your folder's checks (`share add --check ...`); rename them with `--set test=unit` etc.
In the web UI the playbook dialog only offers what exists: peers that run tasks, the
folders they share with you, those folders' checks, your squads and approvers.

**Skills.** Every playbook step names a **skill** (`skill=security-review`). A skill is a
set of instructions the agent follows: the procedure, rules and output format. It is
put in the step's prompt, so every peer and provider works the same way. Built-in skills:

| Skill | For |
| --- | --- |
| `bootstrap-project` | Setting up a fresh project: toolchain, README, tests, CI, claims |
| `reverse-engineer`, `build-run-discovery`, `risk-review` | Learning an existing codebase: architecture and data flows, how to build/test/run, security/privacy/reliability hotspots |
| `design-doc`, `implement-change`, `code-review` | Designing, implementing and independently reviewing a change |
| `security-review`, `privacy-review`, `compliance-check` | SDLC gates: vulnerability classes, personal data handling, licenses and secrets |
| `ops-insights`, `tsg-writing`, `incident-debugging` | Learning from operations, writing troubleshooting guides, the debugging agent |
| `write-spec`, `api-contract`, `prototype-ux`, `spec-to-claims` | Spec-driven development: a testable spec, a REST API contract, a self-contained UX prototype, and claims from acceptance criteria |

Add your team's own, or replace a built-in one with the same name:

```powershell
p2pagents skill list --profile boss
p2pagents skill show security-review --profile boss
p2pagents skill add security-review .\our-security-checklist.md --profile boss   # front matter: name, description
```

**Plan check.** Before a plan runs, it is checked against what exists now:

- connected peers, and the folders they share with you (each folder's checks, your access
  there, worktrees);
- your squads;
- knowledge topics and skills.

`verify=` with the folder's own checks needs full access (the built-in `spec:*` checks
need only read access), `apply=` a `--worktrees` folder, and approvers must be
connected. A plan with errors is not started, and every reason is listed. The web UI
checks plans as you type and shows the problems under the editor.

**GitHub repositories.** Sign a profile in with one of your GitHub CLI accounts, then link
repositories. Each is cloned with that account into `~/.p2prepos/<profile>/<name>` and
shared as a folder with a worktree per task. The token is fetched from `gh` when needed.
It reaches git only as an HTTP header in the environment: never on a command line or in
the clone's config.

```powershell
gh auth login                                           # once, if you haven't
p2pagents github login --gh-user alice --profile alice  # github status / logout
p2pagents repo link acme/shop --access full --check test="npm test" --check lint="npm run lint" --profile alice
p2pagents peers allow <boss-ticket> --profile alice --delegate --access full --share data,shop
p2pagents repo sync shop --profile alice                # fetch + fast-forward
p2pagents repo branches shop --profile alice            # p2p/... branches agents' tasks committed
p2pagents repo pr shop p2p/boss/12-1794781234 --title "Add CSV export" --profile alice
```

`repo pr` pushes the task branch (only `p2p/...` branches) and opens a pull request into
the repository's default branch with the profile's account. In the web UI the **GitHub**
card does the same: **Sign in to GitHub** (pick a GitHub CLI account), **Link repo**
(with checks, and which peers to share it with), **Sync**, and **Open PR** on each task
branch. Digital workers use their owner's GitHub account.

**Knowledge base.** Each profile has a private knowledge base of Markdown topics.

- A step with `save=topic` stores its result there. Only verified results are stored, with
  the run and peer they came from.
- A step with `context=a,b` (or `context=all`) gets those topics in its prompt.

So reverse-engineered architecture, risks, designs and insights build up across runs.

```powershell
p2pagents kb list | show <topic> | add <topic> <file.md> | remove <topic> | search <words> --profile boss
```

**Operations.** On the machine where a service runs, its owner adds monitors. A monitor
runs one of the folder's checks on a schedule, while that profile's agent runs:

```powershell
p2pagents share add svc D:\svc --access full --check health="curl -fsS http://localhost:8080/health" `
  --check restart="pwsh -File restart.ps1" --profile alice
p2pagents monitor add api --share svc --check health --every 60 --fail-after 3 --slo 99.5 --profile alice
```

- Every run is a **metric** (ok, latency, exit code) in the profile's `ops/metrics.jsonl`.
- `fail-after` failures in a row open an **incident** (`ops/incidents.jsonl`). The agent
  **alerts** its terminal and its connected peers by chat.
- **Auto-fix with TSGs.** A troubleshooting guide is a knowledge topic with front matter
  (`tsg`, `monitor`, `match`, `fix`, `verify`, `status`). The fix check of every active
  guide whose monitor matches, and whose `match` text appears in the failure, is run, then
  the verify check.
- **Debugging agent.** If no guide fixes it and the agent has a provider, its model
  investigates read-only in the folder, using the knowledge base. It ends with
  `Confidence: N%` and `Fix: <check>`.
  - At 80% or more confidence, that check runs. If the monitor then passes, the diagnosis
    is saved as a new **active TSG**, so a repeat is fixed without the model.
  - Otherwise a **draft TSG** is written for a person to review, and the incident is
    escalated.
- Fixes can only be checks the owner defined: neither a model nor a peer can run anything
  else.

```powershell
p2pagents ops report --hours 24 --profile alice    # availability vs SLO, p50/p95, incidents, tasks, runs
p2pagents ops incidents --profile alice
p2pagents kb learn --profile alice                 # ops report -> knowledge topic ops-summary
```

**Learning.** Agents with monitors refresh `ops-summary` in the knowledge base every hour.
TSGs learned from incidents accumulate, and the `learn` playbook analyzes it all
(`context=all`) into insights and new TSG drafts. In the web UI, the **Health** panel shows
monitors against their SLOs, open incidents, the incident log and task statistics, and
**Knowledge** lists the topics and TSGs.

### Spec-driven development

Write down and agree on what to build before anyone builds it: a **spec**, a **REST API
contract** and a clickable **UX prototype**, each checked by p2pagents and accepted by
people. Development then starts from evidence, with one claim per acceptance criterion.

A **spec package** lives in the project folder:

| File | What it holds | Checked by |
| --- | --- | --- |
| `spec/SPEC.md` | Problem, users, user stories (`- US-1: As a ..., I want ..., so that ...`), acceptance criteria tied to a story (`- AC-1 (US-1): Given ..., when ..., then ...` or `... must ...`), non-functional requirements (security and privacy included), out of scope, open questions | `spec:spec`: every story has a criterion, every criterion names an existing story and is testable, ids are unique, security and privacy are covered |
| `spec/openapi.json` | The REST API contract (OpenAPI 3, JSON). Each operation lists the criteria it serves: `"x-acceptance": ["AC-1"]` | `spec:api`: `operationId` and summary, a 2xx and a 4xx response, a JSON request body for POST/PUT/PATCH, and every example matches its schema (types, required properties, enums, `$ref`) |
| `prototype/index.html` | A self-contained, clickable prototype that uses the API's examples as data. Screens are marked with what they show: `data-story="US-1" data-ac="AC-1 AC-2"` | `spec:ux`: no external resources and no server calls; every story is shown |
| `evidence/claims.json` | A claim per acceptance criterion (`"id": "AC-1"`), which development proves with tests | `spec:trace`: every criterion is covered by the API or the prototype and claimed; nothing refers to an unknown id |

Start from a working example, edit it, and check it as you go:

```powershell
p2pagents spec init "Wishlist" --dir D:\src\shop    # a starter package that passes every check
p2pagents spec check --dir D:\src\shop              # or: spec check api|spec|ux|trace
```

Or let agents write it. The `spec` playbook runs four steps on a peer, each in the
folder's worktree and building on the previous step's commit:

1. **spec**: `write-spec` skill, `verify=spec:spec`;
2. **api**: `api-contract` skill, `verify=spec:spec,spec:api`;
3. **ux**: `prototype-ux` skill, `verify=spec:ux`;
4. **trace**: `spec-to-claims` skill, all four checks.

Each step gets up to three attempts, each told what failed. The product owner then
accepts the spec and prototype, and the tech lead accepts the API contract.

- **Built-in checks** (`spec:spec`, `spec:api`, `spec:ux`, `spec:trace`) run in the
  worker's runtime after the model, like the folder's own checks, and are signed into
  the evidence. They only read files, in the runtime itself, without following symbolic
  links. So a peer may ask for them with any access, and no folder has to define them.
- **Prototype preview.** A step whose commit creates `prototype/` files gets an **Open
  prototype** button in the web UI. An approval step after it sends the approver the
  prototype, so the product owner (you, or a teammate on their own agent) clicks through
  it from the approval card before answering. The same works from a saved run's report,
  and for a folder you share (**Preview prototype**).
  - Previews open in a new tab at an unguessable link that expires after 2 hours.
  - They run in a CSP sandbox: an opaque origin, no network, no form posts. A prototype a
    model or a peer wrote can't reach your agent's API or token.
- **Then build it.** Merge the spec branch (`repo pr`, or `git merge` the `p2p/...`
  branch), then run `implement-spec`. The model builds the API exactly as the contract
  says and the UI as the prototype shows, and names a test for every criterion in
  `evidence/claims.json`. `test`, `lint`, `spec:api` and `spec:trace` must pass. A
  reviewer squad applies the change on its own copy and checks it independently, then
  someone signs off.

```powershell
p2pagents share add shop D:\src\shop --access full --worktrees --check test="npm test" --check lint="npm run lint" --profile alice
p2pagents playbook explain spec
p2pagents playbook show spec --set peer=alice --set folder=shop --set change="Shoppers save items to a wishlist" `
  --set change_id=wish --set product_owner=carol --set tech_lead=me > wish.plan
p2pagents playbook show implement-spec --set peer=alice --set folder=shop --set change="Wishlist" --set reviewers=qa > build.plan
```

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
| `--workspace <DIR>` | `.` | Folder **your own** tasks (`/ask`, *This agent* in the UI) work in. Never given to peers |
| `--access <MODE>` | `read-only` | Most your own tasks may do there: `read-only`, `workspace-write` (edit files) or `full` (also shell). Peers' access is set per peer and folder ([Shared folders](#shared-folders)) |
| `--bridge <PATH>` | `bridge/index.mjs` in this repo (env `P2PAGENTS_BRIDGE`) | Node bridge script |
| `--ui` | off | Open the [web UI](#web-ui) instead of the terminal prompt |
| `--ui-port <PORT>` | random | Port for the web UI on 127.0.0.1 (needs `--ui`) |
| `--no-open` | off | Print the web UI link without opening a browser (needs `--ui`) |

Subcommands: `p2pagents auth login|status|logout`, `p2pagents peers id|allow|list|remove`,
`p2pagents share add|list|remove`, `p2pagents worker create|list`,
`p2pagents squad create|list|remove`, `p2pagents runs list|show|verify`, `p2pagents kb list|show|add|remove|search|learn`,
`p2pagents playbook list|show`, `p2pagents skill list|show|add|remove`, `p2pagents github login|status|logout`,
`p2pagents repo link|list|sync|branches|pr|unlink`, `p2pagents monitor add|list|remove`, `p2pagents ops report|incidents`, `p2pagents audit`, `p2pagents profile delete`. `P2PAGENTS_HOME` moves the profiles folder
(and uses a separate credential store namespace).

## Web UI

Add `--ui` to any agent to drive it from a browser instead of the terminal prompt:

```powershell
p2pagents --name me --providers copilot --serve --ui
# web UI: http://127.0.0.1:53817/#t=3f9c…   (opens in your default browser)
```

- **Connected peers** with the providers they serve, live.
- **Allowlist**: add a peer by pasting their ticket (optionally letting them delegate, with an
  hourly limit and an access level: read only, edit files, or full) or remove one; changes
  are saved to the profile's `peers.json` and take effect at once (removed peers are
  disconnected).
- **Your ticket** with a copy button, plus this agent's profile, network, providers, accounts,
  access level and workspace.
- **Activity feed**: chat with all connected peers, joins and leaves, refused connections,
  tasks peers delegate to you.
- **Run task**: pick a provider and where to run it (*This agent*, a named peer, or *Any peer*
  when several serve it).
  The **Tasks** panel shows status and elapsed time, renders results as Markdown, can copy
  results and errors, and has a **Cancel** button on running tasks.
- **Plan** mode: write a [plan](#squads-and-plans) in the composer (Enter adds a line,
  Ctrl+Enter runs it). Steps show which steps they wait for, and skipped steps are flagged.
- **Squads**: create, view (members, who is online, running tasks) and remove squads.
  Digital workers show their owner and role in the peer list.
- **Waiting for your approval**: approval requests from plans (yours or teammates'), with
  what you're approving and Approve / Reject (a rejection needs a reason).
- **Evidence runs**: every plan run with its verdict. Open one to read its report (claims,
  checks, commits, signatures) and a fresh signature check. Results that carry valid
  signed evidence show **Signed ✓**.
- **Peer task log** (agents with `--serve`): the [audit log](#audit-log), updated live.
- **Light, dark or System** theme (follows the OS live; the choice is remembered). Keyboard
  accessible, screen-reader labelled, WCAG AA contrast in both themes, works on phone-sized
  windows, honours reduced motion.

Security: the UI listens on `127.0.0.1` only. Each run creates a new random access token,
passed in the link's `#t=` fragment (never sent in requests or logged; the page moves it to
session storage and removes it from the address bar). API calls without it get `401`.
Requests whose `Host` isn't `127.0.0.1`/`localhost` are refused (DNS-rebinding defense).
Pages are served with a strict Content-Security-Policy and all peer text is rendered as
text, never HTML. Restarting the agent invalidates old links; use the new link it prints.
Press Ctrl+C in the terminal to stop the agent.

## Security model

- Connections are encrypted and authenticated (QUIC/TLS with each agent's ed25519 key).
  Only allowlisted keys can connect; everyone else is refused before any message is read.
- An agent runs peer tasks **only** with `--serve`, only for the providers it enabled, only
  from peers allowlisted with `--delegate`, within their hourly limit, and at most 2 at a
  time (others queue).
- Peers' tasks run **only in folders you shared with that peer** (see
  [Shared folders](#shared-folders)), by default your profile's `.p2pdata` folder, read-only.
  Your `--workspace` and every other folder stay private. A peer cannot choose a path or
  raise its access: each task gets the **lower** of the peer's grant
  (`peers allow ... --delegate --access <level>`, default `read-only`) and the folder's
  maximum (`share add ... --access <level>`). Your own tasks get `--access` in `--workspace`.
- Results are signed by the agent that produced them, and checks a peer asks for run only
  with `full` access ([EDD with agents](#evidence-driven-development-with-agents)).
  Approval requests need only the allowlist, and each peer may have at most 20 open.

### Shared folders

A worker decides exactly what each peer can see:

- Every profile has a **data folder**, `~/.p2pdata/<profile>` (created when the agent starts
  with `--serve`; under `$P2PAGENTS_HOME/.p2pdata` if that is set). It is the built-in share
  `data`, always **read-only**. Put there what you want peers' tasks to see, e.g. a status
  note your manager's agent can summarize. Peers' tasks run there unless they name another
  folder.
- To let a peer work on code, share the folder by name with a maximum access, and grant it:

  ```powershell
  p2pagents share add myrepo D:\src\myrepo --access workspace-write --profile alice
  p2pagents peers allow <bob-ticket> --profile alice --delegate --access workspace-write --share data,myrepo
  p2pagents share list --profile alice
  ```

  Bob then runs `/delegate copilot@alice:myrepo Add tests for the parser`. In the web UI,
  Bob picks the folder next to *Run on*; Alice ticks folders in the **Add peer** dialog.
- Peers see shared folders **by name and access only**, never their paths, and only the ones
  granted to them (`/peers` lists them). Changing a connected peer's grant reaches it at once.
- A task naming a folder that isn't shared with that peer is refused (and audited).

| | `data` (built in) | A folder you add |
| --- | --- | --- |
| Where | `~/.p2pdata/<profile>` | Any existing folder |
| Most a task can do | read-only | what you set with `share add --access` |
| Who can use it | peers granted `data` (the default for `peers allow`) | peers you list in `--share` |

### What tasks may do

| Level | Allows | Grant with |
| --- | --- | --- |
| `read-only` | Read and search files **in the task's folder** | default |
| `workspace-write` | Also create and edit files in the folder. No shell commands | `--access workspace-write` on the peer and the folder |
| `full` | Also run shell commands (build, test, git) **inside an OS sandbox** | `--access full` on the peer and the folder. Only for peers you trust |

Always enforced, at every level and for local tasks too:

- **No file access outside the workspace.** Paths are resolved (`..`, symlinks, junctions)
  before checking, so `../`, absolute paths and links pointing outside are refused.
- **No credential files**, even inside the workspace: `.env`/`.env.*` (templates such as
  `.env.example` are fine), `*.pem`, `*.key`, `*.pfx`, `*.p12`, SSH keys (`id_rsa`,
  `id_ed25519`, ...), `.npmrc`, `.pypirc`, `.netrc`, `.git-credentials`, `credentials*`,
  `secrets.*`, and anything under `.ssh`, `.aws`, `.azure`, `.kube`, `.docker`, `.gnupg`,
  `.p2pagents`, `.copilot`.
- **No writes that would run code later**: `.git/` (hooks, config), `.vscode/`, `.idea/`,
  `.claude/`, `.codex/`, `.copilot/`, `.cursor/`, `.github/hooks/`, `.mcp.json`, `.gitmodules`.
- **No web, MCP, memory or sub-agents**: web fetch/search, MCP servers (including GitHub's),
  Copilot memory, sub-agents/skills are not offered to the model at all.

Per provider:

| | `read-only` | `workspace-write` | `full` |
| --- | --- | --- | --- |
| Copilot | only tools `view`, `glob`, `grep`; every read request is path-checked | adds `create`, `edit` (path-checked) | adds the shell tool (`powershell`/`bash`), run in the Copilot CLI's OS sandbox |
| Claude | `dontAsk` mode, tools `Read`, `Glob`, `Grep`, a PreToolUse hook path-checks every call; user/project settings not loaded | adds `Edit`, `Write`, `NotebookEdit` | adds `Bash`, run in Claude's OS sandbox |
| Codex | sandbox `read-only`, approvals `never`, no network or web search | sandbox `workspace-write` | same as `workspace-write` (never `danger-full-access`) |

### The shell sandbox (`full`)

Peers' shell commands never run directly on your machine. Each provider runs them in its
OS sandbox: Windows ProcessContainer, Linux Bubblewrap or macOS Seatbelt for Copilot, and
Claude's sandbox runtime (on Windows a separate low-rights user) for Claude:

- **Writes only inside the workspace** (plus temp folders and toolchain caches such as
  Cargo's registry), so a command can't change files elsewhere, even when obfuscated.
- **No network**: no outbound or local-network connections, so nothing can be uploaded and
  nothing downloaded. Builds work when their dependencies are already cached.
- **No way out**: requests to run a command outside the sandbox are refused.
- **Credential folders unreadable** (`~/.ssh`, `~/.aws`, `~/.azure`, `~/.config/gh`,
  `~/.p2pagents`, ...): for Copilot wherever the sandbox reports the `denied_paths`
  capability (Linux, macOS, and Windows with Copilot CLI 1.0.112+ on a current Windows
  build), and on every platform for Claude.
- **Fails closed**: if a host has no sandbox, Copilot tasks drop the shell tool (printed
  as `copilot: no OS sandbox on this host`) and Claude tasks fail rather than run
  unsandboxed. Run `copilot update` so p2pagents can check sandbox support at startup.

On top of the sandbox, commands that name a URL, a path outside the workspace, the home
folder (`~`, `$HOME`, `$env:USERPROFILE`, `%APPDATA%`, ...) or a credential file are refused
before they run.

Limits to know:

- **Sandboxed commands can still read other files your account can read** outside the
  workspace (only the credential folders above are denied). They have no network to send
  them anywhere, and only the task result goes back to the peer. On older Windows builds
  or Copilot CLIs without `denied_paths`, the credential folders are readable too. For
  untrusted peers keep them at `workspace-write`, or run the serving agent under a
  separate OS account or in a VM/container.
- **Searches see the whole workspace.** A `grep` over the workspace folder is approved as one
  request, so it can return lines from a `.env` inside it. Keep secrets out of the workspace
  you serve (or out of Git-ignored files in it).
- **Codex** confines writes and network with its own sandbox, but its sandbox does not stop
  reads outside the workspace (and on Windows the sandbox is limited). Serve Codex only to
  peers you trust, or run that agent under a separate OS account or in a container.
- Denied requests are printed in the agent's terminal (`copilot: denied ...`,
  `claude: denied ...`). Set `P2PAGENTS_DEBUG_PERMISSIONS=1` to print every Copilot
  permission request.

- Each agent uses only its own profile's accounts and provider instances (see
  [Isolation guarantees](#accounts-and-profiles)). Delegating peers never get credentials.
- Prompts from a peer you allow to delegate run with your accounts and in your workspace,
  and the result goes back to that peer: anything the task can read, the peer can see. Only
  grant `--delegate` to people you trust, and `workspace-write`/`full` to fewer still.

## Test

```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

The tests use a fake executor and a fake bridge ([tests/fixtures/fake_bridge.mjs](tests/fixtures/fake_bridge.mjs)),
so they need Node.js but no LLM credentials. The multi-agent tests run real iroh QUIC
endpoints on localhost in local mode, so they need no internet.
`.\scripts\p2p.ps1 live-test` runs the end-to-end test over the internet.

## Evidence-driven development

This section is about how p2pagents itself is built. To run EDD on your own projects with
a team of agents, see [Evidence-driven development with agents](#evidence-driven-development-with-agents).

Every behavior p2pagents promises is written down as a **claim** in
[evidence/claims.json](evidence/claims.json), with the evidence that proves it: unit and
integration tests, and checks from the live internet test. For example:

```json
{ "id": "AUD-1", "claim": "Every task a peer asks an agent to run is recorded ... without the prompt text.",
  "tests": ["lib::audit::records_outcomes_without_the_prompt", "agents::delegation_needs_permission_and_respects_the_hourly_limit"],
  "live":  ["worker2 audited the refused request from worker1", "audit logs hold no prompt text"] }
```

[scripts/evidence.py](scripts/evidence.py) runs the gates and the tests, matches each result
to the claims and writes a report:

```powershell
.\scripts\p2p.ps1 evidence         # fmt, clippy, tests -> claims (no credentials; what CI runs)
.\scripts\p2p.ps1 evidence -Live   # also the live internet test with real Copilot (needs gh sign-in)
# report: target\evidence\report.md (+ report.json, raw logs in target\evidence\logs)
```

It fails when a gate fails, any claim has missing or failing evidence, or a test or live
check proves no claim. Every claim needs evidence, and every test must prove a claim. CI
runs it on every push, publishes the report in the run summary and keeps it as the
`evidence-report` artifact.

Workflow for a change:

1. Write or update the claim in `evidence/claims.json` first (what will be true).
2. Add the tests (and a live check in `scripts/live_test.py` if it involves real peers or
   providers) and list them under the claim. Test ids are `<target>::<path>` as printed by
   `cargo test`, without `::tests` (`lib::...`, `bin::...`, or the file name in `tests/`).
3. Implement until `.\scripts\p2p.ps1 evidence` passes. Run with `-Live` before a release.
4. Keep the report (`target\evidence\report.md`) with the PR or release as its evidence.

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

- [src/protocol.rs](src/protocol.rs): wire messages (`hello` with addresses and capabilities, `peers`, `chat`, `task_request`, `task_result`, `task_cancel`; unknown types are ignored) and the `p2pagents/1` ALPN
- [src/agent.rs](src/agent.rs): `Agent` node on an iroh endpoint: allowlist enforcement, dialing, gossip, auto-reconnect, broadcast, task delegation with permissions and hourly limits, shutdown
- [src/trust.rs](src/trust.rs): allowlist (`peers.json`) and `p2pa1...` tickets
- [src/llm/mod.rs](src/llm/mod.rs): `Provider`, `Access`, `LlmTask`, the `Executor` trait and `SdkExecutor`
- [src/llm/copilot.rs](src/llm/copilot.rs): per-agent Copilot instance on the official Rust SDK (own token and `COPILOT_HOME`), with per-access tool allowlists and the path-checking permission handler
- [src/llm/policy.rs](src/llm/policy.rs): workspace confinement, credential-file and code-execution-file rules, shell command scan
- [src/profile.rs](src/profile.rs): profiles: private provider homes, OS credential store secrets, environment isolation, digital workers (owner accounts, role)
- [src/auth.rs](src/auth.rs): `auth login/status/logout` for Copilot, Claude and Codex
- [src/llm/bridge.rs](src/llm/bridge.rs): client for the Node bridge (multiplexed JSON lines over stdio)
- [bridge/index.mjs](bridge/index.mjs): Node bridge calling the Claude Agent SDK and Codex SDK
- [src/setup.rs](src/setup.rs): `setup` (installs the embedded bridge) and `doctor`
- [src/audit.rs](src/audit.rs): per-profile audit log of peers' tasks (`audit.jsonl`, no prompt text)
- [src/shares.rs](src/shares.rs): shared folders (built-in read-only `.p2pdata` folder, named shares with a maximum access)
- [src/plan.rs](src/plan.rs): squads (`squads.json`), plan parsing and validation (claims, checks, approvals), and the dependency-ordered plan runner
- [src/evidence.rs](src/evidence.rs): signed evidence records, owner-defined checks (run with a timeout, process tree killed) and retry prompts
- [src/worktree.rs](src/worktree.rs): per-task git worktrees and branches, commits and patches
- [src/runs.rs](src/runs.rs): evidence runs: verdict per claim, Markdown reports, offline signature re-verification
- [src/knowledge.rs](src/knowledge.rs): the knowledge base (topics, context for prompts, troubleshooting guides)
- [src/playbooks.rs](src/playbooks.rs) and [playbooks/](playbooks): bootstrap, discover, sdlc, learn, spec and implement-spec plan templates
- [src/spec.rs](src/spec.rs) and [spec-starter/](spec-starter): the built-in spec checks (`spec:spec`, `spec:api`, `spec:ux`, `spec:trace`) and the `spec init` starter package
- [src/preview.rs](src/preview.rs): sandboxed previews of UX prototypes (from a step's patch, a shared folder or an approval request)
- [src/ops.rs](src/ops.rs): monitors, metrics, incidents, TSG auto-fix, the debugging agent and ops reports
- [src/skills.rs](src/skills.rs) and [skills/](skills): built-in and profile skills (instructions for plan steps)
- [src/github.rs](src/github.rs): GitHub sign-in, linked repositories (clone, sync, push task branches, pull requests)
- [src/llm/changes.rs](src/llm/changes.rs): workspace snapshots and the "files changed" report on task results
- [evidence/claims.json](evidence/claims.json), [scripts/evidence.py](scripts/evidence.py): claims and the evidence harness (see [Evidence-driven development](#evidence-driven-development))
- [install.ps1](install.ps1), [install.sh](install.sh): install prebuilt binaries from GitHub releases
- [src/main.rs](src/main.rs): interactive CLI
- [src/ui.rs](src/ui.rs): web UI server (`--ui`): loopback-only, token-guarded JSON API and Server-Sent Events
- [ui/](ui/): the web app (HTML/CSS/JS, embedded in the binary), light and dark themes
- [scripts/p2p.ps1](scripts/p2p.ps1): doctor / build / test / demo / live-test helper
- [scripts/live_test.py](scripts/live_test.py): live internet test: 4 agents found by key, allowlist, 2 accounts, real Copilot
- [tests/](tests/): multi-agent (iroh on localhost), allowlist, delegation-rights, bridge and credential-isolation tests
- [vendor/iroh/](vendor/iroh/): iroh 1.4.1 without its `cdylib` crate type (Windows GNU link fix)

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. The vendored iroh copy in [vendor/iroh/](vendor/iroh/) keeps its own
license (MIT OR Apache-2.0).
