export function nonce(): string {
  const chars = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let out = "";
  for (let i = 0; i < 32; i++) {
    out += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return out;
}

const CSS = `
:root { color-scheme: light dark; }
* { box-sizing: border-box; }
html, body {
  margin: 0;
  padding: 0;
  height: 100%;
}
body {
  font-family: var(--vscode-font-family);
  font-size: var(--vscode-font-size);
  color: var(--vscode-foreground);
  background: var(--vscode-editor-background);
  line-height: 1.4;
}
button, select, input {
  font: inherit;
  color: inherit;
}
button {
  background: var(--vscode-button-background);
  color: var(--vscode-button-foreground);
  border: none;
  padding: 4px 10px;
  cursor: pointer;
}
button:hover { background: var(--vscode-button-hoverBackground); }
button.secondary {
  background: var(--vscode-button-secondaryBackground);
  color: var(--vscode-button-secondaryForeground);
}
button.link, button.tab {
  background: transparent;
  color: var(--vscode-foreground);
  padding: 4px 8px;
}
button.tab.active {
  border-bottom: 2px solid var(--vscode-focusBorder);
  font-weight: 600;
}
button.chip {
  background: var(--vscode-button-secondaryBackground);
  color: var(--vscode-button-secondaryForeground);
  margin: 0 4px 4px 0;
}
button.chip.selected {
  background: var(--vscode-button-background);
  color: var(--vscode-button-foreground);
}
button.danger { background: var(--vscode-inputValidation-errorBackground); }
.muted { color: var(--vscode-descriptionForeground); }
.kicker { font-size: 11px; letter-spacing: 0.04em; text-transform: uppercase; color: var(--vscode-descriptionForeground); }
.row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; }
.col { display: flex; flex-direction: column; gap: 8px; }
.split { display: grid; grid-template-columns: 240px 1fr; min-height: 0; height: 100%; gap: 12px; }
@media (max-width: 640px) { .split { grid-template-columns: 1fr; height: auto; } }
.list { overflow: auto; border-right: 1px solid var(--vscode-widget-border); padding-right: 8px; min-height: 0; }
.item {
  display: block; width: 100%; text-align: left;
  background: transparent; color: inherit; padding: 8px;
  border-left: 2px solid transparent;
}
.item:hover { background: var(--vscode-list-hoverBackground); }
.item.active { border-left-color: var(--vscode-focusBorder); background: var(--vscode-list-activeSelectionBackground); color: var(--vscode-list-activeSelectionForeground); }
.card {
  border: 1px solid var(--vscode-widget-border);
  padding: 10px 12px;
  margin-bottom: 8px;
}
.card.fail { border-left: 2px solid var(--vscode-diffEditor-removedTextBackground, var(--vscode-errorForeground)); }
.warning { color: var(--vscode-editorWarning-foreground); }
.pad { padding: 10px 12px; }
.workbench {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  overflow: hidden;
}
.tabs {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  flex: 0 0 auto;
  position: sticky;
  top: 0;
  z-index: 5;
  padding: 10px 12px 0;
  margin: 0;
  background: var(--vscode-editor-background);
  border-bottom: 1px solid var(--vscode-widget-border);
}
.panel {
  flex: 1 1 auto;
  min-height: 0;
  overflow: auto;
  padding: 12px;
}
.pre {
  white-space: pre-wrap; font-family: var(--vscode-editor-font-family);
  font-size: 12px; margin: 0;
  color: var(--vscode-foreground);
}
.count { font-variant-numeric: tabular-nums; }
.lane { min-width: 180px; flex: 1; }
.todo {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  margin: 4px 0;
}
.todo .mark { font-family: var(--vscode-editor-font-family); min-width: 1.6em; }
.todo.completed { color: var(--vscode-descriptionForeground); }
.need {
  display: flex;
  flex-direction: column;
  gap: 2px;
  margin-bottom: 4px;
}
button.dismiss {
  background: transparent;
  color: var(--vscode-descriptionForeground);
  padding: 2px 8px;
  align-self: flex-start;
}
`;

function shell(nonceVal: string, body: string, script: string): string {
  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonceVal}';" />
  <style>${CSS}</style>
</head>
<body>
${body}
<script nonce="${nonceVal}">${script}</script>
</body>
</html>`;
}

export function glanceHtml(nonceVal: string): string {
  const body = `
<div class="pad col" id="app">
  <div class="kicker">Now</div>
  <div id="now">No project</div>
  <div class="kicker">Needs you <span class="count" id="need-count">0</span></div>
  <div id="needs"></div>
  <div class="kicker">Todos <span class="count" id="todo-count">0</span></div>
  <div id="todos"></div>
  <div class="kicker">Learnings <span class="count" id="learn-count">0</span></div>
  <div id="learnings"></div>
  <div class="kicker">Memory <span class="count" id="memory-count">0</span></div>
  <div id="memory"></div>
  <div class="kicker">Lineage <span class="count" id="lineage-count">0</span></div>
  <div id="lineage" class="muted">—</div>
  <div id="empty"></div>
</div>`;
  const script = `
const vscode = acquireVsCodeApi();
window.addEventListener('message', (e) => render(e.data));
vscode.postMessage({ type: 'ready' });
function el(html) { const d = document.createElement('div'); d.innerHTML = html; return d.firstElementChild; }
function esc(s) { return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
function relTime(value) {
  const ms = Date.now() - Date.parse(value || '');
  if (!Number.isFinite(ms) || ms < 0) return value || '';
  const minutes = Math.floor(ms / 60000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return minutes + 'm ago';
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return hours + 'h ago';
  return Math.floor(hours / 24) + 'd ago';
}
function todoMark(status) {
  if (status === 'completed') return '[x]';
  if (status === 'in_progress') return '[~]';
  return '[ ]';
}
function harnessName(id) {
  if (id === 'kimi-code') return 'kimi';
  if (id === 'claude-code') return 'claude';
  return id || '?';
}
function clip(s, n) {
  const text = String(s || '').trim();
  if (text.length <= n) return text;
  return text.slice(0, n - 1) + '…';
}
function lineageMeta(root) {
  const bits = [];
  if (root.created_by_harness) bits.push(harnessName(root.created_by_harness));
  if (root.created_at) bits.push(relTime(root.created_at));
  if (typeof root.files_pinned === 'number') bits.push(root.files_pinned + ' files');
  const parent = root.parents && root.parents[0];
  if (parent) bits.push('from ' + String(parent).slice(0, 12));
  return bits.join(' · ');
}
function render(state) {
  if (!state) return;
  if (!state.initialized) {
    document.getElementById('now').textContent = 'No StateRoot project in this workspace.';
    document.getElementById('needs').innerHTML = '';
    document.getElementById('need-count').textContent = '0';
    document.getElementById('todos').innerHTML = '';
    document.getElementById('todo-count').textContent = '0';
    document.getElementById('learnings').innerHTML = '';
    document.getElementById('learn-count').textContent = '0';
    document.getElementById('memory').innerHTML = '';
    document.getElementById('memory-count').textContent = '0';
    document.getElementById('lineage').textContent = '—';
    document.getElementById('lineage-count').textContent = '0';
    document.getElementById('empty').innerHTML = '<button id="init">Initialize</button>';
    document.getElementById('init').onclick = () => vscode.postMessage({ type: 'init' });
    return;
  }
  document.getElementById('empty').innerHTML = '';
  const now = state.now || {};
  const nowButton = document.createElement('button');
  nowButton.className = 'item';
  nowButton.innerHTML =
    '<div><strong>' + esc(now.objective || '—') + '</strong></div>' +
    (now.task ? '<div>' + esc(now.task) + '</div>' : '') +
    (now.nextActions || []).map(action => '<div class="muted">• ' + esc(action) + '</div>').join('') +
    (now.writtenBy || now.writtenAt ? '<div class="muted">written by ' + esc(now.writtenBy || '?') + (now.writtenAt ? ' · ' + esc(relTime(now.writtenAt)) : '') + '</div>' : '') +
    (now.latest ? '<div class="muted">' + esc(now.latest.harness || '?') + ' · ' + esc(now.latest.kind || 'checkpoint') + ' · ' + esc(relTime(now.latest.ts)) + '</div>' : '') +
    (now.todosLabel ? '<div class="muted">' + esc(now.todosLabel) + '</div>' : '') +
    (now.staleNote ? '<div class="warning">' + esc(now.staleNote) + '</div>' : '');
  nowButton.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'control' });
  const nowNode = document.getElementById('now');
  nowNode.innerHTML = '';
  nowNode.appendChild(nowButton);
  const inbox = state.inbox || [];
  document.getElementById('need-count').textContent = String(inbox.length);
  const needs = document.getElementById('needs');
  needs.innerHTML = '';
  inbox.slice(0, 2).forEach((item) => {
    const wrap = document.createElement('div');
    wrap.className = 'need';
    const b = document.createElement('button');
    b.className = 'item';
    b.innerHTML = '<div>' + esc(item.title) + '</div><div class="muted">' + esc(item.detail) + '</div>';
    b.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: item.tab, planId: item.planId, delegationId: item.delegationId });
    const dismiss = document.createElement('button');
    dismiss.className = 'dismiss';
    dismiss.textContent = 'Dismiss';
    dismiss.onclick = (ev) => {
      ev.stopPropagation();
      vscode.postMessage({ type: 'dismiss', id: item.id });
    };
    wrap.appendChild(b);
    wrap.appendChild(dismiss);
    needs.appendChild(wrap);
  });
  if (inbox.length > 2) {
    const more = document.createElement('button');
    more.className = 'link';
    more.textContent = (inbox.length - 2) + ' more — open workbench';
    more.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'control' });
    needs.appendChild(more);
  }
  const lists = state.todos || [];
  const openCount = lists.reduce((n, rec) => n + (rec.items || []).filter(item => item.status !== 'completed').length, 0);
  const totalCount = lists.reduce((n, rec) => n + (rec.items || []).length, 0);
  document.getElementById('todo-count').textContent = totalCount ? (openCount + '/' + totalCount) : '0';
  const todos = document.getElementById('todos');
  todos.innerHTML = '';
  if (!lists.length) {
    const empty = document.createElement('div');
    empty.className = 'muted';
    empty.textContent = 'no federated todos';
    todos.appendChild(empty);
  } else {
    lists.forEach((rec) => {
      const done = (rec.items || []).filter(item => item.status === 'completed').length;
      const total = (rec.items || []).length;
      const b = document.createElement('button');
      b.className = 'item';
      const bind = rec.plan_id ? 'plan-bound' : 'standalone';
      b.innerHTML = '<div>' + esc(harnessName(rec.harness)) + ' · ' + esc(bind) + ' · todos ' + done + '/' + total + '</div>' +
        (rec.items || []).slice(0, 3).map(item => '<div class="muted">' + todoMark(item.status) + ' ' + esc(item.content || item.key || '') + '</div>').join('');
      b.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'todos' });
      todos.appendChild(b);
    });
  }
  const learnings = state.learnings || [];
  document.getElementById('learn-count').textContent = String(learnings.length);
  const learnNode = document.getElementById('learnings');
  learnNode.innerHTML = '';
  if (!learnings.length) {
    const empty = document.createElement('div');
    empty.className = 'muted';
    empty.textContent = 'no project learnings';
    learnNode.appendChild(empty);
  } else {
    learnings.slice(0, 3).forEach((row) => {
      const b = document.createElement('button');
      b.className = 'item';
      b.innerHTML = '<div>' + esc(row.statement) + '</div><div class="muted">' + esc(row.status) + ' · ' + esc(row.category) + '</div>';
      b.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'learnings', learningId: row.id });
      learnNode.appendChild(b);
    });
    if (learnings.length > 3) {
      const more = document.createElement('button');
      more.className = 'link';
      more.textContent = (learnings.length - 3) + ' more — open workbench';
      more.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'learnings' });
      learnNode.appendChild(more);
    }
  }
  const memory = state.memory || { entries: [] };
  const wikiPages = state.wikiPages || [];
  document.getElementById('memory-count').textContent = String((memory.entries || []).length);
  const memoryNode = document.getElementById('memory');
  memoryNode.innerHTML = '';
  if (!(memory.entries || []).length && !wikiPages.length) {
    const empty = document.createElement('div');
    empty.className = 'muted';
    empty.textContent = 'no project memory';
    memoryNode.appendChild(empty);
  } else {
    (memory.entries || []).slice(0, 3).forEach((entry) => {
      const b = document.createElement('button');
      b.className = 'item';
      b.innerHTML = '<div>' + esc(entry.preview || entry.text) + '</div><div class="muted">MEMORY.md · ' + entry.index + (entry.private ? ' · private' : '') + '</div>';
      b.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'memory', memoryIndex: entry.index });
      memoryNode.appendChild(b);
    });
    if ((memory.entries || []).length > 3) {
      const more = document.createElement('button');
      more.className = 'link';
      more.textContent = (memory.entries.length - 3) + ' more facts — open workbench';
      more.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'memory' });
      memoryNode.appendChild(more);
    } else if (wikiPages.length) {
      const more = document.createElement('button');
      more.className = 'link';
      more.textContent = wikiPages.length + ' wiki page' + (wikiPages.length === 1 ? '' : 's') + ' — open workbench';
      more.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'memory' });
      memoryNode.appendChild(more);
    }
  }
  const roots = state.roots || [];
  document.getElementById('lineage-count').textContent = String(roots.length);
  const lineage = document.getElementById('lineage');
  lineage.innerHTML = '';
  if (!roots.length) {
    const empty = document.createElement('div');
    empty.className = 'muted';
    empty.textContent = 'no roots yet';
    lineage.appendChild(empty);
  } else {
    const shown = roots.slice(0, 5);
    shown.forEach((root, index) => {
      const b = document.createElement('button');
      b.className = 'item';
      const hash = (root.id || '').slice(0, 12);
      const reason = clip(root.created_reason, 72);
      const meta = lineageMeta(root);
      b.innerHTML =
        '<div><code>' + esc(hash || '—') + '</code>' + (index === 0 ? ' <span class="muted">current</span>' : '') + '</div>' +
        (reason ? '<div>' + esc(reason) + '</div>' : '') +
        (meta ? '<div class="muted">' + esc(meta) + '</div>' : '');
      b.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'lineage', rootId: root.id });
      lineage.appendChild(b);
    });
    if (roots.length > shown.length) {
      const more = document.createElement('button');
      more.className = 'link';
      more.textContent = (roots.length - shown.length) + ' more — open workbench';
      more.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'lineage' });
      lineage.appendChild(more);
    }
  }
  if (inbox.length <= 2) {
    const open = document.createElement('button');
    open.className = 'secondary';
    open.textContent = 'Open workbench';
    open.onclick = () => vscode.postMessage({ type: 'openWorkbench', tab: 'control' });
    needs.appendChild(open);
  }
  if (state.emptyProject) {
    document.getElementById('empty').innerHTML =
      '<div class="row"><button id="handoff">Write first handoff</button><button class="secondary" id="checkpoint">Checkpoint</button></div>';
    document.getElementById('handoff').onclick = () => vscode.postMessage({ type: 'handoff' });
    document.getElementById('checkpoint').onclick = () => vscode.postMessage({ type: 'checkpoint' });
  }
}
`;
  return shell(nonceVal, body, script);
}

export function workbenchHtml(nonceVal: string): string {
  const body = `
