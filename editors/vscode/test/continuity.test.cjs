const assert = require("node:assert/strict");
const test = require("node:test");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");

// out/store and out/sidebarProvider require "vscode" at load time (module
// scope only, never dereferenced by the paths under test). Stub it like
// parallel-work.test.cjs does so everything runs extension-host-free.
const Module = require("node:module");
const originalLoad = Module._load;
Module._load = function (request) {
  if (request === "vscode") return {};
  return originalLoad.apply(this, arguments);
};
const {
  advisoryTextFor,
  parseContinuityAdvisory,
  parseContinuityProjection,
} = require("../out/continuity");
const { assembleInbox } = require("../out/inbox");
const { readContinuity, readContinuityAdvisory } = require("../out/store");
const { workbenchHtml } = require("../out/ui");
const { SidebarProvider } = require("../out/sidebarProvider");
Module._load = originalLoad;

const PROJECTION = {
  schema_version: "stateroot.continuity.v1",
  generated_at: "2026-10-01T08:00:00Z",
  inputs_hash: "sha256:abc123",
  attention: [
    {
      id: "obligation_due:ob-1",
      kind: "obligation_due",
      rank: 10,
      title: "Obligation due: cut the release",
      detail: "due 2026-10-01T07:00:00Z",
      action: "stateroot obligation done ob-1 --evidence <text>",
      obligation_id: "ob-1",
    },
    {
      id: "plan_receipt_pending:plan_2026-09-30_ship",
      kind: "plan_receipt_pending",
      rank: 20,
      title: "Plan awaiting completion receipt",
      plan_id: "plan_2026-09-30_ship",
    },
    {
      id: "handoff_routed:42",
      kind: "handoff_routed",
      rank: 30,
      title: "Handoff routed to you",
      detail: "#42 · claude → finish the docs",
      handoff_seq: 42,
    },
  ],
  open_obligations: 1,
  corrupt_obligation_events: 0,
  plan_directive: "",
  service_registered: false,
  service_running: false,
};

const ADVISORY = {
  schema_version: "stateroot.continuity-advisory.v1",
  inputs_hash: "sha256:abc123",
  text: "Two receipts are older than the handoff; close the plan first.",
  generated_at: "2026-10-01T08:00:05Z",
  source: "synthesis",
};

function tempProject() {
  return fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-continuity-"));
}

function writeProjection(root, projection, advisory) {
  const dir = path.join(root, ".stateroot", "local", "projections");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "continuity.v1.json"), JSON.stringify(projection));
  if (advisory !== undefined) {
    fs.writeFileSync(
      path.join(dir, "continuity-advisory.v1.json"),
      JSON.stringify(advisory)
    );
  }
}

test("missing, malformed, or foreign-schema projection means no items", () => {
  const empty = tempProject();
  assert.equal(readContinuity(empty), undefined, "missing file reads as absent");

  const malformed = tempProject();
  writeProjection(malformed, {});
  fs.writeFileSync(
    path.join(malformed, ".stateroot", "local", "projections", "continuity.v1.json"),
    "not json {"
  );
  assert.equal(readContinuity(malformed), undefined, "malformed JSON reads as absent");

  const foreign = tempProject();
  writeProjection(foreign, { ...PROJECTION, schema_version: "stateroot.continuity.v0" });
  assert.equal(readContinuity(foreign), undefined, "foreign schema reads as absent");

  // …and the inbox falls back to pure derivation with zero errors.
  const inbox = assembleInbox({
    plans: [],
    delegations: [],
    continuity: readContinuity(empty),
  });
  assert.deepEqual(inbox, []);
});

