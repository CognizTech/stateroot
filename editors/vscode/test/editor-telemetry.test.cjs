const assert = require("node:assert/strict");
const test = require("node:test");
const {
  EDITOR_ID_KEY,
  QUEUE_KEY,
  TELEMETRY_URL,
  LINK_ID_KEY,
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
  resolveInstallIdLink,
  linkRetryPending,
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
    assert.equal(await enqueue(state, buildEvent("e", "editor_seen", "0.2.21", "vscode")), false);
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

test("raw wire carries only the frozen allowlisted headers and an empty body", async () => {
  const FROZEN = [
    "x-sr-schema", "x-sr-event-id", "x-sr-editor-id", "x-sr-event",
    "x-sr-occurred-on", "x-sr-extension-version", "x-sr-editor-host",
    "x-sr-os-arch", "x-sr-profile-class", "x-sr-cli-status",
    "x-sr-path-class", "x-sr-result", "x-sr-stage", "x-sr-install-id",
  ];
  const state = memory();
  const editorId = await getOrCreateEditorId(state);
  const full = buildEvent(editorId, "setup_finished", "0.2.21", "vscode", {
    profile_class: "verified_legacy",
    cli_status: "working",
    path_class: "default_stable",
    result: "failed",
    stage: "install_failed",
    install_id: "11111111-1111-4111-8111-111111111111",
  });
  assert.deepEqual(Object.keys(eventHeaders(full)).sort(), FROZEN.slice().sort());
  await enqueue(state, full);
  const seen = [];
  await flushQueue(state, async (url, init) => {
    seen.push(init);
    return { ok: true };
  }, "http://127.0.0.1:9/editor");
  assert.equal(seen.length, 1);
  assert.equal(seen[0].method, "POST");
  assert.equal(seen[0].body, "");
  for (const name of Object.keys(seen[0].headers)) {
    assert.ok(FROZEN.includes(name), `unexpected header on the wire: ${name}`);
  }
});

test("queue bound refuses newcomers instead of dropping unacknowledged events", async () => {
  const state = memory();
  for (let i = 0; i < 50; i++) {
    assert.equal(await enqueue(state, buildEvent("e", "editor_seen", "0.2.21", "vscode")), true);
  }
  assert.equal(state.get(QUEUE_KEY).length, 50);
  const firstId = state.get(QUEUE_KEY)[0].event_id;
  assert.equal(
    await enqueue(state, buildEvent("e", "setup_finished", "0.2.21", "vscode")),
    false,
    "a full queue reports the no-op so callers can retry later"
  );
  const queue = state.get(QUEUE_KEY);
  assert.equal(queue.length, 50, "bound holds");
  assert.equal(queue[0].event_id, firstId, "oldest unacknowledged event is never evicted");
});

test("concurrent flushes share one serialized pass", async () => {
  const state = memory();
  await enqueue(state, buildEvent("e", "editor_seen", "0.2.21", "vscode"));
  let calls = 0;
  const fetchImpl = async () => {
    calls++;
    await new Promise((resolve) => setTimeout(resolve, 5));
    return { ok: true };
  };
  await Promise.all([
    flushQueue(state, fetchImpl, "http://127.0.0.1:9/editor"),
    flushQueue(state, fetchImpl, "http://127.0.0.1:9/editor"),
  ]);
  assert.equal(calls, 1, "one in-flight pass, no double send");
});

test("os_arch vocabulary matches the server allowlist on every host", () => {
  const { osTarget } = require("../out/installPing");
  assert.equal(osTarget("darwin", "x64"), "macos-x64");
  assert.equal(osTarget("darwin", "arm64"), "macos-aarch64");
  assert.equal(osTarget("linux", "arm64"), "linux-aarch64");
  assert.equal(osTarget("linux", "x64"), "linux-x64");
  assert.equal(osTarget("win32", "x64"), "windows-x64");
});

test("install-ID link persists, skips probing when resolved, and retries after failure", async () => {
  const id = "33333333-3333-4333-8333-333333333333";
  const state = memory();
  let probes = 0;
  const resolved = await resolveInstallIdLink(state, async () => { probes++; return id; });
  assert.equal(resolved, id);
  assert.equal(state.get(LINK_ID_KEY), id);
  // A resolved link never probes again.
  const again = await resolveInstallIdLink(state, async () => { probes++; return undefined; });
  assert.equal(again, id);
  assert.equal(probes, 1);

  // A failed probe leaves a pending marker; the next call retries and resolves.
  const fresh = memory();
  const failed = await resolveInstallIdLink(fresh, async () => { throw new Error("offline"); });
  assert.equal(failed, undefined);
  assert.equal(linkRetryPending(fresh), true);
  const retried = await resolveInstallIdLink(fresh, async () => id);
  assert.equal(retried, id);
  assert.equal(linkRetryPending(fresh), false);
});

test("anyCliBinaryExists distinguishes present-but-broken from missing", () => {
  const vm = require("node:vm");
  const fs = require("node:fs");
  const os = require("node:os");
  const path = require("node:path");
  const file = path.resolve(__dirname, "..", "out", "cliInstall.js");
  const api = {};
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sr-cli-"));
  const bin = path.join(dir, "stateroot");
  vm.runInNewContext(fs.readFileSync(file, "utf8"), {
    exports: api,
    __dirname: path.dirname(file),
    process,
    require(name) {
      if (name === "vscode") return {};
      if (name === "os") {
        const real = require("os");
        return { ...real, homedir: () => dir };
      }
      return require(name);
    },
  }, { filename: file });
  assert.equal(api.anyCliBinaryExists(bin), false, "nothing at any location is missing, not unrunnable");
  fs.writeFileSync(bin, "not a real binary");
  assert.equal(api.anyCliBinaryExists(bin), true, "present file exists, runnable or not");
  fs.rmSync(dir, { recursive: true, force: true });
});