<div class="workbench">
  <div class="tabs">
    <button class="tab" data-tab="control">Control</button>
    <button class="tab" data-tab="plans">Plans</button>
    <button class="tab" data-tab="todos">Todos</button>
    <button class="tab" data-tab="crew">Crew</button>
    <button class="tab" data-tab="learnings">Learnings</button>
    <button class="tab" data-tab="memory">Memory</button>
    <button class="tab" data-tab="lineage">Lineage</button>
  </div>
  <div class="panel" id="panel"></div>
</div>`;
  const script = `
const vscode = acquireVsCodeApi();
let state = Object.assign({ tab: 'control', harnesses: ['claude','codex','kimi'] }, vscode.getState() || {});
window.addEventListener('message', (e) => {
  state = Object.assign(state, e.data || {});
  if (state.tab) vscode.setState({ tab: state.tab, selectedPlanId: state.selectedPlanId });
  render();
});
vscode.postMessage({ type: 'ready' });
document.querySelectorAll('.tab').forEach(btn => {
  btn.addEventListener('click', () => {
    const tab = btn.getAttribute('data-tab');
    state.tab = tab;
    vscode.setState({ tab: state.tab, selectedPlanId: state.selectedPlanId });
    vscode.postMessage({ type: 'openTab', tab: state.tab });
    render();
  });
});
document.getElementById('panel').addEventListener('click', (event) => {
  const target = event.target;
  const el = target && target.nodeType === 1 ? target : target && target.parentElement;
  const btn = el && el.closest('[data-act]');
  if (!btn) return;
  const act = btn.getAttribute('data-act');
  const id = btn.getAttribute('data-id');
  if (act === 'jump') {
    const item = (state.inbox || [])[Number(btn.getAttribute('data-i'))];
    if (!item) return;
    if (item.tab) state.tab = item.tab;
    if (item.planId) state.selectedPlanId = item.planId;
    vscode.setState({ tab: state.tab, selectedPlanId: state.selectedPlanId });
    vscode.postMessage({
      type: 'openTab',
      tab: item.tab,
      planId: item.planId,
      delegationId: item.delegationId,
      kind: item.kind
    });
    render();
  } else if (act === 'selectPlan') { vscode.postMessage({ type: 'selectPlan', id }); }
  else if (act === 'pickHarness') { state.selectedHarness = id; render(); }
  else if (act === 'approve') vscode.postMessage({ type: 'approvePlan', id });
  else if (act === 'donePlan') vscode.postMessage({ type: 'donePlan', id });
  else if (act === 'delegate') vscode.postMessage({ type: 'delegatePlan', id, harness: state.selectedHarness });
  else if (act === 'openPlan') vscode.postMessage({ type: 'openPlan', id });
  else if (act === 'reassign') vscode.postMessage({ type: 'reassign', id });
  else if (act === 'dismiss') vscode.postMessage({ type: 'dismiss', id });
  else if (act === 'log') vscode.postMessage({ type: 'log', id });
  else if (act === 'selectRoot') vscode.postMessage({ type: 'selectRoot', id });
  else if (act === 'compare') vscode.postMessage({ type: 'compare' });
  else if (act === 'diff') vscode.postMessage({ type: 'diff' });
  else if (act === 'revert') vscode.postMessage({ type: 'revert' });
  else if (act === 'fork') vscode.postMessage({ type: 'fork' });
  else if (act === 'selectLearning') vscode.postMessage({ type: 'selectLearning', id });
  else if (act === 'addLearning') vscode.postMessage({ type: 'addLearning' });
  else if (act === 'editLearning') vscode.postMessage({ type: 'editLearning', id });
  else if (act === 'acceptLearning') vscode.postMessage({ type: 'acceptLearning', id });
  else if (act === 'rejectLearning') vscode.postMessage({ type: 'rejectLearning', id });
  else if (act === 'openLearning') vscode.postMessage({ type: 'openLearning', id });
  else if (act === 'selectMemory') vscode.postMessage({ type: 'selectMemory', index: Number(id) });
  else if (act === 'addMemory') vscode.postMessage({ type: 'addMemory' });
  else if (act === 'editMemory') vscode.postMessage({ type: 'editMemory', index: Number(id) });
  else if (act === 'removeMemory') vscode.postMessage({ type: 'removeMemory', index: Number(id) });
  else if (act === 'openMemoryFile') vscode.postMessage({ type: 'openMemoryFile' });
  else if (act === 'openWiki') vscode.postMessage({ type: 'openWiki', rel: id });
});
function esc(s) { return String(s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c])); }
function render() {
  document.querySelectorAll('.tab').forEach(btn => btn.classList.toggle('active', btn.getAttribute('data-tab') === state.tab));
  const panel = document.getElementById('panel');
  if (state.tab === 'control') panel.innerHTML = control();
  else if (state.tab === 'plans') panel.innerHTML = plans();
  else if (state.tab === 'todos') panel.innerHTML = todos();
  else if (state.tab === 'crew') panel.innerHTML = crew();
  else if (state.tab === 'learnings') panel.innerHTML = learningsPanel();
  else if (state.tab === 'memory') panel.innerHTML = memoryPanel();
  else panel.innerHTML = lineage();
}
function todoMark(status) {
  if (status === 'completed') return '[x]';
  if (status === 'in_progress') return '[~]';
  return '[ ]';
}
function harnessName(id) {
  if (id === 'kimi-code') return 'kimi';
  if (id === 'claude-code') return 'claude';
  return id || '?';
}
function relTime(value) {
  const ms = Date.now() - Date.parse(value || '');
  if (!Number.isFinite(ms) || ms < 0) return value || '';
  const minutes = Math.floor(ms / 60000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return minutes + 'm ago';
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return hours + 'h ago';
  return Math.floor(hours / 24) + 'd ago';
}
function clip(s, n) {
  const text = String(s || '').trim();
  if (text.length <= n) return text;
  return text.slice(0, n - 1) + '…';
}
function lineageMeta(root) {
  const bits = [];
  if (root.created_by_harness) bits.push(harnessName(root.created_by_harness));
  if (root.created_at) bits.push(relTime(root.created_at));
  if (typeof root.files_pinned === 'number') bits.push(root.files_pinned + ' files');
  const parent = root.parents && root.parents[0];
  if (parent) bits.push('from ' + String(parent).slice(0, 12));
  return bits.join(' · ');
}
function todoList(items) {
  if (!items || !items.length) return '<div class="muted">No items.</div>';
  return items.map(item =>
    '<div class="todo' + (item.status === 'completed' ? ' completed' : '') + '"><span class="mark">' + todoMark(item.status) + '</span><span>' + esc(item.content || item.key || '') + '</span></div>'
  ).join('');
}
function control() {
  const inbox = state.inbox || [];
  if (!inbox.length) return '<div class="muted">Nothing needs you.</div>';
  return inbox.map((item, i) =>
    '<div class="card"><div class="kicker">' + esc(item.kind) + '</div><div>' + esc(item.title) + '</div><div class="muted">' + esc(item.detail) + '</div><div class="row" style="margin-top:8px"><button data-act="jump" data-i="' + i + '">' + esc(openLabel(item)) + '</button><button class="secondary" data-act="dismiss" data-id="' + esc(item.id) + '">Dismiss</button></div></div>'
  ).join('');
}
function openLabel(item) {
  if (item.kind === 'choose-executor') return 'Open plan';
  if (item.kind === 'reassign') return 'Open crew';
  if (item.kind === 'accept-handoff') return 'Review digest';
  return 'Open';
}
function plans() {
  const list = state.plans || [];
  const sel = state.selectedPlanId;
  const plan = list.find(p => p.id === sel) || list[0];
  const left = list.map(p => {
    const todos = (p.todosTotal !== undefined) ? ' · todos ' + (p.todosDone || 0) + '/' + p.todosTotal : '';
    return '<button class="item' + (plan && p.id === plan.id ? ' active' : '') + '" data-act="selectPlan" data-id="' + esc(p.id) + '"><div>' + esc(p.title) + '</div><div class="muted">' + esc(p.status) + ' · ' + esc(p.created_by_harness) + esc(todos) + '</div></button>';
  }).join('') || '<div class="muted">No plans recorded.</div>';
  if (!plan) return '<div class="split"><div class="list">' + left + '</div><div></div></div>';
  const chips = (state.harnesses || []).map(h =>
    '<button class="chip' + (state.selectedHarness === h ? ' selected' : '') + '" data-act="pickHarness" data-id="' + esc(h) + '">' + esc(h) + '</button>'
  ).join('');
  const approve = plan.status === 'draft' ? '<button data-act="approve" data-id="' + esc(plan.id) + '">Approve</button>' : '';
  const done = (plan.status === 'approved' || plan.status === 'active') ? '<button data-act="donePlan" data-id="' + esc(plan.id) + '">Mark done</button>' : '';
  const todosLabel = (plan.todosTotal !== undefined) ? ' · todos ' + (plan.todosDone || 0) + '/' + plan.todosTotal : '';
  const planTodoBlock = (state.planTodos && state.planTodos.length)
    ? '<div class="kicker">Todos</div>' + todoList(state.planTodos)
    : '';
  return '<div class="split"><div class="list">' + left + '</div><div class="col"><div>' + esc(plan.title) + '</div><div class="muted">' + esc(plan.status) + ' · ' + esc(plan.id) + esc(todosLabel) + '</div><pre class="pre">' + esc(state.planExcerpt || '') + '</pre>' + planTodoBlock + '<div class="kicker">Assign execution</div><div>' + chips + '</div><div class="row">' + approve + done + '<button data-act="delegate" data-id="' + esc(plan.id) + '">Delegate plan</button><button class="secondary" data-act="openPlan" data-id="' + esc(plan.id) + '">Open full plan</button></div></div></div>';
}
function todos() {
  const lists = state.todos || [];
  if (!lists.length) return '<div class="muted">no federated todos</div>';
  return lists.map(rec => {
    const done = (rec.items || []).filter(item => item.status === 'completed').length;
    const total = (rec.items || []).length;
    const bind = rec.plan_id ? 'plan-bound ' + rec.plan_id : 'standalone';
    return '<div class="card"><div class="kicker">' + esc(harnessName(rec.harness)) + ' · ' + esc(bind) + ' · todos ' + done + '/' + total + '</div>' + todoList(rec.items) + '</div>';
  }).join('');
}
function crew() {
  const rows = state.delegations || [];
  const by = {};
  rows.forEach(r => { (by[r.harness] = by[r.harness] || []).push(r); });
  const harnesses = Object.keys(by);
  if (!harnesses.length) return '<div class="muted">No delegations yet. Cursor itself is never a crew card.</div>';
  return '<div class="row" style="align-items:flex-start">' + harnesses.map(h => {
    const cards = by[h].map(r => {
      const fail = ['failed','lost','timed_out'].includes(r.status);
      const reassign = fail && !r.closedPlan;
      return '<div class="card' + (fail ? ' fail' : '') + '"><div>' + esc(r.status) + '</div><div class="muted">' + esc(r.task) + '</div><div class="muted">' + esc(r.id) + '</div>' +
        (reassign ? '<div class="row" style="margin-top:8px"><button data-act="reassign" data-id="' + esc(r.id) + '">Reassign</button><button class="secondary" data-act="log" data-id="' + esc(r.id) + '">Log</button></div>' : '<div class="row" style="margin-top:8px"><button class="secondary" data-act="log" data-id="' + esc(r.id) + '">Log</button></div>') +
        '</div>';
    }).join('');
    return '<div class="lane"><div class="kicker">' + esc(h) + '</div>' + cards + '</div>';
  }).join('') + '</div>';
}
function learningsPanel() {
  const rows = state.learnings || [];
  const sel = state.selectedLearningId;
  const current = rows.find(r => r.id === sel) || rows[0];
  const left = rows.map(r =>
    '<button class="item' + (current && r.id === current.id ? ' active' : '') + '" data-act="selectLearning" data-id="' + esc(r.id) + '"><div>' + esc(r.statement) + '</div><div class="muted">' + esc(r.status) + ' · ' + esc(r.category) + ' · ' + esc(String(r.confidence)) + '</div></button>'
  ).join('') || '<div class="muted">No project learnings.</div>';
  const record = '<div class="row"><button data-act="addLearning">Record learning</button></div>';
  if (!current) return record + '<div class="muted">Project learnings are taste — not facts, and not user-global notes.</div>';
  const candidate = current.status === 'candidate';
  const actions = '<div class="row">' +
    '<button data-act="editLearning" data-id="' + esc(current.id) + '">Edit</button>' +
    '<button class="secondary" data-act="openLearning" data-id="' + esc(current.id) + '">Open file</button>' +
    (candidate ? '<button data-act="acceptLearning" data-id="' + esc(current.id) + '">Accept</button><button class="secondary" data-act="rejectLearning" data-id="' + esc(current.id) + '">Reject</button>' : '') +
    '</div>';
  return '<div class="split"><div class="list">' + left + '</div><div class="col">' + record +
    '<div class="muted">' + esc(current.id) + ' · ' + esc(current.status) + ' · ' + esc(current.category) + (current.sources ? ' · ' + esc(current.sources) : '') + '</div>' +
    '<pre class="pre">' + esc(current.statement) + '</pre>' + actions +
    '</div></div>';
}
function memoryPanel() {
  const store = state.memory || { entries: [], chars: 0, limit: 8000 };
  const entries = store.entries || [];
  const sel = Number(state.selectedMemoryIndex);
  const current = entries.find(e => e.index === sel) || entries[0];
  const left = entries.map(e =>
    '<button class="item' + (current && e.index === current.index ? ' active' : '') + '" data-act="selectMemory" data-id="' + e.index + '"><div>' + esc(e.preview || e.text) + '</div><div class="muted">#' + e.index + (e.private ? ' · private' : '') + '</div></button>'
  ).join('') || '<div class="muted">No MEMORY.md entries.</div>';
  const usage = (store.chars || 0) + '/' + (store.limit || 8000) + ' chars · ' + entries.length + ' entries';
  const record = '<div class="row"><button data-act="addMemory">Add fact</button><button class="secondary" data-act="openMemoryFile">Open MEMORY.md</button></div><div class="muted">' + esc(usage) + '</div>';
  const detail = current
    ? '<pre class="pre">' + esc(current.text) + '</pre><div class="row"><button data-act="editMemory" data-id="' + current.index + '">Edit</button><button class="secondary" data-act="removeMemory" data-id="' + current.index + '">Remove</button></div>'
    : '<div class="muted">Project facts only — USER.md and global memory stay out of this list.</div>';
  const pages = state.wikiPages || [];
  const wiki = '<div class="kicker">Wiki pages</div>' + (pages.length
    ? pages.map(p => '<button class="item" data-act="openWiki" data-id="' + esc(p.rel) + '"><div>' + esc(p.title) + '</div><div class="muted">' + esc(p.rel) + '</div></button>').join('')
    : '<div class="muted">No compiled wiki pages.</div>');
  return '<div class="split"><div class="list">' + left + wiki + '</div><div class="col">' + record + detail + '</div></div>';
}
function lineage() {
  const roots = state.roots || [];
  const a = state.rootA, b = state.rootB;
  const currentId = roots[0] && roots[0].id;
  const left = roots.map(r => {
    const hash = (r.id || '').slice(0, 12);
    const reason = clip(r.created_reason, 88);
    const meta = lineageMeta(r);
    const mark = r.id === currentId ? ' <span class="muted">current</span>' : '';
    return '<button class="item' + (r.id === a || r.id === b ? ' active' : '') + '" data-act="selectRoot" data-id="' + esc(r.id) + '"><div><code>' + esc(hash) + '</code>' + mark + '</div>' +
      (reason ? '<div>' + esc(reason) + '</div>' : '') +
      (meta ? '<div class="muted">' + esc(meta) + '</div>' : '') +
      '</button>';
  }).join('') || '<div class="muted">No roots yet.</div>';
  return '<div class="split"><div class="list">' + left + '</div><div class="col"><div class="muted">Select two roots (click twice).</div><div>A: <code>' + esc((a||'').slice(0,12) || '—') + '</code> · B: <code>' + esc((b||'').slice(0,12) || '—') + '</code></div><div class="row"><button data-act="compare">Compare</button><button class="secondary" data-act="diff">Open native diff</button><button class="secondary" data-act="revert">Restore</button><button class="secondary" data-act="fork">Fork</button></div><pre class="pre">' + esc(state.compareText || '') + '</pre></div></div>';
}
`;
  return shell(nonceVal, body, script);
}
