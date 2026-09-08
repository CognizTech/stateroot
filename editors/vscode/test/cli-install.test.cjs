const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const vm = require("node:vm");
const { PassThrough } = require("node:stream");
const { execFileSync } = require("node:child_process");
const { createHash } = require("node:crypto");
const { pathToFileURL } = require("node:url");
const test = require("node:test");

const extension = path.resolve(__dirname, "..");
const repository = path.resolve(extension, "..", "..");

function childResult(callback, error, stdout = "", stderr = "") {
  const child = { stdout: new PassThrough(), stderr: new PassThrough() };
  queueMicrotask(() => {
    child.stdout.end(stdout);
    child.stderr.end(stderr);
    callback(error, stdout, stderr);
  });
  return child;
}

function loadInstaller({ platform = "darwin", arch = "arm64", execFile, fileSystem = fs } = {}) {
  const api = {};
  const messages = [];
  const output = {
    append: (text) => messages.push(text),
    appendLine: (text) => messages.push(text + "\n"),
    show() {},
  };
  const calls = [];
  const run = execFile || ((command, args, options, callback) => childResult(callback, null));
  const file = path.join(extension, "out", "cliInstall.js");
  vm.runInNewContext(fs.readFileSync(file, "utf8"), {
    exports: api,
    __dirname: path.dirname(file),
    Buffer,
    process: { platform, arch, env: {} },
    require(name) {
      if (name === "fs") return fileSystem;
      if (name === "os") return { homedir: () => "/test user" };
      if (name === "child_process") return {
        execFile(...args) {
          calls.push({ command: args[0], args: args[1], options: args[2] });
          return run(...args);
        },
      };
      if (name === "vscode") return {
        ProgressLocation: { Notification: 15 },
        window: {
          withProgress: (_options, task) => task(),
          showErrorMessage: async () => undefined,
        },
      };
      return require(name);
    },
  }, { filename: file });
  return { api, output, messages, calls };
}

test("Apple Silicon first install executes the bundled script and verifies the CLI", async () => {
  const { api, output, calls } = loadInstaller();
  const result = await api.autoInstallCli(output);
  assert.equal(result, "/test user/.local/bin/stateroot");
  assert.equal(calls[0].command, "sh");
  assert.equal(calls[0].args[0], path.join(extension, "assets", "install.sh"));
  assert.equal(calls[0].options.env.STATEROOT_INSTALL_VIA, "extension");
  assert.equal(calls[1].command, result);
  assert.equal(calls[1].args[0], "--version");
});

test("activation and Install CLI share a single in-flight installation", async () => {
  const { api, output, calls } = loadInstaller();
  const results = await Promise.all([api.installCli(output), api.installCli(output)]);
  assert.ok(results.every((result) => result.ok));
  assert.equal(calls.filter((call) => call.command === "sh").length, 1);
});

test("timeout errors retain the cause and stream partial installer output", async () => {
  const { api, output, messages } = loadInstaller({
    execFile(_command, _args, _options, callback) {
      return childResult(callback, Object.assign(new Error("SIGTERM"), { killed: true }),
        "fetching stateroot-macos-aarch64\n");
    },
  });
  const result = await api.installCli(output);
  assert.equal(result.ok, false);
  assert.match(result.error, /timed out/);
  assert.match(result.error, /fetching stateroot-macos-aarch64/);
  assert.ok(messages.some((line) => line === "fetching stateroot-macos-aarch64\n"));
});

test("an installer success does not hide a missing or unrunnable binary", async () => {
  const { api, output } = loadInstaller({
    execFile(command, _args, _options, callback) {
      return childResult(callback, command === "sh" ? null : new Error("ENOENT"));
    },
  });
  const result = await api.installCli(output);
  assert.equal(result.ok, false);
  assert.match(result.error, /not runnable/);
});

test("a failed installation can be retried", async () => {
  let attempts = 0;
  const { api, output } = loadInstaller({
    execFile(command, _args, _options, callback) {
      if (command === "sh" && ++attempts === 1) return childResult(callback, new Error("download failed"));
      return childResult(callback, null);
    },
  });
  assert.equal((await api.installCli(output)).ok, false);
  assert.equal((await api.installCli(output)).ok, true);
});

test("Cargo-installed CLI is found when GUI PATH omits ~/.cargo/bin", async () => {
  const expected = "/test user/.cargo/bin/stateroot";
  const { api } = loadInstaller({
    execFile(command, _args, _options, callback) {
      return childResult(callback, command === expected ? null : new Error("ENOENT"));
    },
  });
  assert.equal(await api.findWorkingCli("stateroot"), expected);
});

