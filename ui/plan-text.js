// The plan editor's palette as pure functions of the plan text and the cursor, so it
// is tested without a page (tests/plan_text.test.cjs). A click never pastes raw text:
// each part goes where the plan syntax (src/plan.rs) expects it.
const planText = (() => {
  // Step and claim names: letters, digits, '-', '_' or '.', not starting with '.'.
  const NAME = /^(?!\.)[A-Za-z0-9._-]{1,64}$/;
  // Words that start a part of a step line, so never its name.
  const KEYWORDS = new Set(["after", "approve", "using"]);
  // Step options (before what to do), by the option they set. Repeating a list
  // option adds to it; the others take one value.
  const LISTS = new Map([["verify", "verify"], ["mcp", "mcp"], ["skill", "skill"], ["skills", "skill"],
    ["context", "context"], ["claims", "claims"], ["claim", "claims"]]);
  const SINGLE = new Set(["attempts", "save", "apply"]);
  // A sign-off is a person's decision: no checks, retries, patches or saved result.
  const NOT_IN_SIGNOFF = new Set(["verify", "attempts", "apply", "save"]);
  /// What a new step does until you say: selected, so typing replaces it (the plan
  /// check refuses to run it as is, see src/plan.rs).
  const TODO = "What to do";
  const ERRORS = {
    squadSignoff: "A sign-off needs a person (@name or @me), not a squad.",
    signoffOption: "A sign-off is a person's decision: it has no checks, retries, patches or saved result.",
    ownStep: (id) => `Step ${id} can't wait for itself.`,
  };

  const keyOf = (word) => word.slice(0, Math.max(word.indexOf("="), 0)) || null;
  const valueOf = (word) => word.slice(word.indexOf("=") + 1);
  /// The option `word` sets ("skill" for skills=a), or null if it isn't one.
  const optionOf = (word) => {
    const key = keyOf(word);
    return LISTS.get(key) ?? (SINGLE.has(key) ? key : null);
  };
  const isTarget = (word) => /^[@#]./.test(word);

  /// The line around `pos`: where it starts and ends, and its text.
  function lineAt(text, pos) {
    const start = pos > 0 ? text.lastIndexOf("\n", pos - 1) + 1 : 0;
    const newline = text.indexOf("\n", pos);
    const end = newline < 0 ? text.length : newline;
    return { start, end, line: text.slice(start, end) };
  }

  /// Each line of `text` with where it starts and ends.
  function linesOf(text) {
    let start = 0;
    return text.split("\n").map((line) => {
      const at = { start, end: start + line.length, line };
      start = at.end + 1;
      return at;
    });
  }

  /// How the parser reads a line: "empty", "comment", "claim" or "step".
  function kindOf(line) {
    const t = line.trim();
    return !t ? "empty" : t.startsWith("//") ? "comment" : t.startsWith("claim:") ? "claim" : "step";
  }

  /// Sets an option on `step`: a list option adds its values (skill=a, then skill=b,
  /// is skill=a,b), another option replaces its value.
  function setOption(step, word) {
    const option = optionOf(word);
    const at = step.options.findIndex((o) => optionOf(o) === option);
    if (at < 0) {
      step.options.push(word);
    } else if (SINGLE.has(option)) {
      step.options[at] = word;
    } else {
      const have = step.options[at];
      const values = valueOf(have).split(",").filter(Boolean);
      for (const v of valueOf(word).split(",")) if (v && !values.includes(v)) values.push(v);
      step.options[at] = `${keyOf(have)}=${values.join(",")}`;
    }
  }

  /// Removes `word`'s values from the step's option (claims=C-1 from claims=C-1,C-2);
  /// an option left without values goes.
  function removeOption(step, word) {
    const at = step.options.findIndex((o) => optionOf(o) === optionOf(word));
    if (at < 0) return;
    const drop = valueOf(word).split(",");
    const values = valueOf(step.options[at]).split(",").filter((v) => v && !drop.includes(v));
    if (values.length) step.options[at] = `${keyOf(step.options[at])}=${values.join(",")}`;
    else step.options.splice(at, 1);
  }

  /// A step line's parts. Lenient, so a click can repair a line: parts out of order
  /// are found, repeated options are merged, and a line of plain words is what to do.
  function parseStep(line) {
    const words = line.trim().split(/\s+/).filter(Boolean);
    const step = { id: null, after: [], approve: false, target: null, using: null, options: [], todo: "" };
    const found = [];
    let i = 0;
    if (NAME.test(words[0] ?? "") && !KEYWORDS.has(words[0])) step.id = words[i++];
    for (; i < words.length; i++) {
      const [word, next] = [words[i], words[i + 1]];
      if (word === "after" && !step.target && !step.after.length && next) {
        step.after = next.split(",").filter(Boolean);
        i++;
      } else if (word === "approve" && !step.target && !step.approve) {
        step.approve = true;
      } else if (isTarget(word) && !step.target) {
        step.target = word;
      } else if (word === "using" && step.target && !step.using && next) {
        step.using = next;
        i++;
      } else if (optionOf(word)) {
        found.push(word);
      } else {
        break;
      }
    }
    step.todo = words.slice(i).join(" ");
    if (step.id && i === 1 && words.length > 1) {
      step.todo = words.join(" ");
      step.id = null;
    }
    for (const word of found) setOption(step, word);
    return step;
  }

  /// A step as plan text, its parts in the order the parser reads them.
  const formatStep = (s) => [s.id, s.after.length ? `after ${s.after.join(",")}` : "", s.approve ? "approve" : "",
    s.target, s.using ? `using ${s.using}` : "", ...s.options, s.todo].filter(Boolean).join(" ");

  /// The names of the plan's steps.
  const stepIds = (text) => text.split("\n").filter((l) => kindOf(l) === "step")
    .map((l) => parseStep(l).id).filter(Boolean);
  /// The ids of the plan's claims.
  const claimIds = (text) => text.split("\n").map((l) => l.trim().match(/^claim:\s*(\S+)/)?.[1]).filter(Boolean);
  /// The first of make(from), make(from + 1), ... that isn't taken.
  function freeId(taken, make, from) {
    let n = from;
    while (taken.includes(make(n))) n++;
    return make(n);
  }
  const nextStepId = (text) => {
    const ids = stepIds(text);
    return freeId(ids, (n) => `step${n}`, ids.length + 1);
  };

  /// The change to make: replace [from, to) with `insert`, then select [start, end) of
  /// the line that starts at `lineStart`.
  const change = (from, to, insert, lineStart, [start, end]) =>
    ({ from, to, insert, select: [lineStart + start, lineStart + end] });

  /// Puts a new `line` on the cursor's line if it is empty, else on a line after it;
  /// after the plan's last line if the cursor is on a claim (claims come first).
  function placeLine(text, at, kind, line, select) {
    if (kind === "empty") return change(at.start, at.end, line, at.start, select);
    const pos = kind === "claim" ? text.length : at.end;
    const lead = pos === text.length && text.endsWith("\n") ? "" : "\n";
    return change(pos, pos, lead + line, pos + lead.length, select);
  }

  /// Adds `part` (who runs it, an option or `after STEP`) to the step on the cursor's
  /// line, where the plan syntax expects it, or else starts a new step with it. Returns
  /// the change and what to select after it, or {error} if the part doesn't fit.
  function addPart(text, pos, part, { target }) {
    const at = lineAt(text, pos);
    const kind = kindOf(at.line);
    const step = parseStep(kind === "step" ? at.line : "");
    if (isTarget(part)) {
      if (step.approve && part.startsWith("#")) return { error: ERRORS.squadSignoff };
      // A sign-off is by a person: the peer, not one of its folders.
      step.target = step.approve ? part.split(":")[0] : part;
    } else if (part.startsWith("after ")) {
      const dep = part.slice("after ".length);
      if (dep === step.id) return { error: ERRORS.ownStep(dep) };
      if (!step.after.includes(dep)) step.after.push(dep);
    } else {
      if (step.approve && NOT_IN_SIGNOFF.has(optionOf(part))) return { error: ERRORS.signoffOption };
      setOption(step, part);
    }
    step.id ??= nextStepId(text);
    step.target ??= target;
    step.todo ||= TODO;
    const line = (kind === "step" ? at.line.match(/^\s*/)[0] : "") + formatStep(step);
    // Select a placeholder to type over: a new topic's name, or what to do.
    const topic = part === "save=topic" ? line.indexOf(" save=topic ") : -1;
    const select = topic >= 0 ? [topic + 6, topic + 11]
      : step.todo === TODO ? [line.length - TODO.length, line.length] : [line.length, line.length];
    return kind === "step" ? change(at.start, at.end, line, at.start, select) : placeLine(text, at, kind, line, select);
  }

  /// Adds a claim, a step or a sign-off as a line of its own and selects what to write
  /// in it. Claims go after the other claims (before the steps if there are none);
  /// steps and sign-offs after the cursor's line. A sign-off is on the step at the
  /// cursor, or the last step.
  function addLine(text, pos, what, { target, check }) {
    const at = lineAt(text, pos);
    const kind = kindOf(at.line);
    const claims = claimIds(text);
    const ids = stepIds(text);
    const proves = claims.length ? `claims=${claims[0]}` : "";
    const make = (head, todo) => {
      const line = [...head, todo].filter(Boolean).join(" ");
      return [line, [line.length - todo.length, line.length]];
    };
    if (what === "claim") {
      const [line, select] = make(["claim:", freeId(claims, (n) => `C-${n}`, claims.length + 1)],
        "What must be true at the end");
      const lines = linesOf(text);
      const lastClaim = lines.filter((l) => kindOf(l.line) === "claim").at(-1);
      if (lastClaim) return change(lastClaim.end, lastClaim.end, `\n${line}`, lastClaim.end + 1, select);
      const firstStep = lines.find((l) => kindOf(l.line) === "step");
      if (firstStep) return change(firstStep.start, firstStep.start, `${line}\n`, firstStep.start, select);
      const lead = !text || text.endsWith("\n") ? "" : "\n";
      return change(text.length, text.length, lead + line, text.length + lead.length, select);
    }
    if (what === "step") {
      const id = freeId(ids, (n) => `step${n}`, ids.length + 1);
      // Only a step with a check can prove a claim: without one it would spoil it.
      const [line, select] = make([id, target, check ? `verify=${check}` : "", check ? proves : ""], TODO);
      return placeLine(text, at, kind, line, select);
    }
    const of = (kind === "step" && parseStep(at.line).id) || ids.at(-1);
    const id = ids.includes("review") ? freeId(ids, (n) => `review-${n}`, 2) : "review";
    const [line, select] = make([id, of ? `after ${of}` : "", "approve @me", proves],
      of ? `Check the result of ${of} and approve it` : "Approve it");
    return placeLine(text, at, kind, line, select);
  }

  /// Makes a plan check fix's edits (Fix in src/plan.rs) on `text`: add or remove a
  /// step's option, add a line after a step's, remove a claim. Returns the new text.
  function applyFix(text, edits) {
    for (const edit of edits) {
      const lines = linesOf(text);
      if (edit.do === "remove_claim") {
        const at = lines.find((l) => kindOf(l.line) === "claim" && claimIds(l.line)[0] === edit.claim);
        if (at) {
          text = at.end < text.length ? text.slice(0, at.start) + text.slice(at.end + 1)
            : text.slice(0, Math.max(at.start - 1, 0));
        }
        continue;
      }
      const at = lines.find((l) => kindOf(l.line) === "step" && parseStep(l.line).id === (edit.step ?? edit.after));
      if (!at) continue;
      let line = `${at.line}\n${edit.line}`;
      if (edit.do !== "add_line") {
        const step = parseStep(at.line);
        (edit.do === "add" ? setOption : removeOption)(step, edit.option);
        line = at.line.match(/^\s*/)[0] + formatStep(step);
      }
      text = text.slice(0, at.start) + line + text.slice(at.end);
    }
    return text;
  }

  return { addPart, addLine, applyFix, stepIds, claimIds, parseStep, formatStep, ERRORS };
})();
