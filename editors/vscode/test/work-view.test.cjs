const assert = require("node:assert/strict");
const test = require("node:test");
const { loadWorkbench } = require("./support/workbench.cjs");
const { isStaleIntegrationCli, parseMergeAttempt } = require("../out/mergeAttempt");

// The release contract pins this compatibility copy verbatim.
const STALE_INTEGRATION_CLI_MESSAGE =
  "This StateRoot CLI predates the integration contracts — update the CLI to use coordinated integration here.";

function occurrences(haystack, needle) {
  return haystack.split(needle).length - 1;
}

const BASE = {
  tab: "work",
  delegations: [],
  work: [],
  selectedForks: [],
  roots: [],
  integrationStale: false,
};

test("lineage keeps graph rails and makes missing topology actionable", () => {
  const { push } = loadWorkbench();
  const roots = [{ id: "a".repeat(40), parents: [], mainline: true }];
  const fallback = push({ ...BASE, tab: "lineage", roots, lineageUnavailable: true });
  assert.ok(fallback.includes("lineage connections could not be loaded"));
  assert.ok(fallback.includes('data-act="refresh"'));
  const recovered = push({ ...BASE, tab: "lineage", roots, lineageUnavailable: false,
    lineage: { schema_version: "stateroot.lineage.v1", trunk: { tip: roots[0].id }, roots, forks: [] } });
  assert.ok(recovered.includes('class="rail"'));
  assert.ok(!recovered.includes("could not be loaded"));
});

test("lineage receipt and fidelity actions preserve full IDs and render CLI provenance", () => {
  const { panel, posted, push } = loadWorkbench();
  const id = "a".repeat(40);
  const html = push({ ...BASE, tab: "lineage", rootA: id, roots: [{ id }], compareText: '{"activity":{"provenance":"author_statement"},"coverage":"unknown"}' });
  assert.ok(html.includes(id), "full root remains available in tooltip");
  assert.ok(html.includes("author_statement"));
  for (const act of ["receipt", "fidelity", "openOriginal", "openSession", "openEvent"]) {
    assert.ok(html.includes(`data-act="${act}"`));
    panel.listeners.click({ target: { nodeType: 1, closest: () => ({ getAttribute: (key) => key === "data-act" ? act : null }) } });
    assert.equal(posted.at(-1).type, act);
  }
});

const ATTENTION_ATTEMPT = {
  schema_version: "stateroot.merge-attempt.v1",
  id: "ma-attn-1",
  created_at: "2026-09-17T11:00:00Z",
  harness: "claude",
  trunk_tip: "a".repeat(40),
  forks: [
    { name: "f1", tip: "b".repeat(40) },
    { name: "f2", tip: "c".repeat(40) },
  ],
  state: "attention",
  conflicts: [
    { fork: "f1", path: "src/merge.ts", kind: "both_modified" },
    { fork: "f2", path: "docs/guide.md", kind: "deleted_by_trunk" },
  ],
  worktree: "/tmp/stateroot-ma-attn-1",
};

const READY_ATTEMPT = {
  schema_version: "stateroot.merge-attempt.v1",
  id: "ma-ready-1",
  created_at: "2026-09-17T10:00:00Z",
  harness: "kimi",
  trunk_tip: "a".repeat(40),
  forks: [{ name: "f1", tip: "b".repeat(40) }],
  state: "ready",
  conflicts: [],
};

test("attention attempt renders badge, worktree, conflict rows, and handoff commands", () => {
  const { push } = loadWorkbench();
  const html = push({ ...BASE, attempt: ATTENTION_ATTEMPT });
  assert.ok(html.includes(">Changes need reconciliation<"), "state badge renders");
  assert.ok(html.includes("/tmp/stateroot-ma-attn-1"), "worktree path renders");
  assert.ok(html.includes("src/merge.ts"));
  assert.ok(html.includes("both_modified"));
  assert.ok(html.includes("docs/guide.md"));
  assert.ok(html.includes("deleted_by_trunk"));
  assert.ok(html.includes("stateroot merge --status ma-attn-1 --json"));
  assert.ok(
    html.includes('data-cmd="stateroot merge --continue ma-attn-1 --evidence &quot;&lt;tests run&gt;&quot;"'),
    "continue --evidence command is copy-friendly"
  );
  assert.ok(html.includes("stateroot merge --abort ma-attn-1"));
  assert.ok(html.includes("Reconciliation handoff"));
  assert.ok(html.includes("never chooses resolutions"), "the no-semantic-decisions note stays");
});

