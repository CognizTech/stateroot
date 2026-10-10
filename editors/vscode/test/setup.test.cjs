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
      if (args[0] === 'doctor' && args[1] === '--json') {
        return JSON.stringify({
          schema_version: 'stateroot.doctor.v1',
          ok: true,
          integrations: {
            harnesses: [
              { harness: 'cursor', status: 'configured' },
              { harness: 'vscode-copilot', status: 'observed_working' },
            ],
          },
        });
      }
      if (args[0] === 'doctor') return '';
      if (args[0] === 'self-update') {
        version = 'stateroot 0.2.2';
        return 'current: 0.1.15\nrelease: v0.2.2 (production)\nupdated';
      }
      if (args[0] === 'install' && args[1] === '--json') {
        return JSON.stringify({
          schema_version: 'stateroot.integration-health.v1',
          generated_at: '2026-10-09T00:00:00Z',
          harnesses: [
            { harness: 'cursor', status: 'configured', detected: true, problems: [] },
            { harness: 'vscode-copilot', status: 'configured', detected: true, problems: [] },
          ],
          install: { configured: ['cursor', 'vscode-copilot'], failed: [], cli_only: false },
        });
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
  assert.deepEqual(f.calls, ['--version', 'doctor --json'], 'current receipt earns a typed health probe, nothing else');
});

test('current receipt with a healthy integration runs no repair', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  await ensureSetup(f.options);
  assert.ok(!f.calls.some((c) => c.startsWith('install')), 'healthy doctor means no install run');
  assert.ok(!f.calls.includes('self-update'));
});

test('integration drift on a current receipt is repaired once with rearm skipped', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  const seenEnv = [];
  const base = f.options.run;
  f.options.run = async (args, binary, env) => {
    if (args[0] === 'doctor') {
      f.calls.push(args.join(' '));
      throw new Error('hooks check failed');
    }
    if (args[0] === 'install') seenEnv.push(env);
    return base(args, binary, env);
  };
  await ensureSetup(f.options);
  assert.deepEqual(f.calls.filter((c) => c.startsWith('doctor')), ['doctor --json'],
    'a failing typed probe is never rerun in human mode');
  assert.equal(f.calls.filter((c) => c.startsWith('install')).length, 1, 'one repair pass');
  assert.equal(seenEnv.length, 1);
  assert.equal(seenEnv[0].STATEROOT_SKIP_REARM, '1');
  assert.equal(seenEnv[0].STATEROOT_INSTALL_VIA, 'extension');
  assert.equal(f.store[SETUP_KEY].extensionVersion, '0.2.19', 'receipt refreshed after repair');
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
  assert.ok(f.calls.some((c) => c.startsWith('install')));
});

test('explicit auto-update opt-out still configures agents', async () => {
  const f = fixture();
  f.options.noAutoUpdate = true;
  await ensureSetup(f.options);
  assert.ok(!f.calls.includes('self-update'));
  assert.ok(f.calls.some((c) => c.startsWith('install')));
});

