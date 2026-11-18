// Cases for the plan editor's palette (ui/plan-text.js); tests/ui.rs runs this with node.
// "|" marks the cursor before a click; the result shows the selection in [brackets].
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

// Loaded the way the page loads it: a classic script, in a context of its own.
const code = fs.readFileSync(path.join(__dirname, "..", "ui", "plan-text.js"), "utf8");
const planText = vm.runInNewContext(`${code}\n;planText`, {});
const defaults = { target: "@worker:repo", check: "test" };
const { ERRORS } = planText;

function show(plan, change) {
  if (change.error) return `error: ${change.error}`;
  const out = plan.slice(0, change.from) + change.insert + plan.slice(change.to);
  const [start, end] = change.select;
  return `${out.slice(0, start)}[${out.slice(start, end)}]${out.slice(end)}`;
}
const cursor = (text) => [text.replace("|", ""), text.indexOf("|")];
const part = (text, value) => {
  const [plan, pos] = cursor(text);
  return show(plan, planText.addPart(plan, pos, value, defaults));
};
const line = (text, what) => {
  const [plan, pos] = cursor(text);
  return show(plan, planText.addLine(plan, pos, what, defaults));
};

const cases = [
  // Skills clicked in an empty plan start a step, then add up.
  ["skill in an empty plan", part("|", "skill=adr"), "step1 @worker:repo skill=adr [What to do]"],
  ["a second skill", part("step1 @worker:repo skill=adr |What to do", "skill=api-contract"),
    "step1 @worker:repo skill=adr,api-contract [What to do]"],
  ["the same skill again", part("step1 @worker:repo skill=adr What| to do", "skill=adr"),
    "step1 @worker:repo skill=adr [What to do]"],
  ["a line that starts with options", part("skill=adr skill=api-contract|", "skill=code-review"),
    "step1 @worker:repo skill=adr,api-contract,code-review [What to do]"],
  // Options go before what to do, wherever the cursor is.
  ["option after the task text", part("fix @alice:app verify=test Make it work|", "skill=implement-change"),
    "fix @alice:app verify=test skill=implement-change Make it work[]"],
  ["option after using", part("fix @alice:app using claude Make| it work", "verify=lint"),
    "fix @alice:app using claude verify=lint Make it work[]"],
  ["one-value option replaced", part("fix @alice:app verify=test attempts=2 Do| it", "attempts=3"),
    "fix @alice:app verify=test attempts=3 Do it[]"],
  ["topic placeholder selected", part("fix @alice:app Do it|", "save=topic"), "fix @alice:app save=[topic] Do it"],
  ["indentation kept", part("  fix @x d|o", "skill=s"), "  fix @x skill=s do[]"],
  // Who runs it replaces who did; a sign-off is by a person.
  ["who runs it replaced", part("fix @alice:app verify=test Do| it", "@bob:web"), "fix @bob:web verify=test Do it[]"],
  ["plain words become what to do", part("Fix the login bug|", "@bob:web"), "step1 @bob:web Fix the login bug[]"],
  ["sign-off by the peer", part("ok after fix approve @me claims=C-1 Approve| it", "@carol:app"),
    "ok after fix approve @carol claims=C-1 Approve it[]"],
  ["no squad sign-off", part("ok approve @me Approve| it", "#qa"), `error: ${ERRORS.squadSignoff}`],
  ["no checks on a sign-off", part("ok approve @me Approve| it", "verify=test"), `error: ${ERRORS.signoffOption}`],
  // after: once, in its place, never on the step itself.
  ["after in its place", part("a @x do\nb @x Do| it", "after a"), "a @x do\nb after a @x Do it[]"],
  ["after adds up", part("a @x do\nb after a @x Do| it", "after c"), "a @x do\nb after a,c @x Do it[]"],
  ["no waiting for itself", part("a @x Do| it", "after a"), `error: ${ERRORS.ownStep("a")}`],
  // Elsewhere a part starts a new step: on an empty line there, after a comment, and
  // after the plan's end from a claim (claims come first).
  ["empty first line", part("|\nfix @x do", "skill=s"), "step2 @worker:repo skill=s [What to do]\nfix @x do"],
  ["after a comment", part("// notes|\na @x do", "mcp=browser"), "// notes\nstep2 @worker:repo mcp=browser [What to do]\na @x do"],
  ["from a claim line", part("claim: C-1 x|\nclaim: C-2 y", "skill=adr"),
    "claim: C-1 x\nclaim: C-2 y\nstep1 @worker:repo skill=adr [What to do]"],
  // Whole lines: claims with the claims, steps after the cursor's line, sign-offs on
  // the step at the cursor.
  ["claim after the claims", line("claim: C-1 x\nfix @x d|o", "claim"),
    "claim: C-1 x\nclaim: C-2 [What must be true at the end]\nfix @x do"],
  ["first claim before the steps", line("// plan\nfix @x d|o", "claim"),
    "// plan\nclaim: C-1 [What must be true at the end]\nfix @x do"],
  ["claim in a plan of notes", line("// only a note|", "claim"), "// only a note\nclaim: C-1 [What must be true at the end]"],
  ["step after the cursor's line", line("fix @x d|o\nlast @x do", "step"),
    "fix @x do\nstep3 @worker:repo verify=test [What to do]\nlast @x do"],
  ["step proves a claim if there is one", line("claim: C-1 x\nfix @x d|o", "step"),
    "claim: C-1 x\nfix @x do\nstep2 @worker:repo verify=test claims=C-1 [What to do]"],
  ["sign-off on the step at the cursor", line("fix @x d|o\nship @x do", "signoff"),
    "fix @x do\nreview after fix approve @me [Check the result of fix and approve it]\nship @x do"],
  ["sign-off in an empty plan", line("|", "signoff"), "review approve @me [Approve it]"],
];

const failed = cases.filter(([, got, want]) => got !== want);
for (const [name, got, want] of failed) console.error(`${name}\n  want: ${JSON.stringify(want)}\n  got:  ${JSON.stringify(got)}`);
if (failed.length) {
  console.error(`${failed.length} of ${cases.length} palette cases failed`);
  process.exit(1);
}
console.log(`ok: ${cases.length} palette cases`);
