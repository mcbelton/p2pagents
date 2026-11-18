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

### Scheduled plans

An agent can run a plan or a playbook **on its own at set times**: a nightly `learn`, a
weekday-morning review of what changed, an hourly end-to-end check through a peer. Each
run is an ordinary [evidence run](#evidence-driven-development-with-agents).

```powershell
p2pagents schedule add nightly-learn --when "daily 02:00" --playbook learn --set peer=alice --profile boss
p2pagents schedule add smoke --when "every 1h" --plan smoke.plan --profile boss
p2pagents schedule list --profile boss          # next run and the last verdict
p2pagents schedule pause smoke --profile boss   # resume / remove
```

| `--when` (local time) | Runs |
| --- | --- |
| `every 30m`, `every 2h`, `every 1d` | that long after the last run (at most every 5 minutes) |
| `daily 09:00` or `every day at 09:00` | every day at that time |
| `weekdays 08:30`, `weekends 10:00` | Monday to Friday, or Saturday and Sunday |
| `mon,wed,fri 17:00` | on those days |

- Schedules run while the profile's agent runs, with or without the web UI. Changes made
  with the CLI are picked up within 30 seconds, without a restart.
- A schedule never overlaps itself: if a run is still going when the next one is due, the
  next one is skipped. A run missed while the agent was off runs **once** when it starts.
- The plan is checked like any other before it runs. If it can't start (say a peer it
  needs is still reconnecting after a restart), it is tried again every 2 minutes for an
  hour, then recorded as **missed** with the reason, and the schedule counts on from then.
  **Run now** says at once why a run can't start.
- Resuming a paused schedule counts from the moment you resume it: paused runs are not
  caught up.
