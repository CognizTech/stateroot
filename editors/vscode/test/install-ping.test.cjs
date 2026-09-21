const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const test = require("node:test");

const extension = path.resolve(__dirname, "..");

function loadInstallPing({ platform = "linux", arch = "x64", env = {}, fetchImpl } = {}) {
  const api = {};
  const calls = [];
  const file = path.join(extension, "out", "installPing.js");
  const fetch =
    fetchImpl ||
    ((url) => {
      calls.push(url);
      return Promise.resolve(new Response());
    });
  vm.runInNewContext(fs.readFileSync(file, "utf8"), {
    exports: api,
    __dirname: path.dirname(file),
    process: { platform, arch, env },
    fetch,
    AbortController,
    Response,
    setTimeout,
    clearTimeout,
    require(name) {
      if (name === "vscode") return {};
      return require(name);
    },
  }, { filename: file });
  return { api, calls };
}

function fakeContext(version, store = {}) {
  return {
    extension: { packageJSON: { version } },
    globalState: {
      get: (key) => store[key],
      update: (key, value) => {
        store[key] = value;
        return Promise.resolve();
      },
    },
  };
}

test("osTarget matches the CLI ping vocabulary", () => {
  const { api } = loadInstallPing();
  assert.equal(api.osTarget("win32", "x64"), "windows-x64");
  assert.equal(api.osTarget("darwin", "arm64"), "macos-aarch64");
  assert.equal(api.osTarget("darwin", "x64"), "macos-x64");
  assert.equal(api.osTarget("linux", "x64"), "linux-x64");
  assert.equal(api.osTarget("linux", "arm64"), "linux-aarch64");
});

test("pingKind: absent marker is install, differing marker is update, same is silent", () => {
  const { api } = loadInstallPing();
  assert.equal(api.pingKind(undefined, "0.2.18"), "install");
  assert.equal(api.pingKind("0.2.17", "0.2.18"), "update");
  assert.equal(api.pingKind("0.2.18", "0.2.18"), undefined);
});

test("marker is written without a legacy GET ping", () => {
  const store = {};
  const { api, calls } = loadInstallPing();
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(store["stateroot.lastSeenVersion"], "0.2.18");
  assert.equal(calls.length, 0);
});

test("same version stays silent", () => {
  const store = { "stateroot.lastSeenVersion": "0.2.18" };
  const { api, calls } = loadInstallPing();
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(calls.length, 0);
  assert.equal(store["stateroot.lastSeenVersion"], "0.2.18");
});

test("STATEROOT_NO_PING no longer owns the version marker", () => {
  const store = {};
  const { api, calls } = loadInstallPing({ env: { STATEROOT_NO_PING: "1" } });
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(calls.length, 0);
  assert.equal(store["stateroot.lastSeenVersion"], "0.2.18");
});

test("shouldRefreshCli: only a real extension update under a working CLI refreshes", () => {
  const { api } = loadInstallPing();
  // Extension updated, CLI present → refresh.
  assert.equal(api.shouldRefreshCli("0.2.18", "0.2.19", true, false), true);
  // No successful setup receipt: recover pre-marker installs as well.
  assert.equal(api.shouldRefreshCli(undefined, "0.2.19", true, false), true);
  // Same version → nothing to do.
  assert.equal(api.shouldRefreshCli("0.2.19", "0.2.19", true, false), false);
  // CLI missing → the missing-CLI flow owns that path, not refresh.
  assert.equal(api.shouldRefreshCli("0.2.18", "0.2.19", false, false), false);
  // Auto-update disabled → honor the CLI's own opt-out.
  assert.equal(api.shouldRefreshCli("0.2.18", "0.2.19", true, true), false);
});

test("previousVersion reads the marker without writing it", () => {
  const { api } = loadInstallPing();
  const store = { "stateroot.lastSeenVersion": "0.2.17" };
  assert.equal(api.previousVersion(fakeContext("0.2.18", store)), "0.2.17");
  assert.equal(store["stateroot.lastSeenVersion"], "0.2.17");
  assert.equal(api.previousVersion(fakeContext("0.2.18", {})), undefined);
});