test('retry setup repairs same-version integrations', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  await ensureSetup({ ...f.options, retry: true });
  assert.ok(f.calls.some((c) => c.startsWith('install')));
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

test('typed install --json outcome drives the configured list (C3)', async () => {
  const f = fixture();
  const calls = [];
  f.options.run = async (args, selected, env) => {
    calls.push(args.join(' '));
    if (args[0] === '--version') return 'stateroot 0.2.19';
    if (args[0] === 'doctor') return '';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      return JSON.stringify({
        schema_version: 'stateroot.integration-health.v1',
        generated_at: '2026-10-09T00:00:00Z',
        harnesses: [
          { harness: 'cursor', status: 'configured', detected: true, problems: [] },
          { harness: 'kimi-code', status: 'observed_working', detected: true, problems: [] },
          { harness: 'grok', status: 'missing', detected: false, problems: [] },
          { harness: 'zero', status: 'unknown', detected: false, problems: [] },
        ],
        install: { configured: ['cursor', 'kimi-code'], failed: [], cli_only: false },
      });
    }
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await ensureSetup(f.options);
  assert.ok(calls.includes('install --json'), 'typed install attempted first');
  assert.ok(!calls.includes('install'), 'no human-mode rerun when the document parses');
  assert.deepEqual(f.store[SETUP_KEY].configured, ['cursor', 'kimi-code'],
    'the typed outcome is authoritative');
});

test('a typed document WITHOUT a validated outcome never mints Ready (C3)', async () => {
  // The tolerant derivation is gone: all-unknown rows and no outcome field
  // are not a success — no receipt, no second install.
  for (const doc of [
    { // no install outcome at all, all rows unknown
      schema_version: 'stateroot.integration-health.v1',
      generated_at: '2026-10-09T00:00:00Z',
      harnesses: [{ harness: 'zero', status: 'unknown', detected: false, problems: [] }],
    },
    { // outcome with the wrong field shapes
      schema_version: 'stateroot.integration-health.v1',
      generated_at: '2026-10-09T00:00:00Z',
      harnesses: [],
      install: { configured: 'cursor', failed: [], cli_only: false },
    },
    { // cli_only not a boolean
      schema_version: 'stateroot.integration-health.v1',
      generated_at: '2026-10-09T00:00:00Z',
      harnesses: [],
      install: { configured: [], failed: [], cli_only: 'yes' },
    },
  ]) {
    const f = fixture();
    const calls = [];
    f.options.run = async (args) => {
      calls.push(args.join(' '));
      if (args[0] === '--version') return 'stateroot 0.2.19';
      if (args[0] === 'self-update') return 'auto-update is disabled';
      if (args[0] === 'install' && args[1] === '--json') return JSON.stringify(doc);
      throw new Error('unexpected command: ' + args.join(' '));
    };
    await assert.rejects(ensureSetup(f.options), /unrecognized (document|outcome)/);
    assert.deepEqual(calls.filter((c) => c.startsWith('install')), ['install --json'],
      'no human-mode rerun for an unrecognized typed outcome');
    assert.equal(f.store[SETUP_KEY], undefined, 'no Ready receipt');
  }
});

test('a rejection naming ANOTHER flag is never an unsupported --json fallback (C3)', async () => {
  // The command we ran always contains --json; the parser statement names
  // --other. A side-effectful human-mode retry must NOT happen.
  const f = fixture();
  const calls = [];
  f.options.run = async (args) => {
    calls.push(args.join(' '));
    if (args[0] === '--version') return 'stateroot 0.2.19';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      throw new Error(
        "Command exited with code 2.\nerror: unexpected argument '--other' found\n\nUsage: stateroot install --json"
      );
    }
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await assert.rejects(ensureSetup(f.options), /unexpected argument '--other'/);
  assert.deepEqual(calls.filter((c) => c.startsWith('install')), ['install --json'],
    'an unrelated flag rejection is a real failure — no second install');
  assert.equal(f.store[SETUP_KEY], undefined);
});

test('clap3 Found-argument phrasing is a proven unsupported --json rejection (C3)', async () => {
  const f = fixture();
  const calls = [];
  f.options.run = async (args) => {
    calls.push(args.join(' '));
    if (args[0] === '--version') return 'stateroot 0.1.15';
    if (args[0] === 'doctor') return '';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      throw new Error("error: Found argument '--json' which wasn't expected, or isn't valid in this context");
    }
    if (args[0] === 'install') return 'Installed for: cursor\n';
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await ensureSetup(f.options);
  assert.deepEqual(
    calls.filter((c) => c.startsWith('install')),
    ['install --json', 'install'],
    'exactly one human-mode retry after the proven parse-time rejection',
  );
  assert.deepEqual(f.store[SETUP_KEY].configured, ['cursor']);
});

test('CLI without --json falls back to the human install summary exactly once (C3)', async () => {
  const f = fixture();
  const calls = [];
  f.options.run = async (args, selected, env) => {
    calls.push(args.join(' '));
    if (args[0] === '--version') return 'stateroot 0.1.15';
    if (args[0] === 'doctor') return '';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      throw new Error("error: unexpected argument '--json' found\n\nUsage: stateroot install");
    }
    if (args[0] === 'install') return 'Installed for: cursor, vscode-copilot\n';
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await ensureSetup(f.options);
  assert.deepEqual(
    calls.filter((c) => c.startsWith('install')),
    ['install --json', 'install'],
    'exactly one human-mode retry after the flag is rejected',
  );
  assert.deepEqual(f.store[SETUP_KEY].configured, ['cursor', 'vscode-copilot']);
});

test('a real install failure is never retried in human mode (C3)', async () => {
  const f = fixture();
  const calls = [];
  f.options.run = async (args) => {
    calls.push(args.join(' '));
    if (args[0] === '--version') return 'stateroot 0.2.19';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      throw new Error('Command timed out after 600s.');
    }
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await assert.rejects(ensureSetup(f.options), /timed out/);
  assert.deepEqual(calls.filter((c) => c.startsWith('install')), ['install --json'],
    'a timeout/failure is NOT a flag rejection — no second install, no duplicate side effects');
  assert.equal(f.store[SETUP_KEY], undefined, 'no Ready receipt');
});

test('malformed typed output writes no success receipt (C3)', async () => {
  const f = fixture();
  const calls = [];
  f.options.run = async (args) => {
    calls.push(args.join(' '));
    if (args[0] === '--version') return 'stateroot 0.2.19';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') return '{not json';
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await assert.rejects(ensureSetup(f.options), /malformed output/);
  assert.deepEqual(calls.filter((c) => c.startsWith('install')), ['install --json'],
    'malformed typed output never falls back to a second real install');
  assert.equal(f.store[SETUP_KEY], undefined);
});

test('a partial integration fails explicitly and writes no Ready receipt (C3)', async () => {
  const f = fixture();
  f.options.run = async (args) => {
    if (args[0] === '--version') return 'stateroot 0.2.19';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      return JSON.stringify({
        schema_version: 'stateroot.integration-health.v1',
        generated_at: '2026-10-09T00:00:00Z',
        harnesses: [
          { harness: 'cursor', status: 'configured', detected: true, problems: [] },
          { harness: 'kimi-code', status: 'missing', detected: true, problems: ['hooks: no hook registration'] },
        ],
        install: { configured: ['cursor'], failed: ['kimi-code'], cli_only: false },
      });
    }
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await assert.rejects(ensureSetup(f.options), /incomplete for: kimi-code/);
  assert.equal(f.store[SETUP_KEY], undefined, 'partial integration is never a success receipt');
});

test('a no-agent machine is an explicit CLI-only success (C3)', async () => {
  const f = fixture();
  f.options.run = async (args) => {
    if (args[0] === '--version') return 'stateroot 0.2.19';
    if (args[0] === 'self-update') return 'auto-update is disabled';
    if (args[0] === 'install' && args[1] === '--json') {
      return JSON.stringify({
        schema_version: 'stateroot.integration-health.v1',
        generated_at: '2026-10-09T00:00:00Z',
        harnesses: [],
        install: { configured: [], failed: [], cli_only: true },
      });
    }
    throw new Error('unexpected command: ' + args.join(' '));
  };
  await ensureSetup(f.options);
  const receipt = f.store[SETUP_KEY];
  assert.ok(receipt, 'CLI-only is a legitimate completed setup');
  assert.equal(receipt.setupMode, 'cli_only');
  assert.deepEqual(receipt.configured, []);
  assert.match(f.reports.at(-1).detail, /CLI only/);
  assert.equal(f.reports.at(-1).phase, 'ready');
});

test('cached receipt consumes TYPED doctor readiness: a missing detected row is repaired (C2)', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  const base = f.options.run;
  f.options.run = async (args, binary, env) => {
    if (args[0] === 'doctor' && args[1] === '--json') {
      return JSON.stringify({
        schema_version: 'stateroot.doctor.v1',
        ok: true, // base checks pass — integration readiness is SEPARATE
        integrations: {
          harnesses: [
            { harness: 'cursor', status: 'missing' },
            { harness: 'vscode-copilot', status: 'observed_working' },
          ],
        },
      });
    }
    return base(args, binary, env);
  };
  await ensureSetup(f.options);
  assert.deepEqual(
    f.calls.filter((c) => c.startsWith('install')),
    ['install --json'],
    'one repair pass for the drifted integration',
  );
  assert.equal(f.reports.at(-1).phase, 'ready');
});

test('typed doctor with only configured/observed rows means no repair (C2)', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  await ensureSetup(f.options);
  assert.deepEqual(f.calls, ['--version', 'doctor --json']);
});

test('unknown and malformed typed doctor rows cannot validate a cached receipt', async () => {
  for (const row of [{ harness: 'cursor', status: 'unknown' }, null, { harness: 'cursor' }]) {
    const f = fixture();
    await ensureSetup(f.options);
    f.calls.length = 0;
    const base = f.options.run;
    f.options.run = async (args, binary, env) => {
      if (args[0] === 'doctor' && args[1] === '--json') {
        return JSON.stringify({
          schema_version: 'stateroot.doctor.v1', ok: true,
          integrations: { harnesses: [row] },
        });
      }
      return base(args, binary, env);
    };
    await ensureSetup(f.options);
    assert.deepEqual(f.calls.filter((call) => call.startsWith('install')), ['install --json']);
  }
});

test('a doctor timeout is not proof of health and never reruns doctor (C2)', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  const base = f.options.run;
  f.options.run = async (args, binary, env) => {
    if (args[0] === 'doctor') {
      f.calls.push(args.join(' '));
      throw new Error('Command timed out after 600s.');
    }
    return base(args, binary, env);
  };
  await ensureSetup(f.options);
  assert.deepEqual(f.calls.filter((c) => c.startsWith('doctor')), ['doctor --json'],
    'exactly one doctor run — a timeout is unknown, not healthy');
  assert.deepEqual(f.calls.filter((c) => c.startsWith('install')), ['install --json'],
    'unknown readiness falls through to the single repair pass');
});

