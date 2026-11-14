"""Live P2P test: 3 p2pagents processes, each worker with its OWN Copilot instance and
account (separate profiles), gossip discovery, chat, real Copilot delegation, disconnect.

Needs a built binary (cargo build) and the GitHub CLI signed in to at least one account
(`gh auth login`); with two accounts each worker uses a different one.
Profiles are created in a temporary P2PAGENTS_HOME, so your real profiles are untouched.
Run: .\\scripts\\p2p.ps1 live-test
"""
import json, os, re, shutil, subprocess, sys, tempfile, threading, time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(REPO, "target", "debug", "p2pagents.exe" if os.name == "nt" else "p2pagents")
HOME = tempfile.mkdtemp(prefix="p2p-live-home-")
ENV = {**os.environ, "P2PAGENTS_HOME": HOME}
results = []


def gh_accounts():
    out = subprocess.run(["gh", "auth", "status", "--json", "hosts"], capture_output=True, text=True)
    hosts = json.loads(out.stdout or "{}").get("hosts", {})
    return [(a["login"], host) for host, accts in hosts.items() for a in accts if a.get("state") == "success"]


class Agent:
    def __init__(self, name, args):
        self.name, self.lines, self.cursor = name, [], 0
        self.p = subprocess.Popen([EXE, "--name", name, *args], cwd=REPO, env=ENV, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            line = line.rstrip("\n")
            self.lines.append(line)
            print(f"  [{self.name}] {line}", flush=True)

    def send(self, cmd):
        print(f"  [{self.name}] << {cmd}", flush=True)
        self.p.stdin.write(cmd + "\n"); self.p.stdin.flush()

    def wait_for(self, pattern, timeout=15):
        end = time.time() + timeout
        while time.time() < end:
            for i in range(self.cursor, len(self.lines)):
                if re.search(pattern, self.lines[i]):
                    return self.lines[i]
            time.sleep(0.1)
        return None

    def stop(self):
        if self.p.poll() is None:
            try: self.send("/quit")
            except OSError: pass
            try: self.p.wait(10)
            except subprocess.TimeoutExpired: self.p.kill()


def check(name, ok, detail=""):
    results.append((name, bool(ok)))
    print(f"{'PASS' if ok else 'FAIL'}: {name} {detail}", flush=True)


ws = tempfile.mkdtemp(prefix="p2p-live-")
agents = []
try:
    accounts = gh_accounts()
    if not accounts:
        sys.exit("no GitHub CLI account signed in; run: gh auth login")
    acct1, acct2 = accounts[0], accounts[1 % len(accounts)]
    print(f"== sign in profiles: worker1 -> {acct1[0]}@{acct1[1]}, worker2 -> {acct2[0]}@{acct2[1]}")
    for profile, (user, host) in (("worker1", acct1), ("worker2", acct2)):
        out = subprocess.run([EXE, "auth", "login", "copilot", "--profile", profile, "--gh-user", user, "--host", host],
                             env=ENV, capture_output=True, text=True)
        print(f"  {out.stdout.strip()}{out.stderr.strip()}")
        check(f"{profile} signed in as {user}@{host}", f"signed in as {user}@{host}" in out.stdout)

    print("== an agent whose profile is not signed in must refuse, not borrow another account")
    out = subprocess.run([EXE, "--name", "intruder", "--providers", "copilot"], env=ENV, capture_output=True,
                         text=True, stdin=subprocess.DEVNULL, timeout=60)
    check("unsigned profile refused", out.returncode != 0 and "not signed in for profile 'intruder'" in out.stderr,
          out.stderr.strip())

    print("== start worker1 (own Copilot instance, read-only, repo workspace)")
    w1 = Agent("worker1", ["--listen", "127.0.0.1:7601", "--providers", "copilot", "--serve", "--workspace", REPO])
    agents.append(w1)
    check("worker1 listening", w1.wait_for(r"listening on 127\.0\.0\.1:7601", 60))
    check("worker1 uses its own account", w1.wait_for(rf"copilot: {re.escape(acct1[0])}@{re.escape(acct1[1])} \[profile 'worker1'\]"))

    print("== start worker2 (own Copilot instance, workspace-write, temp workspace) -> knows only worker1")
    w2 = Agent("worker2", ["--listen", "127.0.0.1:7602", "--providers", "copilot", "--serve",
                           "--access", "workspace-write", "--workspace", ws, "--peer", "127.0.0.1:7601"])
    agents.append(w2)
    check("worker2 uses its own account", w2.wait_for(rf"copilot: {re.escape(acct2[0])}@{re.escape(acct2[1])} \[profile 'worker2'\]", 60))
    check("worker2 connected to worker1", w2.wait_for(r"^\+ worker1"))

    print("== start boss (no providers) -> knows only worker1")
    boss = Agent("boss", ["--listen", "127.0.0.1:7603", "--peer", "127.0.0.1:7601"])
    agents.append(boss)
    check("boss connected to worker1", boss.wait_for(r"^\+ worker1"))
    check("boss discovered worker2 via gossip", boss.wait_for(r"^\+ worker2"))

    boss.send("/peers")
    check("boss sees worker1 serving Copilot", boss.wait_for(r"worker1 \(127\.0\.0\.1:7601\) serves \[Copilot\]"))
    check("boss sees worker2 serving Copilot", boss.wait_for(r"worker2 \(127\.0\.0\.1:7602\) serves \[Copilot\]"))

    print("== chat broadcast")
    boss.send("hello peers from boss")
    check("worker1 got chat", w1.wait_for(r"^<boss> hello peers from boss"))
    check("worker2 got chat", w2.wait_for(r"^<boss> hello peers from boss"))

    print("== provider nobody serves")
    boss.send("/delegate claude review the code")
    check("claude delegation refused", boss.wait_for(r"no peer serves claude"))

    print("== two real Copilot delegations in parallel")
    boss.send("/delegate copilot@worker1 Read src/protocol.rs and list the message types it defines, in one line.")
    boss.send("/delegate copilot@worker2 Create a file p2p_live.txt containing exactly: written via p2p delegation. Reply DONE.")
    check("worker1 started task", w1.wait_for(r"task #\d+ from boss \(copilot\) started"))
    check("worker2 started task", w2.wait_for(r"task #\d+ from boss \(copilot\) started"))
    check("worker1 result back at boss", boss.wait_for(r"=== copilot on worker1 ===", 300))
    check("worker2 result back at boss", boss.wait_for(r"=== copilot on worker2 ===", 300))
    time.sleep(1)
    out = "\n".join(boss.lines)
    check("worker1 answer mentions task_request", "task_request" in out.lower() or "taskrequest" in out.lower())
    path = os.path.join(ws, "p2p_live.txt")
    content = open(path).read().strip() if os.path.exists(path) else None
    check("worker2 wrote p2p_live.txt in its workspace", content and "written via p2p delegation" in content, repr(content))
    check("no task failures", not any("failed" in l for l in boss.lines))
    for profile in ("worker1", "worker2"):
        state = os.path.join(HOME, "profiles", profile, "copilot", "session-state")
        check(f"{profile}'s Copilot instance used its own COPILOT_HOME", os.path.isdir(state) and os.listdir(state), state)

    print("== worker2 leaves")
    w2.stop()
    check("boss saw worker2 disconnect", boss.wait_for(r"^- worker2"))
    check("worker1 saw worker2 disconnect", w1.wait_for(r"^- worker2"))
    boss.send("/delegate copilot@worker2 hi")
    check("delegating to departed peer refused", boss.wait_for(r"no peer named worker2"))
finally:
    for a in reversed(agents):
        a.stop()
    shutil.rmtree(ws, ignore_errors=True)
    shutil.rmtree(HOME, ignore_errors=True)

passed = sum(ok for _, ok in results)
print(f"\n== {passed}/{len(results)} checks passed")
sys.exit(0 if passed == len(results) else 1)
