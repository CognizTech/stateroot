const assert = require('node:assert/strict');
const test = require('node:test');
const path = require('node:path');
const { ensureSetup, SETUP_KEY, editorHarness, classifyRecovery, allowInstallerFallback, classifySetupFailure } = require('../out/setup');

function fixture() {
  const store = {};
  const calls = [];
  const reports = [];
  let version = 'stateroot 0.1.15';
  const binary = path.join(process.cwd(), 'custom bin', 'stateroot');
  const options = {
    state: { get: key => store[key], update: async (key, value) => { store[key] = value; } },
    extensionVersion: '0.2.19', binary, noAutoUpdate: false,
    install: async () => { throw new Error('Existing CLI must not be replaced by default installer'); },
    run: async (args, selected) => {
      assert.equal(selected, binary);
      calls.push(args.join(' '));
      if (args[0] === '--version') return version;
      if (args[0] === 'self-update') {
        version = 'stateroot 0.2.2';
        return 'current: 0.1.15\nrelease: v0.2.2 (production)\nupdated';
      }
      if (args[0] === 'install') return 'Installed for: cursor, vscode-copilot\n';
      throw new Error('unexpected command');
    },
    report: state => reports.push(state),
  };
  return { options, store, calls, reports };
}

test('pre-marker installs refresh their selected binary and persist only successful setup', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  assert.ok(f.calls.includes('self-update'));
  assert.deepEqual(f.store[SETUP_KEY].configured, ['cursor', 'vscode-copilot']);
  assert.equal(f.reports.at(-1).version, 'stateroot 0.2.2');
  f.calls.length = 0;
  await ensureSetup(f.options);
  assert.deepEqual(f.calls, ['--version']);
});

test('failed update retries on the same extension version without telemetry involvement', async () => {
  const f = fixture();
  f.store['stateroot.lastSeenVersion'] = '0.2.19';
  const run = f.options.run;
  f.options.run = async (args, binary) => {
    if (args[0] === 'self-update') throw new Error('offline');
    return run(args, binary);
  };
  await assert.rejects(ensureSetup(f.options), /offline/);
  assert.equal(f.store[SETUP_KEY], undefined);
  f.options.run = run;
  await ensureSetup(f.options);
  assert.equal(f.store[SETUP_KEY].extensionVersion, '0.2.19');
});

test('failed agent configuration is not recorded as ready', async () => {
  const f = fixture();
  const run = f.options.run;
  f.options.run = async (args, binary) => {
    if (args[0] === 'install') throw new Error('hooks could not be written');
    return run(args, binary);
  };
  await assert.rejects(ensureSetup(f.options), /hooks could not/);
  assert.equal(f.store[SETUP_KEY], undefined);
  assert.notEqual(f.reports.at(-1).phase, 'ready');
});

test('legacy updater reporting success without fetching a release does not complete setup', async () => {
  const f = fixture();
  const run = f.options.run;
  f.options.run = async (args, binary) => args[0] === 'self-update'
    ? 'could not check for updates (no public release repo configured yet)' : run(args, binary);
  await assert.rejects(ensureSetup(f.options), /could not check/);
  assert.equal(f.store[SETUP_KEY], undefined);
});

test('a runnable but still-old selected CLI fails update verification', async () => {
  const f = fixture();
  const run = f.options.run;
  f.options.run = async (args, binary) => args[0] === 'self-update'
    ? 'release: v0.2.2 (production)' : run(args, binary);
  await assert.rejects(ensureSetup(f.options), /did not reach 0.2.2/);
  assert.equal(f.store[SETUP_KEY], undefined);
});

test('missing CLI installs automatically then connects agents without redownloading', async () => {
  const f = fixture();
  const binary = f.options.binary;
  f.options.binary = undefined;
  let installs = 0;
  f.options.install = async () => { installs++; return binary; };
  await ensureSetup(f.options);
  assert.equal(installs, 1);
  assert.ok(!f.calls.includes('self-update'));
  assert.ok(f.calls.includes('install'));
});

test('explicit auto-update opt-out still configures agents', async () => {
  const f = fixture();
  f.options.noAutoUpdate = true;
  await ensureSetup(f.options);
  assert.ok(!f.calls.includes('self-update'));
  assert.ok(f.calls.includes('install'));
});

test('retry setup repairs same-version integrations', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  await ensureSetup({ ...f.options, retry: true });
  assert.ok(f.calls.includes('install'));
  assert.ok(f.calls.includes('self-update'));
});

test('unrunnable selected CLI is recovered with the bundled installer', async () => {
  const f = fixture();
  const run = f.options.run;
  let versionCalls = 0;
  f.options.run = async (args, binary) => {
    if (args[0] === '--version' && versionCalls++ === 0) throw new Error('ENOENT');
    return run(args, binary);
  };
  f.options.install = async () => f.options.binary;
  await ensureSetup(f.options);
  assert.equal(f.store[SETUP_KEY].binary, f.options.binary);
});

test('failed updater falls back to the bundled installer only on the default stable path', async () => {
  const f = fixture();
  const run = f.options.run;
  f.options.pathClass = 'default_stable';
  let installed = 0;
  f.options.install = async () => { installed++; return f.options.binary; };
  f.options.run = async (args, binary) => {
    if (args[0] === 'self-update') throw new Error('offline');
    return run(args, binary);
  };
  await ensureSetup(f.options);
  assert.equal(installed, 1);
  assert.equal(f.store[SETUP_KEY].extensionVersion, '0.2.19');
});

test('custom and cargo paths never fall back to the bundled installer', async () => {
  const f = fixture();
  const run = f.options.run;
  f.options.pathClass = 'cargo';
  f.options.install = async () => { throw new Error('must not replace cargo CLI'); };
  f.options.run = async (args, binary) => args[0] === 'self-update'
    ? Promise.reject(new Error('offline')) : run(args, binary);
  await assert.rejects(ensureSetup(f.options), /offline/);
  assert.equal(f.store[SETUP_KEY], undefined);
});

test('host identities distinguish Cursor and VS Code including Insiders', () => {
  assert.equal(editorHarness('Cursor'), 'cursor');
  assert.equal(editorHarness('Visual Studio Code'), 'vscode-copilot');
  assert.equal(editorHarness('Visual Studio Code - Insiders'), 'vscode-copilot');
});

test('recovery classifier: missing CLI installs, stale CLI updates, current receipt is health-only', () => {
  assert.equal(classifyRecovery({ cliStatus: 'missing', receiptCurrent: false, retry: false, staleForUpdate: false }), 'install');
  assert.equal(classifyRecovery({ cliStatus: 'unrunnable', receiptCurrent: true, retry: false, staleForUpdate: false }), 'install');
  assert.equal(classifyRecovery({ cliStatus: 'working', receiptCurrent: false, retry: false, staleForUpdate: true }), 'self_update');
  assert.equal(classifyRecovery({ cliStatus: 'working', receiptCurrent: true, retry: false, staleForUpdate: false }), 'health');
  assert.equal(classifyRecovery({ cliStatus: 'working', receiptCurrent: true, retry: true, staleForUpdate: false }), 'self_update');
  assert.equal(allowInstallerFallback('default_stable'), true);
  assert.equal(allowInstallerFallback('cargo'), false);
  assert.equal(allowInstallerFallback('nightly'), false);
  assert.equal(allowInstallerFallback('custom'), false);
  assert.equal(classifySetupFailure('macOS on Intel is not shipped'), 'unsupported_platform');
  assert.equal(classifySetupFailure('CLI update did not reach 0.2.2'), 'update_failed');
  assert.equal(classifySetupFailure('hooks could not be written'), 'integration_failed');
});
