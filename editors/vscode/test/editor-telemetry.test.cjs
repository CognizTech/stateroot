const assert = require("node:assert/strict");
const test = require("node:test");
const {
  EDITOR_ID_KEY,
  QUEUE_KEY,
  TELEMETRY_URL,
  classifyPath,
  classifyProfile,
  eventHeaders,
  enqueue,
  ack,
  flushQueue,
  getOrCreateEditorId,
  capturePreflight,
  buildEvent,
  parseInstallId,
  editorHost,
} = require("../out/editorTelemetry");

function memory() {
  const store = {};
  return {
    store,
    get: (key, fallback) => (key in store ? store[key] : fallback),
    update: async (key, value) => {
      store[key] = value;
    },
  };
}

test("path class never transmits the path and distinguishes default/custom/cargo/nightly", () => {
  assert.equal(classifyPath("/home/u/.local/bin/stateroot", "/home/u/.local/bin/stateroot"), "default_stable");
  assert.equal(classifyPath("/opt/bin/stateroot", "/home/u/.local/bin/stateroot"), "custom");
  assert.equal(classifyPath("/home/u/.cargo/bin/stateroot"), "cargo");
  assert.equal(classifyPath("C:\\Users\\u\\.cargo\\bin\\stateroot.exe"), "cargo");
  assert.equal(classifyPath("/tmp/stateroot-nightly"), "nightly");
  assert.equal(classifyPath(undefined), "unknown");
});

test("profile class never guesses evidence-free profiles as fresh or legacy", () => {
  assert.equal(classifyProfile({ previousVersion: "0.2.19", workspaceInitialized: false, cliStatus: "missing" }), "verified_legacy");
  assert.equal(classifyProfile({ previousReceipt: { extensionVersion: "0.2.19" }, workspaceInitialized: false, cliStatus: "missing" }), "verified_legacy");
  assert.equal(classifyProfile({ workspaceInitialized: true, cliStatus: "missing" }), "verified_project");
  assert.equal(classifyProfile({ workspaceInitialized: false, cliStatus: "working" }), "existing_cli");
  assert.equal(classifyProfile({ workspaceInitialized: false, cliStatus: "missing" }), "unknown_first_seen");
});

test("editor host maps Cursor vs VS Code", () => {
  assert.equal(editorHost("Cursor"), "cursor");
  assert.equal(editorHost("Visual Studio Code"), "vscode");
});

test("event headers are allowlisted enums, versions, dates, and ids", () => {
  const event = buildEvent("editor-1", "setup_finished", "0.2.21", "cursor", {
    profile_class: "verified_legacy",
    cli_status: "working",
    path_class: "default_stable",
    result: "ready",
    install_id: "11111111-1111-4111-8111-111111111111",
  });
  const headers = eventHeaders(event);
  for (const name of Object.keys(headers)) {
    assert.match(name, /^x-sr-/);
  }
  assert.equal(headers["x-sr-schema"], "2");
  assert.equal(headers["x-sr-event"], "setup_finished");
  assert.equal(headers["x-sr-result"], "ready");
  assert.ok(!JSON.stringify(headers).includes("/home"));
  assert.equal(parseInstallId('{"install_id":"abc","secret":"nope"}'), "abc");
  assert.equal(parseInstallId('{"paused":true}'), undefined);
});

test("editor events survive offline activation and deduplicate after retry", async () => {
  const state = memory();
  const editorId = await getOrCreateEditorId(state);
  const event = buildEvent(editorId, "setup_started", "0.2.21", "vscode", {
    profile_class: "unknown_first_seen",
    cli_status: "missing",
  });
  await enqueue(state, event);
  await enqueue(state, event);
  assert.equal(state.get(QUEUE_KEY).length, 1);
  const attempts = [];
  await flushQueue(state, async (url, init) => {
    attempts.push({ url, headers: init.headers });
    throw new Error("offline");
  }, "http://127.0.0.1:9/editor");
  assert.equal(state.get(QUEUE_KEY).length, 1, "failed POST stays queued");
  await flushQueue(state, async () => ({ ok: true }), "http://127.0.0.1:9/editor");
  assert.equal(state.get(QUEUE_KEY).length, 0);
  assert.equal(attempts[0].url, "http://127.0.0.1:9/editor");
  assert.equal(attempts[0].headers["x-sr-event"], "setup_started");
  assert.equal(attempts[0].headers["content-type"], undefined);
});

test("STATEROOT_NO_PING skips enqueue and flush", async () => {
  const prev = process.env.STATEROOT_NO_PING;
  process.env.STATEROOT_NO_PING = "1";
  try {
    const state = memory();
    await enqueue(state, buildEvent("e", "editor_seen", "0.2.21", "vscode"));
    assert.equal(state.get(QUEUE_KEY, []).length, 0);
    let called = 0;
    await flushQueue(state, async () => { called++; return { ok: true }; });
    assert.equal(called, 0);
  } finally {
    if (prev === undefined) delete process.env.STATEROOT_NO_PING;
    else process.env.STATEROOT_NO_PING = prev;
  }
});

test("two editor profiles can link to one CLI install_id", async () => {
  const a = memory();
  const b = memory();
  const idA = await getOrCreateEditorId(a);
  const idB = await getOrCreateEditorId(b);
  assert.notEqual(idA, idB);
  const install = "22222222-2222-4222-8222-222222222222";
  await enqueue(a, buildEvent(idA, "setup_finished", "0.2.21", "vscode", {
    result: "ready",
    install_id: install,
  }));
  await enqueue(b, buildEvent(idB, "setup_finished", "0.2.21", "cursor", {
    result: "ready",
    install_id: install,
  }));
  assert.equal(a.get(QUEUE_KEY)[0].install_id, install);
  assert.equal(b.get(QUEUE_KEY)[0].install_id, install);
  assert.notEqual(a.get(EDITOR_ID_KEY), b.get(EDITOR_ID_KEY));
});

test("preflight snapshot is captured without mutating markers", () => {
  const snapshot = capturePreflight({
    previousVersion: "0.2.19",
    previousReceipt: { extensionVersion: "0.2.19", binary: "/opt/stateroot", version: "0.2.3" },
    host: "cursor",
    workspaceInitialized: true,
    cliStatus: "missing",
    binary: "/opt/stateroot",
    defaultDest: "/home/u/.local/bin/stateroot",
  });
  assert.equal(snapshot.profileClass, "verified_legacy");
  assert.equal(snapshot.pathClass, "custom");
  assert.equal(snapshot.cliStatus, "missing");
  assert.equal(TELEMETRY_URL, "https://stateroot.dev/api/telemetry/v2/editor");
});

test("ack removes only the finished event", async () => {
  const state = memory();
  const first = buildEvent("e", "editor_seen", "0.2.21", "vscode");
  const second = buildEvent("e", "setup_started", "0.2.21", "vscode");
  await enqueue(state, first);
  await enqueue(state, second);
  await ack(state, first.event_id);
  assert.deepEqual(state.get(QUEUE_KEY).map((row) => row.event), ["setup_started"]);
});
