const assert = require("node:assert/strict");
const test = require("node:test");

// out/parallelWork.js pulls in ./cli, which requires "vscode" at load time
// (module-scope only, never dereferenced). Stub it like the other tests stub
// the CLI layer so the derivation runs CLI-free.
const Module = require("node:module");
const originalLoad = Module._load;
Module._load = function (request) {
  if (request === "vscode") return {};
  return originalLoad.apply(this, arguments);
};
const { deriveParallelWork } = require("../out/parallelWork");
Module._load = originalLoad;

function lineageWith(forks) {
  return {
    schema_version: "stateroot.lineage.v1",
    trunk: { ref: "refs/stateroot/trunk", tip: "a".repeat(40) },
    forks,
    roots: [],
  };
}

function fork(name, extra) {
  return Object.assign({ name, ref: "refs/stateroot/forks/" + name, contained: false }, extra);
}

function delegation(forkId, extra) {
  return Object.assign(
    { id: "d-" + forkId, harness: "kimi", task: "task for " + forkId, fork_id: forkId },
    extra
  );
}

function phases(cards) {
  return cards.map((card) => card.phase);
}

test("no ready lineages: empty and missing lineage derive no cards", () => {
  assert.deepEqual(deriveParallelWork(undefined, []), []);
  assert.deepEqual(deriveParallelWork(lineageWith([]), []), []);
});

test("one ready lineage: a captured outcome root is ready", () => {
  const cards = deriveParallelWork(lineageWith([fork("f1")]), [
    delegation("f1", { status: "succeeded", outcome_root: "b".repeat(40) }),
  ]);
  assert.equal(cards.length, 1);
  assert.equal(cards[0].phase, "ready");
  assert.equal(cards[0].outcomeRoot, "b".repeat(40));
});

test("seven ready lineages all derive ready", () => {
  const names = ["f1", "f2", "f3", "f4", "f5", "f6", "f7"];
  const cards = deriveParallelWork(
    lineageWith(names.map((name) => fork(name))),
    names.map((name) => delegation(name, { outcome_root: name.repeat(8) }))
  );
  assert.equal(cards.length, 7);
  assert.deepEqual(phases(cards), names.map(() => "ready"));
});

test("contained fork is merged regardless of attempts", () => {
  const cards = deriveParallelWork(lineageWith([fork("f1", { contained: true })]), []);
  assert.deepEqual(phases(cards), ["merged"]);
});

test("failed or timed_out without an outcome root needs attention", () => {
  const cards = deriveParallelWork(lineageWith([fork("f1"), fork("f2"), fork("f3")]), [
    delegation("f1", { status: "failed" }),
    delegation("f2", { status: "timed_out" }),
    delegation("f3", { status: "lost" }),
  ]);
  assert.deepEqual(phases(cards), ["attention", "attention", "attention"]);
});

test("a failed attempt with a captured outcome root is still ready", () => {
  const cards = deriveParallelWork(lineageWith([fork("f1")]), [
    delegation("f1", { status: "failed", outcome_root: "c".repeat(40) }),
  ]);
  assert.deepEqual(phases(cards), ["ready"]);
});

test("cleanup pending derives cleanup_pending with the exact retry command", () => {
  const cards = deriveParallelWork(
    lineageWith([fork("f-clean", { cleanup: { state: "pending", pending: "worktree still registered" } })]),
    []
  );
  assert.equal(cards[0].phase, "cleanup_pending");
  assert.equal(cards[0].cleanupCommand, "stateroot merge --cleanup f-clean");
});

test("finished cleanup derives no retry command", () => {
  const cards = deriveParallelWork(
    lineageWith([fork("f1", { contained: true, cleanup: { state: "done" } })]),
    []
  );
  assert.equal(cards[0].phase, "merged");
  assert.equal(cards[0].cleanupCommand, undefined);
});

test("a cancelling delegation marks the card cancelling", () => {
  const cards = deriveParallelWork(lineageWith([fork("f1")]), [
    delegation("f1", { status: "cancelling" }),
  ]);
  assert.deepEqual(phases(cards), ["cancelling"]);
});

test("a running delegation marks the card running", () => {
  const cards = deriveParallelWork(lineageWith([fork("f1")]), [
    delegation("f1", { status: "running" }),
  ]);
  assert.deepEqual(phases(cards), ["running"]);
});


test("lineage reader supports paged and legacy CLI responses without masking operational failures", async () => {
  const cli = require("../out/cli");
  const { readParallelWork } = require("../out/parallelWork");
  const previousRun = cli.runCli, previousPath = cli.cliPath;
  let binary = "legacy-page-test";
  let calls = [];
  const output = { appendLine() {} };
  const full = lineageWith([]);
  try {
    cli.cliPath = () => binary;
    cli.runCli = async args => {
      calls.push(args);
      if (args.includes("--limit")) throw new Error("error: unexpected argument '--limit' found");
      return JSON.stringify(full);
    };
    assert.deepEqual(await readParallelWork("/project", output), full);
    assert.equal(calls.length, 2);
    await readParallelWork("/project", output, 50);
    assert.deepEqual(calls.at(-1), ["log", "--json"]);
    assert.equal(calls.length, 3, "old CLI capability is cached");
    binary = "paged-page-test";
    const paged = { ...full, topology: [], pagination: { schema_version: "stateroot.lineage.pagination.v1", offset: 0, limit: 50, total: 0, has_more: false, history: "empty" } };
    cli.runCli = async args => { calls.push(args); return JSON.stringify(paged); };
    assert.deepEqual(await readParallelWork("/project", output), paged);
    assert.ok(calls.at(-1).includes("--limit"));
    paged.pagination.total = 12;
    assert.equal(await readParallelWork("/project", output), undefined, "invalid pages stay errors");
    binary = "broken-page-test";
    calls = [];
    cli.runCli = async args => { calls.push(args); throw new Error("permission denied"); };
    assert.equal(await readParallelWork("/project", output), undefined);
    assert.equal(calls.length, 1, "real failures never trigger a legacy retry");
  } finally { cli.runCli = previousRun; cli.cliPath = previousPath; }
});
