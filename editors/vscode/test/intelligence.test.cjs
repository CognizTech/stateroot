const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const { spawnSync } = require("node:child_process");
const { emptyInventory, parseInventory, parseMcpDiagnosis } = require("../out/intelligence");
const { parsePlanGraph } = require("../out/planGraph");
const { loadWorkbench } = require("./support/workbench.cjs");

test("inventory keeps identity, native route and unavailable evidence distinct", () => {
  const skill = parseInventory("skills", JSON.stringify([
    {
      identity_key: "psi-one", slug: "portable", harness: "codex", scope: "project",
      lifecycle: "active", package_digest: "digest-one", source_path: "package/portable",
      compatibility: { compatible: true, reasons: [] },
    },
    {
      identity_key: "psi-two", slug: "foreign", harness: "cursor", scope: "global",
      lifecycle: "reference_only", package_digest: "digest-two", source_path: "package/foreign",
      native_invocation: "stateroot delegate --to cursor --skill foreign --task <microtask>",
      compatibility: { compatible: false, reasons: ["missing binary"] },
    },
  ])).skills;
  assert.equal(skill[0].identity, "psi-one");
  assert.equal(skill[1].lifecycle, "reference_only");
  assert.match(skill[1].nativeRoute, /delegate --to cursor/);
  assert.deepEqual(skill[1].compatibilityReasons, ["missing binary"]);
  assert.equal(emptyInventory().sources.health.state, "unavailable");
  assert.throws(() => parseInventory("skills", JSON.stringify({ skills: [] })), /no array/);
  const conflict = parseMcpDiagnosis(JSON.stringify({
    issues: [{ action: "collision", name: "filesystem", detail: "two origins differ" }],
    actions_preview: [{ action: "conflict", name: "filesystem", detail: "resolve before projection" }],
  }));
  assert.equal(conflict.mcpIssues[0].action, "collision");
  assert.equal(conflict.mcpActions[0].name, "filesystem");
});

test("workbench labels unknown use and preserves a blocked plan without executing it", () => {
  const inventory = emptyInventory();
  inventory.sources.skills = { state: "ready" };
  inventory.skills = [{
    identity: "psi-safe", slug: "<script>bad</script>", origin: "cursor", scope: "global",
    lifecycle: "reference_only", digest: "digest", path: "foreign/skill",
    compatible: false, compatibilityReasons: ["missing native binary"], nativeRoute: "delegate to cursor",
  }];
  const { push } = loadWorkbench();
  const html = push({ tab: "intelligence", intelligence: inventory });
  assert.match(html, /&lt;script&gt;bad&lt;\/script&gt;/);
  assert.doesNotMatch(html, /<script>bad<\/script>/);
  assert.match(html, /read\/use\/outcome unknown/);
  assert.match(html, /missing native binary/);

  const blocked = push({
    tab: "plans", plans: [{ id: "plan-one", title: "One", status: "approved", created_by_harness: "codex" }],
    selectedPlanId: "plan-one", harnesses: [],
    planGraph: { observationId: "obs-one", rows: [{ id: "plan-one", readiness: "blocked", workerState: "unassigned", blockers: [{ code: "missing_source", detail: "upstream source not published" }] }] },
  });
  assert.match(blocked, /missing_source: upstream source not published/);

  const work = push({
    tab: "work", delegations: [], work: [], selectedForks: [],
    planGraph: { observationId: "obs-two", rows: [
      { id: "plan-ready", title: "Ready task", status: "approved", readiness: "ready", workerState: "unassigned", blockers: [] },
      { id: "plan-blocked", title: "Blocked task", status: "active", readiness: "blocked", workerState: "unknown", blockers: [{ code: "missing_source", detail: "required root absent" }] },
      { id: "plan-undeclared", title: "Undeclared task", status: "approved", readiness: "dependencies_not_declared", workerState: "unassigned", blockers: [] },
    ] },
  });
  assert.match(work, /Parallel-ready plans \(1\)/);
  assert.match(work, /Blocked approved or active plans \(2\)/);
  assert.match(work, /missing_source: required root absent/);
  assert.match(work, /Undeclared task/);
  assert.match(work, /dependencies_not_declared/);
  assert.match(work, /data-act="openPlan" data-id="plan-blocked"/);
  const unavailable = push({ tab: "work", delegations: [], work: [], selectedForks: [], planGraph: { observationId: "", rows: [], unavailable: "CLI timeout" } });
  assert.match(unavailable, /Plan readiness unavailable: CLI timeout/);
  assert.doesNotMatch(unavailable, /No authorized plan is ready/);
});

