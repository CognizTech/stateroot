const assert = require('node:assert/strict');
const test = require('node:test');
const Module = require('node:module');
const cp = require('node:child_process');
const originalLoad = Module._load;
const notices = [];
Module._load = function(request, parent) {
  if (request === 'vscode') return {
    workspace: { getConfiguration: () => ({ get: () => '' }) },
    window: { showErrorMessage: async message => { notices.push(message); } },
  };
  if (request === './cliInstall' && parent?.filename.endsWith('cli.js')) return {
    findWorkingCli: async () => 'fixture-stateroot',
  };
  return originalLoad.apply(this, arguments);
};
const { runCliReport, cliFailureMessage, readDelegationTargets } = require('../out/cli');
Module._load = originalLoad;

test('timeout identifies the limit and preserves command output', () => {
  const error = Object.assign(new Error('Command failed'), { killed: true, signal: 'SIGTERM' });
  const message = cliFailureMessage(error, 'partial delegation result', '', 20000);
  assert.match(message, /timed out after 20s \(SIGTERM\)/);
  assert.match(message, /partial delegation result/);
  assert.match(cliFailureMessage(Object.assign(new Error('failed'), { code: 1 }), '', 'permission denied', 20000), /code 1.*\npermission denied/);
});

test('background failures log once without popups; manual failures still notify', async () => {
  const previous = cp.execFile;
  const lines = [];
  const output = { appendLine: line => lines.push(line) };
  let fail = true;
  cp.execFile = (_binary, _args, _options, callback) => {
    callback(fail ? Object.assign(new Error('failed'), { code: 1 }) : null, fail ? '' : 'no delegations recorded\n', fail ? 'fixture error' : '');
  };
  try {
    const args = ['delegate', 'list'];
    assert.equal(await runCliReport(args, '/fixture', output, 20000, { allowInstall: false, notifyOnError: false }), undefined);
    const logged = lines.length;
    await runCliReport(args, '/fixture', output, 20000, { allowInstall: false, notifyOnError: false });
    assert.equal(lines.length, logged);
    assert.equal(notices.length, 0);
    assert.ok(lines.some(line => line.includes('fixture-stateroot')));
    fail = false;
    assert.equal(await runCliReport(args, '/fixture', output), 'no delegations recorded\n');
    fail = true;
    await runCliReport(args, '/fixture', output, 20000, { allowInstall: false, notifyOnError: false });
    assert.ok(lines.length > logged, 'a new failure after recovery is logged');
    await runCliReport(args, '/fixture', output);
    assert.equal(notices.length, 1);
    assert.match(notices[0], /fixture error/);
  } finally { cp.execFile = previous; }
});

test('delegation targets use available CLI rows and fall back only for an older CLI', async () => {
  const previous = cp.execFile;
  let error;
  let stdout = JSON.stringify({schema_version: 'stateroot.delegation-targets.v1', targets: [
    {id: 'commandcode', display: 'Command Code', available: true},
    {id: 'claude', display: 'Claude Code', available: false},
  ]});
  cp.execFile = (_binary, args, options, callback) => {
    assert.deepEqual(args, ['delegate', 'targets', '--json']);
    assert.equal(options.cwd, '/fixture');
    callback(error && Object.assign(new Error('CLI failed'), {code: 2}), stdout, error || '');
  };
  try {
    assert.deepEqual(await readDelegationTargets('/fixture'), {
      available: [{id: 'commandcode', display: 'Command Code'}], legacy: false,
    });
    stdout = JSON.stringify({schema_version: 'stateroot.delegation-targets.v1', targets: []});
    assert.deepEqual(await readDelegationTargets('/fixture'), { available: [], legacy: false });
    error = "error: unrecognized subcommand 'targets'";
    const legacy = await readDelegationTargets('/fixture');
    assert.equal(legacy.legacy, true);
    assert.deepEqual(legacy.available.map(row => row.id), ['claude', 'codex', 'kimi']);
    error = 'No permissions';
    await assert.rejects(readDelegationTargets('/fixture'), /No permissions/);
  } finally { cp.execFile = previous; }
});
