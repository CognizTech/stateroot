const assert = require('node:assert/strict');
const test = require('node:test');
const { singleFlight } = require('../out/refresh');

test('timer, watcher and user refreshes share one pending run', async () => {
  let finish;
  let calls = 0;
  const refresh = singleFlight(() => { calls++; return new Promise(resolve => { finish = resolve; }); });
  const first = refresh();
  assert.equal(refresh(), first);
  assert.equal(refresh(), first);
  await Promise.resolve();
  assert.equal(calls, 1);
  finish('fresh');
  assert.equal(await first, 'fresh');
  const next = refresh();
  await Promise.resolve();
  assert.equal(calls, 2);
  finish('new');
  assert.equal(await next, 'new');
});

test('failure releases the refresh slot for recovery', async () => {
  let calls = 0;
  const refresh = singleFlight(async () => { if (++calls === 1) throw new Error('timeout'); return 'recovered'; });
  await assert.rejects(refresh(), /timeout/);
  assert.equal(await refresh(), 'recovered');
});
