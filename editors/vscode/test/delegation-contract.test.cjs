const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const cp = require("node:child_process");
const Module = require("node:module");
const { loadWorkbench } = require("./support/workbench.cjs");

let root;
const originalLoad = Module._load;
Module._load = function (request) {
  if (request === "vscode") return { workspace: { get workspaceFolders() { return [{ uri: { fsPath: root } }]; } } };
  return originalLoad.apply(this, arguments);
};
const { parseDelegateList } = require("../out/cli");
const { snapshot } = require("../out/snapshot");
const { assembleInbox, delegationTargetsClosedPlan } = require("../out/inbox");
const { listWikiPages, wikiPagePath } = require("../out/store");
const { deriveParallelWork } = require("../out/parallelWork");
Module._load = originalLoad;

test("legacy attention uses bound worker evidence without probing foreign processes", () => {
  const plan = { id: "plan_fixture", title: "Fixture", status: "approved" };
  for (const status of ["running", "starting", "unknown", "completed", "cancelling"]) {
    const delegations = [{ id: "worker", harness: "codex", plan_id: plan.id, task: "no task reference", status }];
    assert.deepEqual(assembleInbox({ plans: [plan], delegations }), [], status);
  }
  const failed = [{ id: "worker", harness: "codex", plan_id: plan.id, task: "", outcome: "failed", status: "running" }];
  assert.deepEqual(assembleInbox({ plans: [plan], delegations: failed }).map(row => row.kind), ["choose-executor", "reassign"]);
  assert.deepEqual(assembleInbox({ plans: [plan], delegations: failed, continuity: { attention: [] } }), [], "authoritative empty result wins");
  const closed = { ...plan, id: "closed", status: "done" };
  assert.equal(delegationTargetsClosedPlan({ ...failed[0], plan_id: closed.id }, [closed]), true);
  assert.equal(delegationTargetsClosedPlan({ ...failed[0], task: "mentions closed" }, [plan, closed]), false, "structural binding wins over incidental task references");
});

test("wiki listing and opening share modern precedence while retaining legacy pages", t => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-wiki-contract-"));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  for (const dir of ["wiki", "memories"]) {
    fs.mkdirSync(path.join(temp, ".stateroot", dir, "pages"), { recursive: true });
    fs.writeFileSync(path.join(temp, ".stateroot", dir, "pages", "shared.md"), dir);
  }
  fs.writeFileSync(path.join(temp, ".stateroot", "memories", "pages", "legacy.md"), "legacy");
  assert.deepEqual(listWikiPages(temp).map(row => row.rel), ["legacy.md", "shared.md"]);
  assert.equal(fs.readFileSync(wikiPagePath(temp, "shared.md"), "utf8"), "wiki");
  assert.equal(fs.readFileSync(wikiPagePath(temp, "legacy.md"), "utf8"), "legacy");
});

test("parallel work honors terminal outcomes before stale running or cancelling fields", () => {
  const lineage = { forks: [{ name: "fixture", contained: false }] };
  for (const status of ["running", "cancelling"]) {
    const rec = { id: "worker", fork_id: "fixture", status, outcome: "failed" };
    assert.equal(deriveParallelWork(lineage, [rec])[0].phase, "attention");
    assert.equal(deriveParallelWork(lineage, [{ ...rec, outcome_root: "a".repeat(40) }])[0].phase, "ready");
  }
  assert.equal(deriveParallelWork(lineage, [{ id: "worker", fork_id: "fixture", status: "unknown", outcome_root: "old-captured-root" }])[0].phase, "unknown", "an older capture never proves an unknown current worker ready");
});