test('a legacy CLI without doctor --json falls back to the human doctor exit code (C2)', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  const base = f.options.run;
  f.options.run = async (args, binary, env) => {
    if (args[0] === 'doctor' && args[1] === '--json') {
      f.calls.push(args.join(' '));
      throw new Error("error: unexpected argument '--json' found\n\nUsage: stateroot doctor");
    }
    return base(args, binary, env);
  };
  await ensureSetup(f.options);
  assert.deepEqual(f.calls, ['--version', 'doctor --json', 'doctor'],
    'proven parse-time rejection earns exactly one human-mode probe');
  assert.ok(!f.calls.some((c) => c.startsWith('install')), 'healthy human doctor means no repair');
});

test('malformed typed doctor output is not proof of health (C2)', async () => {
  const f = fixture();
  await ensureSetup(f.options);
  f.calls.length = 0;
  const base = f.options.run;
  f.options.run = async (args, binary, env) => {
    if (args[0] === 'doctor' && args[1] === '--json') {
      f.calls.push(args.join(' '));
      return '{not json';
    }
    return base(args, binary, env);
  };
  await ensureSetup(f.options);
  assert.deepEqual(f.calls.filter((c) => c.startsWith('doctor')), ['doctor --json']);
  assert.deepEqual(f.calls.filter((c) => c.startsWith('install')), ['install --json'],
    'malformed readiness falls through to the single repair pass');
});
