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

test("marker is written before the ping fires", () => {
  const store = {};
  let seenMarkerAtFire;
  const { api } = loadInstallPing({
    fetchImpl: () => {
      seenMarkerAtFire = store["stateroot.lastSeenVersion"];
      return Promise.resolve(new Response());
    },
  });
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(store["stateroot.lastSeenVersion"], "0.2.18");
  assert.equal(seenMarkerAtFire, "0.2.18", "marker must be written before the ping fires");
});

test("recording loader: install ping carries via=extension, kind, os, version", () => {
  const store = {};
  const { api, calls } = loadInstallPing();
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(calls.length, 1);
  const url = new URL(calls[0]);
  assert.equal(url.searchParams.get("via"), "extension");
  assert.equal(url.searchParams.get("kind"), "install");
  assert.equal(url.searchParams.get("v"), "0.2.18");
  assert.equal(url.searchParams.get("os"), "linux-x64");
  assert.equal(url.searchParams.get("from"), null);
});

test("version change pings kind=update with from=<old>", () => {
  const store = { "stateroot.lastSeenVersion": "0.2.17" };
  const { api, calls } = loadInstallPing();
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(calls.length, 1);
  const url = new URL(calls[0]);
  assert.equal(url.searchParams.get("kind"), "update");
  assert.equal(url.searchParams.get("from"), "0.2.17");
  assert.equal(store["stateroot.lastSeenVersion"], "0.2.18");
});

test("same version stays silent", () => {
  const store = { "stateroot.lastSeenVersion": "0.2.18" };
  const { api, calls } = loadInstallPing();
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(calls.length, 0);
});

test("STATEROOT_NO_PING opts out and leaves the marker untouched", () => {
  const store = {};
  const { api, calls } = loadInstallPing({ env: { STATEROOT_NO_PING: "1" } });
  api.maybePing(fakeContext("0.2.18", store));
  assert.equal(calls.length, 0);
  assert.equal(store["stateroot.lastSeenVersion"], undefined);
});