const binary = process.env.STATEROOT_CLI_TEST_BINARY;
test("real CLI delegation report composes through snapshot, attention and existing work cards", { skip: !binary }, t => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-extension-contract-"));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  root = path.join(temp, "project");
  fs.mkdirSync(root);
  const env = { ...process.env, STATEROOT_HOME: path.join(temp, "config"), STATEROOT_TEST_HOME: path.join(temp, "user"), STATEROOT_TEST_CMD_PROBES: "" };
  for (const name of ["DEEPSEEK_API_KEY", "OPENAI_API_KEY", "STATEROOT_SYNTHESIS_API_KEY", "STATEROOT_SYNTHESIS_API_BASE"]) delete env[name];
  const cli = (args, input) => cp.execFileSync(binary, args, { cwd: root, env, input, stdio: ["pipe", "pipe", "pipe"], encoding: "utf8", timeout: 60000 });
  cli(["init"]);
  const planOutput = cli(["plan", "record", "--stdin", "--title", "Extension contract fixture"], "# Fixture\n\nKeep unknown workers visible.\n");
  const planId = planOutput.match(/plan_\d{4}-[^\s`]+/)[0];
  cli(["plan", "approve", planId]);
  assert.throws(() => cli(["plan", "done", planId]), /evidence/, "the real completion contract requires evidence");
  const dir = path.join(root, ".stateroot", "delegations");
  fs.mkdirSync(dir, { recursive: true });
  const fixture = {
    "ordinary.json": JSON.stringify({ id: "ordinary", harness: "codex", status: "running", outcome: "completed", task: "task with · separator", fork_id: "finished", outcome_root: "b".repeat(40), ts: "2026-10-23T00:00:01Z" }),
    "unknown.json": JSON.stringify({ id: "foreign", harness: "kimi", status: "running", pid: 4294967294, process_namespace: "foreign-runtime", plan_id: planId, fork_id: "unverified", task: "execute bound plan", ts: "2026-10-23T00:00:02Z" }),
    "broken.json": "{broken fixture bytes",
    "future.json": JSON.stringify({ schema_version: "future", id: "must-not-be-a-worker", harness: "codex" }),
    "idless.json": JSON.stringify({ task: "preserve idless" }),
  };
  for (const [file, bytes] of Object.entries(fixture)) fs.writeFileSync(path.join(dir, file), bytes);
  const text = cli(["delegate", "list"]);
  assert.match(text, /broken.json · degraded · unparseable JSON/);
  assert.match(text, /foreign · kimi · unknown/);
  const report = parseDelegateList(text);
  assert.equal(report.records.length, 2, "degraded observations never become workers");
  assert.equal(report.degraded.length, 3);
  // Direct fixture writes bypass normal CLI mutations. Match the existing
  // Rust continuity fixture: status explicitly refreshes the projection.
  cli(["status", "--json"]);
  const state = snapshot({ liveDelegations: report, lineage: { schema_version: "stateroot.lineage.v1", trunk: {}, roots: [], forks: [{ name: "finished", contained: false }, { name: "unverified", contained: false }] } });
  assert.equal(state.delegations.find(row => row.id === "ordinary").status, "completed");
  assert.equal(state.delegations.find(row => row.id === "ordinary").task, "task with · separator");
  assert.equal(state.delegations.find(row => row.id === "foreign").status, "unknown");
  assert.equal(state.delegations.length, 2);
  assert.equal(state.degradedDelegations.length, 3);
  assert.equal(state.work[0].phase, "ready", "terminal outcome beats recorded running status");
  assert.equal(state.work[1].phase, "unknown");
  assert.ok(!state.inbox.some(row => ["choose-executor", "plan_unassigned", "reassign", "plan_closure"].includes(row.kind)), JSON.stringify({ planId, inbox: state.inbox, continuity: state.continuity }));
  assert.ok(state.inbox.some(row => row.kind === "delegation_status_unknown"));
  assert.ok(state.inbox.some(row => row.kind === "delegation_record_degraded"));
  const planView = loadWorkbench();
  const planHtml = planView.push({ ...state, tab: "plans" });
  assert.ok(planHtml.includes('data-act="planDoneEvidence"'), "plan completion reuses the evidence prompt");
  assert.ok(!planHtml.includes('data-act="donePlan"'));
  planView.panel.listeners.click({ target: { nodeType: 1, closest: () => ({ getAttribute: name => name === "data-act" ? "planDoneEvidence" : name === "data-id" ? planId : null }) } });
  assert.equal(planView.posted.at(-1).type, "planDoneEvidence");
  assert.equal(planView.posted.at(-1).id, planId);
  const html = loadWorkbench().push({ ...state, tab: "work" });
  assert.ok(html.includes("broken.json"));
  assert.ok(html.includes("unparseable JSON"));
  assert.ok(!html.includes('data-id="broken.json"'), "degraded filenames have no worker actions");
  assert.ok(!html.includes("must-not-be-a-worker"));
  for (const [file, bytes] of Object.entries(fixture)) assert.equal(fs.readFileSync(path.join(dir, file), "utf8"), bytes);

  // Changed producer input invalidates the prior unknown assessment. The
  // same consumer must recover real failure guidance on its next refresh.
  const changed = { ...JSON.parse(fixture["unknown.json"]), outcome: "failed" };
  fs.writeFileSync(path.join(dir, "unknown.json"), JSON.stringify(changed));
  const nextReport = parseDelegateList(cli(["delegate", "list"]));
  cli(["status", "--json"]);
  const next = snapshot({ liveDelegations: nextReport });
  assert.equal(next.delegations.find(row => row.id === "foreign").status, "failed");
  assert.ok(next.inbox.some(row => row.kind === "delegation_failed"));
  cli(["plan", "done", planId, "--evidence", "Fixture completed: parser, snapshot, inbox and card assertions passed"]);
  const completed = snapshot({ liveDelegations: nextReport });
  assert.equal(completed.plans.find(row => row.id === planId).status, "done");
  assert.equal(completed.delegations.find(row => row.id === "foreign").closedPlan, true);
  assert.ok(!completed.inbox.some(row => row.kind === "delegation_failed"));
});