test("failed CLI readers remain unavailable rather than appearing empty", () => {
  const inventory = emptyInventory();
  inventory.sources.skills = { state: "unavailable", detail: "CLI returned malformed JSON" };
  const { push } = loadWorkbench();
  const html = push({ tab: "intelligence", intelligence: inventory });
  assert.match(html, /skills: unavailable · CLI returned malformed JSON/);
  assert.doesNotMatch(html, /No Skills reported/);
  assert.doesNotMatch(html, /No MCP servers reported/);
  inventory.sources.skills = { state: "ready" };
  assert.match(push({ tab: "intelligence", intelligence: inventory }), /No Skills reported/);
});

test("plan graph preserves declared prerequisites and assignment in Plans and Work", () => {
  const doc = { schema_version: "stateroot.plan-graph.v1", nodes: [{
    id: "plan-target", title: "Target", status: "approved", readiness: "blocked", worker_state: "unassigned",
    dependencies_declared: true, assignment: { harness: "codex", fork: "work-fork" },
    dependencies: [{ dependency: { plan_id: "plan-upstream", body_digest: "abc", kind: "source" }, state: "missing", reason: "source not published", evidence_basis: "unavailable" }], blockers: [],
  }] };
  const graph = parsePlanGraph(JSON.stringify(doc));
  assert.equal(graph.rows[0].dependencies[0].planId, "plan-upstream");
  assert.equal(graph.rows[0].dependencies[0].bodyDigest, "abc");
  assert.deepEqual(graph.rows[0].assignment, { harness: "codex", fork: "work-fork", automaticDispatch: "not_declared" });
  const { push } = loadWorkbench();
  const plans = push({ tab: "plans", plans: [{ id: "plan-target", title: "Target", status: "approved", created_by_harness: "codex" }], selectedPlanId: "plan-target", harnesses: [], planGraph: graph });
  const work = push({ tab: "work", delegations: [], work: [], selectedForks: [], planGraph: graph });
  for (const html of [plans, work]) {
    assert.match(html, /Assigned harness: codex/);
    assert.match(html, /assigned fork: work-fork/);
    assert.match(html, /plan-upstream · source · missing: source not published/);
  }
  assert.match(push({ tab: "plans", plans: [{ id: "plan-target", title: "Target", status: "approved", created_by_harness: "codex" }], selectedPlanId: "plan-target", harnesses: [], planGraph: { rows: [{ id: "plan-target", readiness: "unknown", workerState: "unknown", blockers: [] }] } }), /Assignment unknown/);
});

test("historical real CLI graph preserves original assignment and prerequisites", { skip: !process.env.STATEROOT_CLI_TEST_BINARY || !process.env.STATEROOT_I7_FIXTURE }, () => {
  const fixture = process.env.STATEROOT_I7_FIXTURE;
  const testHome = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-i7-graph-"));
  try {
    const graph = spawnSync(process.env.STATEROOT_CLI_TEST_BINARY, ["plan", "graph", "--json"], {
      cwd: fixture, env: { ...process.env, STATEROOT_TEST_HOME: testHome, STATEROOT_HOME: path.join(testHome, "config"), STATEROOT_NO_AUTO_UPDATE: "1" }, encoding: "utf8", timeout: 90000,
    });
    assert.equal(graph.status, 0, graph.stderr);
    const raw = JSON.parse(graph.stdout);
    const rawNode = raw.nodes.find(row => row.id.startsWith("plan_2026-10-27T03-17-23Z_i6"));
    const view = parsePlanGraph(graph.stdout);
    const node = view.rows.find(row => row.id === rawNode.id);
    assert.deepEqual(node.dependencies.map(row => row.planId), rawNode.dependencies.map(row => row.dependency.plan_id));
    assert.equal(node.dependencies.length, 5);
    assert.equal(node.assignment.harness, rawNode.assignment.harness);
    const html = loadWorkbench().push({ tab: "plans", plans: [{ id: node.id, title: node.title, status: node.status, created_by_harness: "codex" }], selectedPlanId: node.id, harnesses: [], planGraph: view });
    assert.match(html, /Assigned harness: codex/);
    for (const item of node.dependencies) assert.ok(html.includes(item.planId));
  } finally { fs.rmSync(testHome, { recursive: true, force: true }); }
});