- In the web UI, **Plan** mode has **Schedule…** to schedule the plan in the editor: pick
  how often (every day, weekdays, weekends, days you pick, every few minutes, hours or
  days, or the text form) and see the next three runs before you save. The
  **Schedules** panel shows each schedule in plain words ("Weekdays at 08:30"), when it
  runs next ("in 3 hours"), its last run's result with a link to the evidence run, and
  **Run now**, **Pause**/**Resume**, **Edit** and **Delete**. Steps of scheduled runs
  appear as tasks, like any plan.

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
| `mcp=a,b` | Lets the model use those MCP servers the folder's owner offers you (see [MCP servers](#mcp-servers-owner-controlled-tools-for-peers-tasks)) |
| `approve @person` / `approve @me` | Instead of a model, a team member (or you) approves or rejects, seeing the earlier steps' results. |

A step line is `name [after steps] @peer[:folder] or #squad [options] what to do`: options
go before the task text (the plan check warns about one typed after it). Repeating an
option that takes a list adds to it: `skill=adr skill=api-contract` is
`skill=adr,api-contract`. `attempts`, `apply` and `save` are given once.

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
| ⚠️ unverified | It rests on a model's word: a step that claims it has neither checks nor a sign-off (even if another step proves it), or no step claims it. The plan check warns about both before the run, and the web UI offers the fix (e.g. **Let review prove C-1**) |
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
p2pagents playbook show design --set peer=alice --set change="Wishlist" --set change_id=wish > design.plan  # then design and reviews
```

In the web UI: **Plan** mode, **Start from a playbook…**: each playbook with what it is
for, the steps it runs, what it proves and what it needs; then pick who does the work and
where, and the plan is filled in and checked.

| Playbook | What it proves |
| --- | --- |
| `bootstrap` | Toolchain and dependencies set up, README, a test and CI, a first `evidence/claims.json`, lead sign-off. All checked by the owner's `build`/`test` checks |
| `discover` | Architecture, components and data flows; how to build, test and run (the `test` check passes as documented); security, privacy and reliability hotspots; a glossary. Saved as knowledge and confirmed by someone who knows the system |
| `sdlc` | Design, then implementation (`test`, `lint`), then independent verification by a reviewer squad applying the patch. Security scan plus accepted review, privacy review accepted by the privacy champion, license and secret scans (`licenses`, `secrets`), then release approval |
| `learn` | Insights about usage, health and recurring failures, plus TSG drafts for failures without one, accepted by a person |
| `spec` | Spec-driven: a feature's spec (user stories, testable acceptance criteria, security and privacy), its REST API contract with examples, a clickable UX prototype and a claim per criterion. Each is proven by built-in checks, then the product owner (who clicks through the prototype) and the tech lead accept it. See [Spec-driven development](#spec-driven-development) |
| `design` | The SDLC design of an accepted spec: architecture with context, component, UML and workflow diagrams, ADRs, data dictionary and data flows, threat model, test and operations plans, then a review pack with a deck per audience and a customer feedback document. Each is proven by built-in checks, then the tech lead, security and privacy champions, product owner and leadership review it. See [Design and reviews](#design-and-reviews) |
| `implement-spec` | The accepted spec (and design) built as specified, with a test proving each acceptance criterion (`test`, `lint`, `spec:api`, `spec:trace`), verified by a reviewer squad, then signed off |
| `rehearse` | The feature's reviews rehearsed before they happen: agents play the tech lead, security and privacy champions, product owner, leadership and a customer in parallel, then a synthesis (`review:rehearsal`) of actions and prepared answers, and your decision. See [Rehearse the reviews first](#rehearse-the-reviews-first) |

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
| `architecture-design`, `adr`, `data-design`, `threat-modeling`, `test-strategy`, `operability-design` | The design package: architecture and diagrams, decision records, data dictionary and flows, STRIDE threat model, test plan, operations plan |
| `review-technical`, `review-product`, `review-leadership`, `review-customer` | The review pack, each in its audience's tone: technical, product (PM), leadership (LT) decks and the customer feedback document |

Add your team's own, or replace a built-in one with the same name:

```powershell
p2pagents skill list --profile boss
p2pagents skill show security-review --profile boss
p2pagents skill add security-review .\our-security-checklist.md --profile boss   # front matter: name, description
p2pagents skill import .\skills\release-notes --profile boss                      # a SKILL.md or its folder; named by its front matter
```

In the web UI, **Plugins & tools › Skills** adds a skill (pasted, or loaded from a
`SKILL.md`) and lists yours, those of your plugins and the built-in ones. Skills also come
in [plugins](#plugins-skills-and-mcp-servers-packaged-together).

**Plan check.** Before a plan runs, it is checked against what exists now:

- connected peers, and the folders they share with you (each folder's checks, your access
  there, worktrees);
- your squads;
- knowledge topics and skills.

`verify=` with the folder's own checks needs full access (the built-in `spec:`, `design:`
and `review:` checks need only read access), `apply=` a `--worktrees` folder, and approvers must be
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
  **alerts** its terminal and the peers on its allowlist by chat; a peer that isn't
  connected gets the alerts it missed when it connects (the last 20, up to a day old, while
  the agent runs). An incident stays open until the monitor passes again, also across
  agent restarts, so the same failure never alerts or runs the debugging agent twice.
- A monitor whose folder or check no longer exists **can't run**: it is skipped (no
  samples, no incidents) and the report and **Health** panel say what to fix, or to
  `p2pagents monitor remove` it. Monitors use the folder's checks as they are now.
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

### Design and reviews

Between an accepted spec and its implementation, the `design` playbook produces the
SDLC artifacts reviewers need. Every artifact is proven by a built-in check and traced
to the spec. Then each audience reviews it in its own form.

**The design package** lives in `design/`. Diagrams are Mermaid: they render on GitHub
(in the pull request) and in VS Code.

| File | What it holds | Checked by |
| --- | --- | --- |
| `architecture.md` | Context diagram, components (table and component diagram), domain model (UML class or ER diagram), deployment, quality attributes with targets, and the list of decisions | `design:arch` |
| `workflows.md` | A UML sequence diagram per workflow, each named after the user story it implements (all stories covered); state or activity diagrams | `design:arch` |
| `adr/ADR-0001-*.md` | Architecture decision records: status (Proposed, Accepted, ...), context, decision, alternatives considered, consequences | `design:arch` |
| `data-dictionary.md` | Every field (every property of the API's schemas, and what is stored besides): type, classification (Public, Internal, Confidential, Personal, Sensitive) and, for personal data, purpose and retention | `design:data` |
| `dataflow.md` | The data flow diagram with every trust boundary as a subgraph, and a table of flows (`DF-1`): what data moves, whether it crosses a boundary, how it is protected. Every entity with personal data is in a flow | `design:data` |
| `threat-model.md` | Assets, trust boundaries and STRIDE threats (`TM-1`): element, category, threat, mitigation, risk, status. All six categories, every boundary flow covered, no open high risk | `design:threats` |
| `test-plan.md` | A test for every acceptance criterion and every high-risk threat, at a level that proves it (criteria the API serves need contract, integration or e2e tests), plus how performance and security are tested | `design:tests` |
| `operations.md` | Rollout (flag, rings), rollback, an SLO with a target, metrics, dashboard, an alert table with actions, and runbooks | `design:ops` |

**The review pack** lives in `review/`. The decks are [Marp](https://marp.app) Markdown:
slides separated by `---`, speaker notes in `<!-- -->` comments.

| File | Audience and tone | `review:decks` holds it to |
| --- | --- | --- |
| `technical.md` | Engineers and architects: precise, evidence-backed, decision-focused | at most 30 slides; architecture, ADRs, API, data, threats, tests, operations, open questions; cites existing ADR and threat ids |
| `product.md` | Product owner and PMs: user value, prototype walkthrough, scope, measurable success | at most 20 slides; problem, every user story, prototype, scope, success metrics, release plan, decisions needed |
| `leadership.md` | Leadership team: bottom line up front, outcomes, risks, one clear ask | at most 12 slides; summary first, then impact, timeline, risks and ask; under 91 words a slide; no code, diagrams source or internal ids |
| `customer-feedback.md` | Customers: warm, plain, about them, honest about scope | what we heard, what we're building, how it helps, what's not included, at least 3 questions, next steps; no internal ids, security details or email addresses |

Start from a working example, or let agents write it:

```powershell
p2pagents spec init "Wishlist" --dir D:\src\shop
p2pagents design init "Wishlist" --dir D:\src\shop     # design/ and review/ starters that pass every check
p2pagents spec check design --dir D:\src\shop          # or: spec check (everything present), threats, review
p2pagents design render --dir D:\src\shop              # review/html/index.html: decks as slide shows, documents as pages
npx @marp-team/marp-cli D:\src\shop\review\leadership.md --pptx   # a PowerPoint, if your reviewers want one
p2pagents playbook show design --set peer=alice --set folder=shop --set change="Wishlist" --set change_id=wish `
  --set tech_lead=bob --set security_champion=sam --set privacy_approver=pat --set product_owner=carol --set leader=lee > design.plan
```

The `design` playbook runs on a peer, in the folder's worktree, each step building on
the previous one's commit:

1. **arch**: `architecture-design` and `adr` skills, `verify=design:arch`.
2. **data**: `data-design`, `verify=design:data`.
3. **threats**: `threat-modeling`, `verify=design:threats`.
4. **plans**: `test-strategy` and `operability-design`, `verify=design:tests,design:ops`.
5. **pack**: the four review skills, all six checks.

Then the reviews:

- The tech lead accepts the design (technical deck).
- The security champion accepts the threat model.
- The privacy approver accepts the data handling.
- The product owner accepts the product review and approves sharing the customer
  document.
- Leadership reviews last.

Every approval request carries the **review pack**. The approver opens it from the
request (**Open review pack**), or from the step or saved run in the web UI:

- the decks as slide shows: arrow keys, **Speaker notes**, **All slides**, **Print / PDF**;
- the design documents;
- the prototype.

It is served in the same sandbox as prototypes. Decks and documents are rendered
without their raw HTML, so a model's or a peer's Markdown can't run script. Shared
folders with `design/` or `review/` offer **Preview review pack** too.

#### Rehearse the reviews first

The `rehearse` playbook runs a dry run of the reviews before you book them:

1. Six agents on a squad of reviewers each play one stakeholder, all at the same time:
   the tech lead, the security champion, the privacy champion, the product owner,
   leadership and a customer. Each reads the review pack and answers in role, following
   the `stakeholder-simulation` skill: likely verdict, hardest questions, concerns with a
   severity, and what would convince them.
2. A peer merges their feedback into `review/rehearsal.md`, following the
   `feedback-synthesis` skill. The rehearsed reviews reach it as external content, never
   as instructions.
3. You decide what to fix before the real reviews.

The built-in check `review:rehearsal` proves the rehearsal is usable:

- readiness (ready, ready with changes or not ready);
- a section for each of the six audiences, with its likely verdict;
- an actions table where every concern has a severity (blocker, major, minor) and an
  action;
- at least three prepared answers to the hardest questions.

`spec check` runs it once a rehearsal exists, and `design init` includes an example.

```powershell
p2pagents playbook show rehearse --set peer=alice --set folder=shop --set change="Wishlist" --set reviewers=qa > rehearse.plan
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
`p2pagents share add|list|remove`, `p2pagents mcp add|import|list|pause|resume|remove`,
`p2pagents plugin add|list|show|update|enable|disable|remove`, `p2pagents worker create|list`,
`p2pagents squad create|list|remove`, `p2pagents runs list|show|verify`, `p2pagents kb list|show|add|remove|search|learn`,
`p2pagents playbook list|explain|show`, `p2pagents schedule add|list|pause|resume|remove`, `p2pagents skill list|show|add|import|remove`, `p2pagents github login|status|logout`,
`p2pagents repo link|list|sync|branches|pr|unlink`, `p2pagents monitor add|list|remove`, `p2pagents ops report|incidents`, `p2pagents audit`, `p2pagents profile delete`. `P2PAGENTS_HOME` moves the profiles folder
(and uses a separate credential store namespace). `P2PAGENTS_SCHEDULE_TICK_SECS` sets how
often an agent looks for due schedules (default 30).

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
  when several serve it), and the peer's folder. If that folder offers MCP servers, tick
  the ones the task may use (each shows what its owner says it is for).
  The **Tasks** panel shows status and elapsed time, renders results as Markdown, can copy
  results and errors, and has a **Cancel** button on running tasks.
- **Plan** mode: write a [plan](#squads-and-plans) in the composer (Enter adds a line,
  Ctrl+Enter runs it), or press **Open editor** for a full-size editor with everything a
  plan can use beside it: line templates (claim, step, sign-off), connected peers and
  their folders, squads, the folders' checks and MCP servers, skills, knowledge topics,
  `after` and other options. A click adds to the step your cursor is on, in the place the
  plan syntax expects (skills you click add up, who runs it is replaced), or starts a new
  step on an empty line, then selects what to type next. The plan is checked as you type,
  and problems with a clear fix come with a button that makes it.
  Steps show which steps they wait for, and skipped steps are flagged. **Schedule…** runs
  the plan on its own at set times ([scheduled plans](#scheduled-plans)). "What is a
  plan?" stays folded until you open it.
- **Schedules**: each schedule in plain words, its next run, its last run's result (with
  a link to the evidence run), and Run now, Pause/Resume, Edit and Delete.
- **Plugins & tools**: import [plugins](#plugins-skills-and-mcp-servers-packaged-together)
  (review what one contains first; importing runs nothing), offer, pause, change or remove
  [MCP servers](#mcp-servers-owner-controlled-tools-for-peers-tasks) in your shared
  folders (by hand, from an MCP configuration file or from a plugin), and add or remove
  your own skills.
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
  windows, honours reduced motion. From 1180 px wide down the page scrolls instead of
  squeezing the composer. Anything that deletes or withdraws something asks first, in the
  page, saying what stays.

The UI follows the written [UX design guidelines](docs/ux-guidelines.md) (principles,
layout, tokens, components, states, forms, microcopy, accessibility and a definition of
done for UI changes).

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
- **No web, memory or sub-agents**: web fetch/search, Copilot memory, sub-agents and skills
  are not offered to the model at all. MCP servers are offered only when the folder's owner
  defines them (see [MCP servers](#mcp-servers-owner-controlled-tools-for-peers-tasks)); a
  project's own `.mcp.json` and the built-in GitHub server are never used.
- **External content is data, not instructions.** Earlier steps' results, knowledge topics,
  check output, incident text and MCP tool results reach the model inside an
  `<external_content source="...">` envelope, with a standing rule to never follow
  instructions found inside it (prompt-injection *spotlighting*). The envelope can't be
  closed or reopened from inside: forged tags are escaped.
- **Secrets are redacted before results leave the machine.** Results, failing check
  output and check commands, error messages, knowledge topics and incident alerts are
  scanned for known secret formats before they are sent or saved, and each match is
  replaced with `[REDACTED:<kind>]`, with a note saying how many. The formats: private keys
  (also PGP key blocks), GitHub, AWS (access and secret keys), Slack, OpenAI, Anthropic,
  Google and npm tokens, JWTs, connection-string keys, signed URLs, bearer tokens and
  password settings (`password=`, `DB_PASSWORD=`, `PGPASSWORD=`...). Check output is
  redacted before it is cut to its end, so a key printed early can't leak in part. The
  worker signs the evidence for the redacted output. This is a safety net; the
  credential-file rules above still apply.

Per provider:

| | `read-only` | `workspace-write` | `full` |
| --- | --- | --- | --- |
| Copilot | only tools `view`, `glob`, `grep`; every read request is path-checked | adds `create`, `edit` (path-checked) | adds the shell tool (`powershell`/`bash`), run in the Copilot CLI's OS sandbox |
| Claude | `dontAsk` mode, tools `Read`, `Glob`, `Grep`, a PreToolUse hook path-checks every call; user/project settings not loaded | adds `Edit`, `Write`, `NotebookEdit` | adds `Bash`, run in Claude's OS sandbox |
| Codex | sandbox `read-only`, approvals `never`, no network or web search | sandbox `workspace-write` | same as `workspace-write` (never `danger-full-access`) |

### MCP servers: owner-controlled tools for peers' tasks

A folder's owner can let peers' tasks use **MCP servers**: a browser (e.g. Playwright) to
check a web app, a docs or ticket system, an internal API. The owner defines each server
on a shared folder, like its checks:

```powershell
# a local server (stdio): only two of its tools, for any peer with access to 'app'
p2pagents mcp add app browser --access read-only --tool browser_navigate --tool browser_snapshot `
  --description "A headless browser for checking pages" --profile alice -- npx @playwright/mcp@latest --headless
# a remote server (https): all its tools, for peers with full access; the header stays on this machine
p2pagents mcp add app docs --url https://docs.example.com/mcp --header "Authorization=Bearer <token>" --profile alice
# the servers of an MCP configuration file (mcp.json, .mcp.json, .vscode/mcp.json); ${workspaceFolder} becomes the folder
p2pagents mcp import .vscode\mcp.json --share app --server playwright --access read-only --profile alice
p2pagents mcp list --profile alice     # values of variables and headers are hidden
p2pagents mcp pause app docs --profile alice    # offered to no task until you resume it
p2pagents mcp remove app docs --profile alice
```

Changes take effect for a running agent's peers within seconds, also when made from the
command line. In the web UI, **Plugins & tools › MCP servers** does the same: offer a
server by hand, from a pasted (or loaded) MCP configuration or from a
[plugin](#plugins-skills-and-mcp-servers-packaged-together), type the values it needs
(secrets in hidden fields; they are never shown again), and pause, edit or remove offered
servers. It warns when a server needs more access than its folder ever gives.

A plan step asks for servers by name with `mcp=` (or ticks them under **MCP** when
running a task in the web UI):

```text
look @alice:app mcp=browser Open http://localhost:3000 and report what the home page shows
```

| | |
| --- | --- |
| What peers learn | Only the **names** of the servers they may use, per folder, and what their owner says each is for (`--description`). Never commands, URLs, environment variables or headers. |
| Who can use a server | Peers whose access in that folder is at least the server's `--access` (default `full`: a server runs with your rights and may reach the network). A paused server, or one whose plugin is off, is offered to nobody. |
| What a task gets | Only the servers it asks for, and only their allowed tools (`--tool`, default all). A name the folder doesn't offer, or doesn't offer at the peer's access, is refused before the model starts. |
| Copilot | The servers are added to the task's session and their allowed tools to its tool allowlist, under the names `mcp-<server>-<tool>`, so no server can pass for a built-in tool. The permission handler approves only those servers and tools. A post-tool hook puts every result in the external-content envelope, and the repository's own hooks never run. |
| Claude | The servers are passed with `strictMcpConfig` (no other MCP configuration is loaded), their tools are pre-approved as `mcp__<server>__<tool>`, and the tool hook refuses any other. A post-tool hook puts every result in the external-content envelope. Server names can't contain `__`, so no server can pass for another. |
| Codex | Can't use MCP servers here; such tasks are refused. |
| Results | MCP tool results reach the model inside the `<external_content>` envelope, and the task's prompt carries the rule to treat them as data, never as instructions. |

Servers run on your machine with your rights, outside the shell sandbox, so offer only
servers you trust. Remote URLs must be `https://`, except `http://` on this machine.

### Plugins: skills and MCP servers, packaged together

A **plugin** bundles skills and MCP servers, in the layouts GitHub Copilot CLI
(`plugin.json`, `.github/plugin/plugin.json`), Claude Code (`.claude-plugin/plugin.json`)
and Agent Plugins (`plugin.json` with its `$schema`, `skills/`, `mcp.json`) use, or a bare
folder of `skills/*/SKILL.md` and `.mcp.json`. A marketplace (`marketplace.json`) lists
several; pick one with `--plugin`.

```powershell
p2pagents plugin add .\team-tools --profile alice                          # a folder
p2pagents plugin add octo-org/agent-plugins:plugins/reviewer --profile alice  # GitHub (private ones with the profile's GitHub sign-in)
p2pagents plugin add octo-org/marketplace --plugin reviewer --profile alice   # from a marketplace
p2pagents plugin show reviewer --profile alice     # its skills, MCP servers, the values they need, where they're offered
# offer one of its MCP servers in a shared folder, with the values it needs
p2pagents mcp add app tickets --plugin reviewer --set TICKETS_TOKEN=<token> --access read-only --profile alice
p2pagents plugin update reviewer --profile alice   # fetch it again
p2pagents plugin disable reviewer --profile alice  # enable / remove
```

- **Importing runs nothing.** The plugin's files are copied into the profile. Agents,
  commands, hooks and LSP servers are listed as not used and never run.
- **Skills** become usable in plan steps (`skill=name`, or `skill=plugin.name` when
  another skill has that name), and peers' steps get their `SKILL.md` text like any skill.
- **MCP servers** are templates: nothing is offered until you offer one in a folder, with
  the values it needs (a plugin's `userConfig`, `${input:x}`, `${env:X}` or `${X}`
  placeholders; values not given with `--set` come from environment variables of the same
  name). `${PLUGIN_ROOT}` (and the Claude and Copilot spellings) becomes the plugin's
  folder, `${PLUGIN_DATA}` a folder it may write to, `${workspaceFolder}` the shared folder.
- **Turning a plugin off** withdraws its skills and the servers it offered; turning it on
  offers them again. **Updating** keeps each offer's access, tools and values, and pauses
  servers that would now start differently until you look at them and resume them.
  **Removing** a plugin removes the servers it offered.
- In the web UI, **Plugins & tools › Plugins** fetches a plugin for review (what it
  contains, the values its servers need, what isn't used) before you import it, lists a
  marketplace's plugins to pick from, and turns plugins on and off, updates and removes
  them.

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
- [src/shares.rs](src/shares.rs): shared folders (built-in read-only `.p2pdata` folder, named shares with a maximum access, their checks and MCP servers)
- [src/mcp.rs](src/mcp.rs): owner-defined MCP servers (validation, access rule, secrets hidden from listings, provider config), MCP configuration files and server templates with placeholders
- [src/plugins.rs](src/plugins.rs): plugins (sources, fetching for review, the Copilot CLI, Claude Code and Agent Plugins layouts, marketplaces, the profile's plugin store, offers made from templates)
- [src/store.rs](src/store.rs): atomic writes and file locks for the profile's JSON files
- [src/boundary.rs](src/boundary.rs): the external-content envelope and standing rule against prompt injection
- [src/redact.rs](src/redact.rs): secret redaction of results, check output, knowledge and alerts
- [src/schedule.rs](src/schedule.rs): scheduled plans (`--when` parsing, `schedules.json`, the scheduler)
- [src/plan.rs](src/plan.rs): squads (`squads.json`), plan parsing and validation (claims, checks, approvals), and the dependency-ordered plan runner
- [src/evidence.rs](src/evidence.rs): signed evidence records, owner-defined checks (run with a timeout, process tree killed) and retry prompts
- [src/worktree.rs](src/worktree.rs): per-task git worktrees and branches, commits and patches
- [src/runs.rs](src/runs.rs): evidence runs: verdict per claim, Markdown reports, offline signature re-verification
- [src/knowledge.rs](src/knowledge.rs): the knowledge base (topics, context for prompts, troubleshooting guides)
- [src/playbooks.rs](src/playbooks.rs) and [playbooks/](playbooks): bootstrap, discover, sdlc, learn, spec, design, implement-spec and rehearse plan templates
- [src/spec.rs](src/spec.rs) and [spec-starter/](spec-starter): the built-in spec checks (`spec:spec`, `spec:api`, `spec:ux`, `spec:trace`) and the `spec init` starter package
- [src/design.rs](src/design.rs) and [design-starter/](design-starter): the design and review checks (`design:arch`, `design:data`, `design:threats`, `design:tests`, `design:ops`, `review:decks`, `review:rehearsal`) and the `design init` starter
- [src/render.rs](src/render.rs): Markdown to self-contained HTML: Marp decks as slide shows, documents as pages, without raw HTML
- [src/preview.rs](src/preview.rs): sandboxed previews of UX prototypes and review packs (from a step's patch, a shared folder or an approval request)
- [src/ops.rs](src/ops.rs): monitors, metrics, incidents, TSG auto-fix, the debugging agent and ops reports
- [src/skills.rs](src/skills.rs) and [skills/](skills): built-in and profile skills (instructions for plan steps)
- [src/github.rs](src/github.rs): GitHub sign-in, linked repositories (clone, sync, push task branches, pull requests)
- [src/llm/changes.rs](src/llm/changes.rs): workspace snapshots and the "files changed" report on task results
- [evidence/claims.json](evidence/claims.json), [scripts/evidence.py](scripts/evidence.py): claims and the evidence harness (see [Evidence-driven development](#evidence-driven-development))
- [install.ps1](install.ps1), [install.sh](install.sh): install prebuilt binaries from GitHub releases
- [src/main.rs](src/main.rs): interactive CLI
- [src/ui.rs](src/ui.rs): web UI server (`--ui`): loopback-only, token-guarded JSON API and Server-Sent Events; [src/ui/tools.rs](src/ui/tools.rs): its plugins, MCP servers and skills API
- [ui/](ui/): the web app (HTML/CSS/JS, embedded in the binary), light and dark themes; [ui/tools.js](ui/tools.js) is the Plugins & tools dialog
- [docs/ux-guidelines.md](docs/ux-guidelines.md): the UI's design guidelines and definition of done
- [scripts/p2p.ps1](scripts/p2p.ps1): doctor / build / test / demo / live-test helper
- [scripts/live_test.py](scripts/live_test.py): live internet test: 4 agents found by key, allowlist, 2 accounts, real Copilot
- [tests/](tests/): multi-agent (iroh on localhost), allowlist, delegation-rights, bridge and credential-isolation tests; [tests/fixtures/echo_mcp.py](tests/fixtures/echo_mcp.py) is a tiny MCP server and [tests/fixtures/plugin/](tests/fixtures/plugin) a Claude Code style plugin (a skill, an MCP server, a hook that must never run) for the tests
- [vendor/iroh/](vendor/iroh/): iroh 1.4.1 without its `cdylib` crate type (Windows GNU link fix)

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT)
at your option. The vendored iroh copy in [vendor/iroh/](vendor/iroh/) keeps its own
license (MIT OR Apache-2.0).
