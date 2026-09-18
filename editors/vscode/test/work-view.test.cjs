const assert = require("node:assert/strict");
const test = require("node:test");
const vm = require("node:vm");

// out/ui.js is vscode-free at runtime (its only import chain is type-only),
// so it loads directly like out/setup does in setup.test.cjs.
const { workbenchHtml } = require("../out/ui");
const { isStaleIntegrationCli, parseMergeAttempt } = require("../out/mergeAttempt");

// The release contract pins this compatibility copy verbatim.
const STALE_INTEGRATION_CLI_MESSAGE =
  "This StateRoot CLI predates the integration contracts — update the CLI to use coordinated integration here.";

function occurrences(haystack, needle) {
  return haystack.split(needle).length - 1;
}

/** Run the real workbench client script with a minimal DOM stub, then drive
 * it with posted snapshot state and inspect the rendered panel HTML. */
function loadWorkbench() {
  const html = workbenchHtml("testnonce");
  const match = html.match(/<script nonce="testnonce">([\s\S]*?)<\/script>/);
  assert.ok(match, "workbench HTML carries a nonce'd script");
  const panel = {
    innerHTML: "",
    listeners: {},
    scrollTop: 0,
    scrollLeft: 0,
    querySelector: () => null,
    addEventListener(type, fn) {
      this.listeners[type] = fn;
    },
  };
  const posted = [];
  let messageHandler;
  const sandbox = {
    acquireVsCodeApi: () => ({
      postMessage: (msg) => posted.push(msg),
      getState: () => ({}),
      setState: () => {},
    }),
    window: {
      addEventListener: (type, fn) => {
        if (type === "message") messageHandler = fn;
      },
    },
    document: {
      querySelectorAll: () => [],
      getElementById: (id) => (id === "panel" ? panel : null),
    },
  };
  vm.runInNewContext(match[1], sandbox);
  assert.equal(typeof messageHandler, "function");
  const push = (data) => {
    messageHandler({ data });
    return panel.innerHTML;
  };
  return { panel, posted, push };
}

const BASE = {
  tab: "work",
  delegations: [],
  work: [],
  selectedForks: [],
  roots: [],
  integrationStale: false,
};

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
  assert.ok(html.includes(">attention<"), "state badge renders");
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
  assert.ok(html.includes(">ready<"), "state badge renders");
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

test("a stale integration CLI renders the compat message exactly once", () => {
  // Drive it from the failure signature, as prepareMerge does.
  let stale = false;
  try {
    parseMergeAttempt("Merged forks: f1, f2");
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