test("ready attempt renders the publish command and no reconciliation handoff", () => {
  const { push } = loadWorkbench();
  const html = push({ ...BASE, attempt: READY_ATTEMPT });
  assert.ok(html.includes(">Prepared for review<"), "state badge renders");
  assert.ok(html.includes('data-cmd="stateroot merge --continue ma-ready-1"'), "publish command renders");
  assert.ok(!html.includes("--abort"), "no abort command for a clean attempt");
  assert.ok(!html.includes("--evidence"), "no evidence flag for a clean attempt");
  assert.ok(!html.includes("Reconciliation handoff"));
});

test("no stored attempt renders no attempt block", () => {
  const { push } = loadWorkbench();
  const html = push(BASE);
  assert.ok(!html.includes("merge --continue"));
  assert.ok(!html.includes("merge --abort"));
  assert.ok(!html.includes("Reconciliation handoff"));
  assert.ok(!html.includes("Publish"));
});

test("staged candidate offers final checks without claiming verified publication", () => {
  const { push } = loadWorkbench();
  const tree = "d".repeat(40);
  const html = push({ ...BASE, attempt: { ...READY_ATTEMPT, state: "staged", staged_tree: tree, worktree: "/tmp/candidate" } });
  assert.ok(html.includes("Candidate staged"));
  assert.ok(html.includes(tree));
  assert.ok(html.includes("not evidence that source checks passed"));
  assert.ok(html.includes('data-cmd="stateroot merge --stage ma-ready-1"'));
  assert.ok(html.includes("restage any new verification records"));
  assert.ok(!html.includes("No conflicts —"));
});

test("a stale integration CLI renders the compat message exactly once", () => {
  // Drive it from the failure signature, as prepareMerge does.
  let stale = false;
  try {
    parseMergeAttempt(JSON.stringify({ schema_version: "stateroot.merge-attempt.v0" }));
    assert.fail("guard must throw");
  } catch (err) {
    stale = isStaleIntegrationCli(err);
  }
  assert.equal(stale, true);
  const { push } = loadWorkbench();
  const html = push({ ...BASE, integrationStale: stale });
  assert.equal(occurrences(html, STALE_INTEGRATION_CLI_MESSAGE), 1, "message renders exactly once");
  const again = push({ ...BASE, integrationStale: stale, attempt: READY_ATTEMPT });
  assert.equal(occurrences(again, STALE_INTEGRATION_CLI_MESSAGE), 1, "still once alongside a stored attempt");
  const fresh = push(BASE);
  assert.equal(occurrences(fresh, STALE_INTEGRATION_CLI_MESSAGE), 0, "absent when the CLI is current");
});

test("cleanup_pending fork shows the merge --cleanup retry command", () => {
  const { push } = loadWorkbench();
  const html = push({
    ...BASE,
    work: [
      {
        fork: {
          name: "f-clean",
          ref: "refs/stateroot/forks/f-clean",
          contained: false,
          cleanup: { state: "pending", pending: "worktree still registered" },
        },
        attempts: [],
        phase: "cleanup_pending",
        cleanupCommand: "stateroot merge --cleanup f-clean",
      },
    ],
  });
  assert.ok(html.includes("cleanup_pending"));
  assert.ok(html.includes("worktree still registered"), "pending detail still renders");
  assert.ok(
    html.includes('data-cmd="stateroot merge --cleanup f-clean"'),
    "retry command is copy-friendly"
  );
});

test("copy buttons post copyCmd to the extension — nothing executes in the webview", () => {
  const { panel, posted, push } = loadWorkbench();
  push({ ...BASE, attempt: ATTENTION_ATTEMPT });
  const button = {
    getAttribute: (name) =>
      name === "data-act" ? "copyCmd" : name === "data-cmd" ? "stateroot merge --abort ma-attn-1" : null,
  };
  panel.listeners.click({ target: { nodeType: 1, closest: () => button } });
  const copy = posted.filter((msg) => msg.type === "copyCmd");
  assert.equal(copy.length, 1);
  assert.equal(copy[0].text, "stateroot merge --abort ma-attn-1");
  assert.ok(
    !posted.some((msg) => msg.type === "continue" || msg.type === "abort"),
    "the webview never asks the extension to run continue/abort"
  );
});