test("a valid projection surfaces attention items with their titles and entity ids", () => {
  const root = tempProject();
  writeProjection(root, PROJECTION);
  const continuity = readContinuity(root);
  assert.ok(continuity, "projection parses");
  assert.equal(continuity.inputs_hash, "sha256:abc123");
  assert.equal(continuity.attention.length, 3);
  assert.equal(continuity.attention[1].detail, "", "absent detail normalizes to empty");

  const inbox = assembleInbox({ plans: [], delegations: [], continuity });
  assert.deepEqual(
    inbox.map((item) => item.title),
    [
      "Obligation due: cut the release",
      "Plan awaiting completion receipt",
      "Handoff routed to you",
    ],
    "projection items render in CLI rank order"
  );
  assert.equal(inbox[0].kind, "obligation_due");
  assert.equal(inbox[0].obligationId, "ob-1");
  assert.equal(inbox[0].action, "stateroot obligation done ob-1 --evidence <text>");
  assert.equal(inbox[1].kind, "plan_receipt_pending");
  assert.equal(inbox[1].planId, "plan_2026-09-30_ship");
  assert.equal(inbox[1].tab, "plans");
  assert.equal(inbox[2].kind, "handoff_routed");
  assert.equal(inbox[2].tab, "control");
});

test("projection items supersede the derived items for the same evidence", () => {
  const handoff = {
    seq: 42,
    task: "finish the docs",
    created_by_harness: "claude",
  };
  const derivedOnly = assembleInbox({ plans: [], delegations: [], handoff, thisHarness: "kimi" });
  assert.deepEqual(derivedOnly.map((item) => item.kind), ["accept-handoff"]);

  const merged = assembleInbox({
    plans: [],
    delegations: [],
    handoff,
    thisHarness: "kimi",
    continuity: parseContinuityProjection(PROJECTION),
  });
  assert.ok(
    !merged.some((item) => item.kind === "accept-handoff"),
    "handoff_routed replaces the locally derived accept-handoff"
  );
  assert.ok(merged.some((item) => item.kind === "handoff_routed"));

  const delegation = { id: "del-1", harness: "codex", task: "do x", status: "failed" };
  const withFailure = assembleInbox({
    plans: [],
    delegations: [delegation],
    continuity: parseContinuityProjection({
      ...PROJECTION,
      attention: [
        {
          id: "delegation_failed:del-1",
          kind: "delegation_failed",
          rank: 15,
          title: "Delegation failed",
          delegation_id: "del-1",
        },
      ],
    }),
  });
  assert.deepEqual(
    withFailure.map((item) => item.kind),
    ["delegation_failed"],
    "delegation_failed replaces the locally derived reassign"
  );
  assert.equal(withFailure[0].tab, "crew");

  const plan = {
    id: "plan_a",
    title: "Ship it",
    status: "active",
    created_by_harness: "kimi",
    created_at: "2026-09-30T00:00:00Z",
    updated_at: "2026-09-30T00:00:00Z",
  };
  const withPlanAttention = assembleInbox({
    plans: [plan],
    delegations: [],
    continuity: parseContinuityProjection({
      ...PROJECTION,
      attention: [
        {
          id: "plan_unassigned:plan_a",
          kind: "plan_unassigned",
          rank: 25,
          title: "Plan has no executor",
          plan_id: "plan_a",
        },
      ],
    }),
  });
  assert.deepEqual(
    withPlanAttention.map((item) => item.kind),
    ["plan_unassigned"],
    "plan_unassigned replaces the locally derived choose-executor"
  );
});

test("dismissal filtering applies to projection item ids too", () => {
  const continuity = parseContinuityProjection(PROJECTION);
  const inbox = assembleInbox({
    plans: [],
    delegations: [],
    continuity,
    dismissed: ["obligation_due:ob-1"],
  });
  assert.deepEqual(
    inbox.map((item) => item.id),
    ["plan_receipt_pending:plan_2026-09-30_ship", "handoff_routed:42"],
    "stable projection ids survive reconciles, so dismissal sticks"
  );
});