test("unsupported Intel Mac cannot accidentally receive an ARM binary", async () => {
  const { api, output, calls } = loadInstaller({ arch: "x64" });
  assert.equal(await api.autoInstallCli(output), undefined);
  assert.equal(calls.length, 0);
});

test("Linux and Windows retain their respective installer commands", async () => {
  for (const [platform, command, filename] of [
    ["linux", "sh", "install.sh"], ["win32", "powershell.exe", "install.ps1"],
  ]) {
    const { api, output, calls } = loadInstaller({ platform, arch: "x64" });
    assert.equal((await api.installCli(output)).ok, true);
    assert.equal(calls[0].command, command);
    assert.equal(calls[0].args.at(-1), path.join(extension, "assets", filename));
  }
});

test("missing packaged installer fails explicitly instead of using an older release script", async () => {
  const { api, output, calls } = loadInstaller({ fileSystem: { ...fs, existsSync: () => false } });
  const result = await api.installCli(output);
  assert.equal(result.ok, false);
  assert.match(result.error, /Bundled install.sh is missing/);
  assert.equal(calls.length, 0);
});

test("packaged installers match the checked-out source revision", () => {
  for (const filename of ["install.sh", "install.ps1"]) {
    assert.deepEqual(fs.readFileSync(path.join(extension, "assets", filename)),
      fs.readFileSync(path.join(repository, filename)));
  }
});

test("terminal PATH includes an absolute CLI location and does not mutate on every refresh", () => {
  const api = {};
  const file = path.join(extension, "out", "terminalPath.js");
  vm.runInNewContext(fs.readFileSync(file, "utf8"), { exports: api, require }, { filename: file });
  const mutations = [];
  const collection = { prepend: (name, value) => mutations.push([name, value]) };
  const update = api.terminalPathUpdater(collection);
  update("stateroot");
  assert.equal(mutations.length, 0);
  update("/test user/.local/bin/stateroot");
  update("/test user/.local/bin/stateroot");
  assert.deepEqual(mutations, [["PATH", "/test user/.local/bin" + path.delimiter]]);
  update("/test user/.cargo/bin/stateroot");
  assert.equal(mutations.length, 2);
});

test("POSIX download verification rejects tampering before installation", {
  skip: !((process.platform === "darwin" && process.arch === "arm64") ||
    (process.platform === "linux" && process.arch === "x64")),
}, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-checksum-test-"));
  try {
    const asset = process.platform === "darwin" ? "stateroot-macos-aarch64" : "stateroot-linux-x64";
    const data = Buffer.from("test release payload");
    const digest = createHash("sha256").update(data).digest("hex");
    fs.writeFileSync(path.join(dir, asset), data);
    fs.writeFileSync(path.join(dir, "checksums.txt"), `${digest}  ${asset}\n`);
    // Run the actual platform/download/checksum stages, stopping before writes
    // to the install location or any global harness configuration.
    const verifyStage = fs.readFileSync(path.join(repository, "install.sh"), "utf8")
      .split("# --- install ")[0];
    const env = { ...process.env, STATEROOT_INSTALL_BASE: pathToFileURL(dir).href };
    assert.match(execFileSync("sh", ["-c", verifyStage], { env, encoding: "utf8" }), /checksum verified/);
    fs.appendFileSync(path.join(dir, asset), "tampered");
    assert.throws(() => execFileSync("sh", ["-c", verifyStage], { env, stdio: "pipe" }),
      (err) => /checksum mismatch/.test(err.stderr.toString()));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});

test("fresh zsh profile is created in ZDOTDIR without deleting existing settings or duplicating PATH", {
  skip: process.platform === "win32",
}, () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "stateroot-profile-test-"));
  try {
    const script = fs.readFileSync(path.join(repository, "install.sh"), "utf8");
    const pathStage = script.split("# --- PATH ")[1].split("# --- anonymous install ping")[0];
    const body = 'set -eu\nlog() { :; }\nDEST_DIR="$1"\n# --- PATH ' + pathStage;
    const env = { ...process.env, SHELL: "/bin/zsh", ZDOTDIR: dir };
    const args = ["-c", body, "test-path", path.join(dir, "bin")];
    execFileSync("sh", args, { env });
    const profile = path.join(dir, ".zshrc");
    assert.match(fs.readFileSync(profile, "utf8"), /# stateroot PATH/);
    fs.appendFileSync(profile, "\n# existing user setting\n");
    execFileSync("sh", args, { env });
    const result = fs.readFileSync(profile, "utf8");
    assert.equal(result.split("# stateroot PATH").length - 1, 1);
    assert.match(result, /existing user setting/);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