test("render preserves panel scroll position across poll pushes", () => {
  const { panel, push } = loadWorkbench();
  let top = 0;
  let left = 0;
  let html = "";
  Object.defineProperty(panel, "scrollTop", {
    get: () => top,
    set: (v) => { top = v; },
  });
  Object.defineProperty(panel, "scrollLeft", {
    get: () => left,
    set: (v) => { left = v; },
  });
  // A real DOM resets scroll when innerHTML is replaced; emulate that.
  Object.defineProperty(panel, "innerHTML", {
    get: () => html,
    set: (v) => { html = v; top = 0; left = 0; },
  });
  // The split views scroll on an inner `.list` element, not the panel — and
  // a real innerHTML replacement resets that inner scroller too.
  let listTop = 0;
  const listEl = {
    get scrollTop() { return listTop; },
    set scrollTop(v) { listTop = v; },
    scrollLeft: 0,
  };
  Object.defineProperty(panel, "innerHTML", {
    get: () => html,
    set: (v) => { html = v; top = 0; left = 0; listTop = 0; },
  });
  panel.querySelector = (sel) => (sel === ".list" ? listEl : null);
  push({ ...BASE, tab: "lineage" });
  panel.scrollTop = 480;
  listTop = 640;
  push({ ...BASE, tab: "lineage" });
  assert.equal(panel.scrollTop, 480, "scrollTop preserved across re-render");
  assert.equal(listEl.scrollTop, 640, "inner .list scrollTop preserved across re-render");
});


test("lineage pages legacy history locally and keeps comparison selections across refresh and page boundaries", () => {
  const { panel, posted, push } = loadWorkbench();
  const roots = Array.from({ length: 103 }, (_, i) => ({ id: "root-" + i, mainline: true, parents: [] }));
  const click = offset => panel.listeners.click({ target: { nodeType: 1, closest: () => ({
    getAttribute: key => key === "data-act" ? "lineagePage" : key === "data-id" ? String(offset) : null,
  }) } });
  let html = push({ ...BASE, tab: "lineage", roots, rootA: "root-0", rootB: "root-102" });
  assert.equal(occurrences(html, 'class="item rail-row'), 50);
  assert.ok(html.includes('Roots 1–50 of 103'));
  panel.scrollTop = 500;
  click(50);
  html = panel.innerHTML;
  assert.equal(panel.scrollTop, 0, "explicit navigation starts at the new page top");
  assert.ok(html.includes('Roots 51–100 of 103'));
  assert.ok(html.includes('data-id="root-50"'));
  assert.ok(!html.includes('data-act="selectRoot" data-id="root-0"'));
  assert.ok(html.includes('title="root-0"') && html.includes('title="root-102"'), "cross-page selections remain usable");
  html = push({ roots });
  assert.ok(html.includes('Roots 51–100 of 103'), "poll preserves page");
  html = push({ roots: [{ id: "inserted" }, ...roots] });
  assert.ok(html.includes('Roots 52–101 of 104'), "new roots preserve the viewed anchor");
  click(100);
  assert.equal(occurrences(panel.innerHTML, 'class="item rail-row'), 4);
  html = push({ roots: roots.slice(0, 1) });
  assert.ok(html.includes('Roots 1–1 of 1'), "shrinking histories clamp safely");
  assert.ok(!posted.some(msg => msg.type === "lineagePage"), "legacy pagination needs no CLI reads");
});

test("paged lineage requests metadata while retaining topology across page boundaries", () => {
  const { panel, posted, push } = loadWorkbench();
  const all = Array.from({ length: 52 }, (_, i) => ({ id: "r" + i, mainline: true, parents: i < 51 ? ["r" + (i + 1)] : [] }));
  const lineage = { schema_version: "stateroot.lineage.v1", trunk: { tip: "r0" }, forks: [],
    roots: all.slice(0, 50), topology: all,
    pagination: { schema_version: "stateroot.lineage.pagination.v1", offset: 0, limit: 50, total: 52, has_more: true, history: "h1" } };
  let html = push({ ...BASE, tab: "lineage", lineage });
  assert.ok(html.includes('Roots 1–50 of 52'));
  panel.listeners.click({ target: { nodeType: 1, closest: () => ({ getAttribute: key => key === "data-act" ? "lineagePage" : key === "data-id" ? "50" : null }) } });
  assert.equal(posted.at(-1).type, "lineagePage");
  assert.equal(posted.at(-1).offset, 50);
  html = push({ lineageLoading: true });
  assert.ok(html.includes("Loading…"));
  html = push({ lineageLoading: false, lineage: { ...lineage, roots: all.slice(50), pagination: { ...lineage.pagination, offset: 50, has_more: false } } });
  assert.equal(occurrences(html, 'class="item rail-row'), 2);
  assert.ok(html.includes('Roots 51–52 of 52'));
  assert.ok(html.includes('Connections continue beyond this page.'));
  html = push({ lineageLoadError: "Page unavailable; retry navigation." });
  assert.ok(html.includes("Page unavailable"));
  assert.equal(occurrences(html, 'class="item rail-row'), 2, "failure retains the last good page");
});
