const assert = require("node:assert/strict");
const vm = require("node:vm");
const { workbenchHtml } = require("../../out/ui");

/** Drive the real webview script without an installed editor host. */
function loadWorkbench() {
  const match = workbenchHtml("testnonce").match(/<script nonce="testnonce">([\s\S]*?)<\/script>/);
  assert.ok(match, "workbench HTML carries a nonce'd script");
  const panel = {
    innerHTML: "", listeners: {}, scrollTop: 0, scrollLeft: 0,
    querySelector: () => null,
    addEventListener(type, fn) { this.listeners[type] = fn; },
  };
  const posted = [];
  let messageHandler;
  vm.runInNewContext(match[1], {
    acquireVsCodeApi: () => ({ postMessage: msg => posted.push(msg), getState: () => ({}), setState() {} }),
    window: { addEventListener(type, fn) { if (type === "message") messageHandler = fn; } },
    document: { querySelectorAll: () => [], getElementById: id => id === "panel" ? panel : null },
  });
  assert.equal(typeof messageHandler, "function");
  return { panel, posted, push(data) { messageHandler({ data }); return panel.innerHTML; } };
}

module.exports = { loadWorkbench };
