const assert = require("node:assert/strict");
const test = require("node:test");

// out/mergeAttempt.js is a pure contract module: no vscode, no CLI.
const {
  MERGE_ATTEMPT_SCHEMA,
  UNSUPPORTED_ATTEMPT_ERROR,
  isStaleIntegrationCli,
  parseMergeAttempt,
} = require("../out/mergeAttempt");

function readyAttempt() {
  return {
    schema_version: MERGE_ATTEMPT_SCHEMA,
    id: "ma-ready-1",
    created_at: "2026-09-17T10:00:00Z",
    harness: "kimi",
    trunk_tip: "a".repeat(40),
    forks: [
      { name: "f1", tip: "b".repeat(40) },
      { name: "f2", tip: "c".repeat(40) },
    ],
    state: "ready",
    conflicts: [],
    folded_forks: [{ name: "f0", tip: "d".repeat(40) }],
  };
}

function attentionAttempt() {
  return {
    schema_version: MERGE_ATTEMPT_SCHEMA,
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
      { fork: "f1", path: "src/merge.ts", kind: "both_modified", ancestor: "r1", ours: "r2", theirs: "r3" },
      { fork: "f2", path: "docs/guide.md", kind: "deleted_by_trunk" },
      { fork: "f2", path: "src/new.ts", kind: "added_both" },
      { fork: "f1", path: "old.txt" },
    ],
    pending_forks: [{ name: "f3", tip: "e".repeat(40) }],
    worktree: "/tmp/stateroot-ma-attn-1",
  };
}

test("v1 prepare payloads parse, ready and attention", () => {
  const ready = parseMergeAttempt(JSON.stringify(readyAttempt()));
  assert.equal(ready.id, "ma-ready-1");
  assert.equal(ready.state, "ready");
  assert.deepEqual(ready.conflicts, []);
  const attention = parseMergeAttempt(JSON.stringify(attentionAttempt()));
  assert.equal(attention.state, "attention");
  assert.equal(attention.worktree, "/tmp/stateroot-ma-attn-1");
  assert.equal(attention.conflicts.length, 4);
  assert.equal(attention.conflicts[1].kind, "deleted_by_trunk");
});

test("anything outside stateroot.merge-attempt.v1 throws the unsupported sentinel", () => {
  const wrongSchema = { ...readyAttempt(), schema_version: "stateroot.merge-attempt.v0" };
  assert.throws(() => parseMergeAttempt(JSON.stringify(wrongSchema)), new RegExp(UNSUPPORTED_ATTEMPT_ERROR));
  const missingId = readyAttempt();
  delete missingId.id;
  assert.throws(() => parseMergeAttempt(JSON.stringify(missingId)), new RegExp(UNSUPPORTED_ATTEMPT_ERROR));
  const bogusState = { ...readyAttempt(), state: "merged" };
  assert.throws(() => parseMergeAttempt(JSON.stringify(bogusState)), new RegExp(UNSUPPORTED_ATTEMPT_ERROR));
  const conflictsNotArray = { ...readyAttempt(), conflicts: "none" };
  assert.throws(() => parseMergeAttempt(JSON.stringify(conflictsNotArray)), new RegExp(UNSUPPORTED_ATTEMPT_ERROR));
});

test("a pre-contract CLI printing human output hits the same sentinel", () => {
  assert.throws(() => parseMergeAttempt("Merged forks: f1, f2"), new RegExp(UNSUPPORTED_ATTEMPT_ERROR));
  assert.throws(() => parseMergeAttempt(""), new RegExp(UNSUPPORTED_ATTEMPT_ERROR));
});

test("schema and parse failures classify as a stale integration CLI", () => {
  try {
    parseMergeAttempt(JSON.stringify({ schema_version: "stateroot.merge-attempt.v0" }));
    assert.fail("guard must throw");
  } catch (err) {
    assert.equal(isStaleIntegrationCli(err), true);
  }
  try {
    parseMergeAttempt("Merged forks: f1, f2");
    assert.fail("guard must throw");
  } catch (err) {
    assert.equal(isStaleIntegrationCli(err), true);
  }
});

test("unknown-flag and unknown-subcommand stderr classify as a stale integration CLI", () => {
  const signatures = [
    "error: unexpected argument '--prepare' found",
    "error: unrecognized subcommand 'merge'",
    "error: Found argument '--prepare' which wasn't expected, but is similar to '--json'",
    "error: unrecognized option 'evidence'",
    "error: unknown option `--prepare`",
  ];
  for (const stderr of signatures) {
    assert.equal(isStaleIntegrationCli(new Error(stderr)), true, stderr);
  }
});

test("ordinary command failures are not a stale CLI", () => {
  const ordinary = [
    "ENOENT",
    "fork f9 not found",
    "not a stateroot project (run stateroot init)",
    "merge attempt ma-1 is not ready to continue",
  ];
  for (const message of ordinary) {
    assert.equal(isStaleIntegrationCli(new Error(message)), false, message);
  }
});