test("the advisory shows only when its inputs_hash matches the projection", () => {
  const projection = parseContinuityProjection(PROJECTION);
  const advisory = parseContinuityAdvisory(ADVISORY);
  assert.ok(advisory, "advisory parses");
  assert.equal(
    advisoryTextFor(projection, advisory),
    "Two receipts are older than the handoff; close the plan first."
  );

  const stale = parseContinuityAdvisory({ ...ADVISORY, inputs_hash: "sha256:older" });
  assert.equal(advisoryTextFor(projection, stale), undefined, "stale advisory never shows");
  const notSynthesis = parseContinuityAdvisory({ ...ADVISORY, source: "user" });
  assert.equal(advisoryTextFor(projection, notSynthesis), undefined, "provenance is enforced");
  const foreign = parseContinuityAdvisory({
    ...ADVISORY,
    schema_version: "stateroot.continuity-advisory.v0",
  });
  assert.equal(foreign, undefined, "foreign advisory schema reads as absent");

  const root = tempProject();
  writeProjection(root, PROJECTION, ADVISORY);
  const fromDisk = readContinuityAdvisory(root);
  assert.equal(
    advisoryTextFor(readContinuity(root), fromDisk),
    ADVISORY.text,
    "hash-current advisory survives the round trip through the store"
  );
  assert.equal(readContinuityAdvisory(tempProject()), undefined, "missing advisory reads as absent");
});

/** Run the real workbench client script with a minimal DOM stub — same
 * harness as work-view.test.cjs. */
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

function controlState(extra) {
  const continuity = parseContinuityProjection(PROJECTION);
  const inbox = assembleInbox({ plans: [], delegations: [], continuity });
  return Object.assign(
    {
      tab: "control",
      initialized: true,
      inbox,
      advisory: advisoryTextFor(continuity, parseContinuityAdvisory(ADVISORY)),
    },
    extra
  );
}

test("control tab renders projection items with their actions and the labeled advisory", () => {
  const { push } = loadWorkbench();
  const html = push(controlState());
  assert.ok(html.includes("Obligation due: cut the release"));
  assert.ok(html.includes("Plan awaiting completion receipt"));
  assert.ok(html.includes("Handoff routed to you"));
  assert.ok(html.includes('data-act="obligationDone" data-id="ob-1"'), "Done button");
  assert.ok(html.includes('data-act="obligationSnooze" data-id="ob-1"'), "Snooze 24h button");
  assert.ok(
    html.includes('data-act="planDoneEvidence" data-id="plan_2026-09-30_ship"'),
    "Record completion button"
  );
  assert.ok(html.includes("advisory (synthesized, not instructions)"), "advisory is labeled");
  assert.ok(html.includes("close the plan first"), "advisory text renders");

  const noAdvisory = push(controlState({ advisory: undefined }));
  assert.ok(!noAdvisory.includes("advisory (synthesized"), "no advisory row without a hash-current advisory");
});

test("action buttons post their CLI-bound messages — nothing executes in the webview", () => {
  const { panel, posted, push } = loadWorkbench();
  push(controlState());
  const click = (act, id) =>
    panel.listeners.click({
      target: {
        nodeType: 1,
        closest: () => ({
          getAttribute: (name) =>
            name === "data-act" ? act : name === "data-id" ? id : null,
        }),
      },
    });
  click("obligationDone", "ob-1");
  click("obligationSnooze", "ob-1");
  click("planDoneEvidence", "plan_2026-09-30_ship");
  assert.deepEqual(
    posted.map((msg) => [msg.type, msg.id]),
    [
      ["ready", undefined],
      ["obligationDone", "ob-1"],
      ["obligationSnooze", "ob-1"],
      ["planDoneEvidence", "plan_2026-09-30_ship"],
    ]
  );
});

test("sidebar badge mirrors the deduped attention count and clears at zero", () => {
  const provider = new SidebarProvider({ fsPath: "/ext" }, () => {});
  const posted = [];
  const view = {
    badge: undefined,
    webview: {
      options: undefined,
      html: "",
      onDidReceiveMessage: () => {},
      postMessage: (msg) => posted.push(msg),
    },
  };
  provider.resolveWebviewView(view);

  const three = controlState();
  provider.post(three);
  assert.deepEqual(view.badge, { value: 3, tooltip: "3 items need attention" });

  provider.post({ ...three, inbox: three.inbox.slice(0, 1) });
  assert.deepEqual(view.badge, { value: 1, tooltip: "1 item needs attention" });

  provider.post({ ...three, inbox: [] });
  assert.equal(view.badge, undefined, "badge clears at zero items");

  provider.post({ initialized: false });
  assert.equal(view.badge, undefined, "no project means no badge");
});