test("structured inventory readers agree with a real CLI", { skip: !process.env.STATEROOT_CLI_TEST_BINARY }, () => {
  const binary = process.env.STATEROOT_CLI_TEST_BINARY;
  const cwd = path.resolve(__dirname, "../../..");
  const testHome = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-intelligence-"));
  const env = { ...process.env, STATEROOT_TEST_HOME: testHome, STATEROOT_NO_AUTO_UPDATE: "1" };
  const calls = {
    skills: ["skill", "scan", "--json"],
    rules: ["rules", "list", "--json"],
    mcp: ["mcp", "status", "--json"],
    mcpHealth: ["mcp", "doctor", "--json"],
    tools: ["mcp", "tools", "--json"],
    health: ["doctor", "--json"],
  };
  try {
    for (const [kind, args] of Object.entries(calls)) {
      const result = spawnSync(binary, args, { cwd, env, encoding: "utf8", timeout: 90000 });
      assert.equal(result.status, 0, `${kind}: ${result.stderr}`);
      if (kind === "mcpHealth") {
        const parsed = parseMcpDiagnosis(result.stdout);
        assert.ok(Array.isArray(parsed.mcpIssues));
        assert.ok(Array.isArray(parsed.mcpActions));
      } else {
        const parsed = parseInventory(kind, result.stdout);
        const key = kind === "health" ? "harnesses" : kind;
        assert.ok(Array.isArray(parsed[key]), `${kind} reader must return a list`);
      }
    }
    const graph = spawnSync(binary, ["plan", "graph", "--json"], { cwd, env, encoding: "utf8", timeout: 90000 });
    assert.equal(graph.status, 0, `plan graph: ${graph.stderr}`);
    const parsedGraph = parsePlanGraph(graph.stdout);
    assert.ok(parsedGraph.rows.every(row => row.id && row.readiness));
    const openPlan = parsedGraph.rows.find(row =>
      (row.status === "approved" || row.status === "active") && row.readiness !== "ready" && row.readiness !== "complete"
    );
    assert.ok(openPlan, "the real project graph has an open non-ready plan");
    const workHtml = loadWorkbench().push({ tab: "work", delegations: [], work: [], selectedForks: [], planGraph: parsedGraph });
    assert.ok(workHtml.includes(openPlan.id), "the real CLI graph reaches the Work view");

    const project = fs.mkdtempSync(path.join(testHome, "project-"));
    const init = spawnSync(binary, ["init", project], { cwd: project, env, encoding: "utf8", timeout: 90000 });
    assert.equal(init.status, 0, `init: ${init.stderr}`);
    fs.mkdirSync(path.join(testHome, ".codex"), { recursive: true });
    fs.writeFileSync(path.join(testHome, ".codex", "AGENTS.md"), "# Global same slug\nGlobal instruction.\n");
    fs.writeFileSync(path.join(project, "AGENTS.md"), "# Project same slug\nProject instruction.\n");
    const sync = spawnSync(binary, ["rules", "sync"], { cwd: project, env, encoding: "utf8", timeout: 90000 });
    assert.equal(sync.status, 0, `rules sync: ${sync.stderr}`);
    const listed = spawnSync(binary, ["rules", "list", "--json"], { cwd: project, env, encoding: "utf8", timeout: 90000 });
    assert.equal(listed.status, 0, `rules list: ${listed.stderr}`);
    const sameSlug = parseInventory("rules", listed.stdout).rules.filter(row => row.slug === "codex-agents");
    assert.deepEqual(sameSlug.map(row => row.scope), ["user", "project"], "equal slugs retain distinct scope and provenance");

    const cursorSkills = path.join(testHome, ".cursor", "skills", "portable-example");
    fs.mkdirSync(cursorSkills, { recursive: true });
    fs.writeFileSync(path.join(cursorSkills, "SKILL.md"), "---\nname: portable-example\ndescription: example\n---\n# Portable example\n");
    const scanned = spawnSync(binary, ["skill", "scan", "--json"], { cwd: project, env, encoding: "utf8", timeout: 90000 });
    assert.equal(scanned.status, 0, `skill scan: ${scanned.stderr}`);
    const example = parseInventory("skills", scanned.stdout).skills.find(row => row.slug === "portable-example");
    assert.ok(example, "real Skill source is discovered");
    assert.equal(example.scope, "global");
    assert.equal(example.origin, "cursor");

    fs.writeFileSync(path.join(testHome, ".cursor", "mcp.json"), JSON.stringify({ mcpServers: { collision: { command: "one" } } }));
    fs.writeFileSync(path.join(testHome, ".claude.json"), JSON.stringify({ mcpServers: { collision: { command: "two" } } }));
    const diagnosis = spawnSync(binary, ["mcp", "doctor", "--json"], { cwd: project, env, encoding: "utf8", timeout: 90000 });
    assert.equal(diagnosis.status, 0, `mcp doctor: ${diagnosis.stderr}`);
    const conflict = parseMcpDiagnosis(diagnosis.stdout).mcpIssues;
    assert.ok(conflict.some(row => row.name === "collision" && row.action === "collision"), "real conflicting origins stay visible");
  } finally {
    fs.rmSync(testHome, { recursive: true, force: true });
  }
});
