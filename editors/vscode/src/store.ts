import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import { tsNewer } from "./freshness";

export const STORE = ".stateroot";
export const CLI_MODE_HARNESSES = ["claude", "codex", "kimi"] as const;

export function readJson<T = Record<string, unknown>>(file: string): T | undefined {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8")) as T;
  } catch {
    return undefined;
  }
}

export function projectRoot(): string | undefined {
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const dir = folder.uri.fsPath;
    if (fs.existsSync(path.join(dir, STORE, "manifest.json"))) {
      return dir;
    }
  }
  return undefined;
}

export interface PlanMeta {
  id: string;
  title: string;
  status: string;
  created_by_harness: string;
  created_at: string;
  updated_at: string;
  notes?: string;
  todosDone?: number;
  todosTotal?: number;
}

export interface DelegationRecord {
  id: string;
  harness: string;
  task: string;
  status?: string;
  outcome?: string;
  pid?: number;
  log?: string;
  ts?: string;
}

export interface RootManifest {
  id: string;
  created_at: string;
  created_reason?: string;
  created_by_harness?: string;
  files_pinned?: number;
  coverage?: string;
  parents?: string[];
}

export interface HandoffPacket {
  seq?: number;
  task?: string;
  objective?: string;
  next_actions?: string[];
  created_by_harness?: string;
  recommended_next_harness?: string;
  accepted_by?: unknown;
  written_at?: string;
  created_at?: string;
}

export interface LatestActivity {
  ts: string;
  harness?: string;
  kind?: string;
}

export interface TodoItem {
  key: string;
  content: string;
  status: string;
}

export interface TodoRecord {
  harness: string;
  session_id: string;
  plan_id?: string | null;
  items: TodoItem[];
  updated_at: string;
}

export function displayHarness(id: string): string {
  if (id === "kimi-code") {
    return "kimi";
  }
  if (id === "claude-code") {
    return "claude";
  }
  return id;
}

export function todoCounts(items: TodoItem[]): { done: number; total: number } {
  const total = items.length;
  const done = items.filter((item) => item.status === "completed").length;
  return { done, total };
}

export function listTodoRecords(root: string): TodoRecord[] {
  const dir = path.join(root, STORE, "todos");
  let harnesses: string[] = [];
  try {
    harnesses = fs.readdirSync(dir);
  } catch {
    return [];
  }
  const rows: TodoRecord[] = [];
  for (const harness of harnesses) {
    const harnessDir = path.join(dir, harness);
    let names: string[] = [];
    try {
      if (!fs.statSync(harnessDir).isDirectory()) {
        continue;
      }
      names = fs.readdirSync(harnessDir).filter((name) => name.endsWith(".json"));
    } catch {
      continue;
    }
    for (const name of names) {
      const rec = readJson<TodoRecord>(path.join(harnessDir, name));
      if (!rec?.harness || !Array.isArray(rec.items)) {
        continue;
      }
      rows.push({
        harness: rec.harness,
        session_id: rec.session_id || name.replace(/\.json$/, ""),
        plan_id: rec.plan_id ?? null,
        items: rec.items.filter((item) => item && typeof item.content === "string"),
        updated_at: rec.updated_at || "",
      });
    }
  }
  rows.sort((a, b) => (b.updated_at || "").localeCompare(a.updated_at || ""));
  return rows;
}

/** Newest session record per harness — same view as `stateroot todo list`. */
export function currentTodoLists(root: string): TodoRecord[] {
  return currentTodoListsFrom(listTodoRecords(root));
}

export function currentTodoListsFrom(records: TodoRecord[]): TodoRecord[] {
  const seen = new Set<string>();
  const current: TodoRecord[] = [];
  const newestFirst = [...records].sort((a, b) => {
    const time = (b.updated_at || "").localeCompare(a.updated_at || "");
    if (time !== 0) {
      return time;
    }
    const aText = todoHasText(a) ? 1 : 0;
    const bText = todoHasText(b) ? 1 : 0;
    if (aText !== bText) {
      return bText - aText;
    }
    return (b.session_id || "").localeCompare(a.session_id || "");
  });
  for (const record of newestFirst) {
    if (seen.has(record.harness)) {
      continue;
    }
    if (
      !todoHasText(record) &&
      newestFirst.some((other) => other.harness === record.harness && todoHasText(other))
    ) {
      continue;
    }
    seen.add(record.harness);
    current.push(record);
  }
  return current;
}

function todoHasText(record: TodoRecord): boolean {
  return (record.items || []).some((item) => (item.content || item.key || "").trim().length > 0);
}

export function todoLabel(item: TodoItem): string {
  return (item.content || item.key || "").trim();
}

export function planBoundTodos(root: string, planId: string): TodoRecord | undefined {
  return planBoundIndex(listTodoRecords(root)).get(planId);
}

export function planBoundIndex(records: TodoRecord[]): Map<string, TodoRecord> {
  const map = new Map<string, TodoRecord>();
  for (const record of records) {
    const planId = record.plan_id;
    if (!planId || record.items.length === 0) {
      continue;
    }
    const existing = map.get(planId);
    if (!existing || (record.updated_at || "") > (existing.updated_at || "")) {
      map.set(planId, record);
    }
  }
  return map;
}

export function listPlans(root: string): PlanMeta[] {
  const dir = path.join(root, STORE, "plans");
  let names: string[] = [];
  try {
    names = fs.readdirSync(dir).filter((f) => f.endsWith(".json"));
  } catch {
    return [];
  }
  const plans: PlanMeta[] = [];
  for (const name of names) {
    const meta = readJson<PlanMeta>(path.join(dir, name));
    if (meta?.id) {
      plans.push(meta);
    }
  }
  plans.sort((a, b) => {
    const rank = planStatusRank(a.status) - planStatusRank(b.status);
    if (rank !== 0) {
      return rank;
    }
    return planTime(b).localeCompare(planTime(a));
  });
  return plans;
}

function planStatusRank(status: string): number {
  switch (status) {
    case "active":
      return 0;
    case "approved":
      return 1;
    case "draft":
      return 2;
    case "done":
      return 3;
    default:
      return 4;
  }
}

function planTime(plan: PlanMeta): string {
  return plan.created_at || plan.id || plan.updated_at || "";
}

export function planBodyPath(root: string, id: string): string {
  return path.join(root, STORE, "plans", `${id}.md`);
}

export function planExcerpt(root: string, id: string, maxLines = 40): string {
  try {
    const text = fs.readFileSync(planBodyPath(root, id), "utf8");
    return text.split(/\r?\n/).slice(0, maxLines).join("\n");
  } catch {
    return "";
  }
}

export function listDelegations(root: string): DelegationRecord[] {
  const dir = path.join(root, STORE, "delegations");
  let names: string[] = [];
  try {
    names = fs.readdirSync(dir).filter((f) => f.endsWith(".json"));
  } catch {
    return [];
  }
  const rows: DelegationRecord[] = [];
  for (const name of names) {
    const rec = readJson<DelegationRecord>(path.join(dir, name));
    if (rec?.id) {
      rows.push(rec);
    }
  }
  rows.sort((a, b) => (b.ts || b.id).localeCompare(a.ts || a.id));
  return rows;
}

export function listRoots(root: string): RootManifest[] {
  const dir = path.join(root, STORE, "roots");
  let names: string[] = [];
  try {
    names = fs.readdirSync(dir).filter((f) => f.endsWith(".json"));
  } catch {
    return [];
  }
  const rows: RootManifest[] = [];
  for (const name of names) {
    const rec = readJson<RootManifest>(path.join(dir, name));
    if (rec?.id) {
      rows.push(rec);
    }
  }
  rows.sort((a, b) => (b.created_at || "").localeCompare(a.created_at || ""));
  return rows;
}

export function readState(root: string): Record<string, unknown> | undefined {
  return readJson(path.join(root, STORE, "project", "state.json"));
}

export function readHandoff(root: string): HandoffPacket | undefined {
  return readJson<HandoffPacket>(path.join(root, STORE, "handoffs", "current.json"));
}

export function readLatestActivity(root: string): LatestActivity | undefined {
  let best: LatestActivity | undefined;
  try {
    const lines = fs
      .readFileSync(path.join(root, STORE, "memories", "episodic.jsonl"), "utf8")
      .split(/\r?\n/);
    for (let index = lines.length - 1; index >= 0; index -= 1) {
      const line = lines[index].trim();
      if (!line) {
        continue;
      }
      const record = JSON.parse(line) as LatestActivity;
      if (typeof record.ts === "string" && record.ts) {
        best = { ts: record.ts, harness: record.harness, kind: "checkpoint" };
        break;
      }
    }
  } catch {
    // A missing or partially-written local journal means no checkpoint signal.
  }
  const latestRoot = listRoots(root)[0];
  if (latestRoot?.created_at) {
    const candidate: LatestActivity = {
      ts: latestRoot.created_at,
      harness: latestRoot.created_by_harness,
      kind: "root",
    };
    if (!best || tsNewer(candidate.ts, best.ts)) {
      best = candidate;
    }
  }
  return best;
}

export function readManifest(root: string): Record<string, unknown> | undefined {
  return readJson(path.join(root, STORE, "manifest.json"));
}

export function shortHash(id: string): string {
  return id.slice(0, 12);
}

export function liveStatus(rec: DelegationRecord): string {
  return rec.outcome || rec.status || "unknown";
}

export interface LearningRecord {
  id: string;
  statement: string;
  category: string;
  confidence: number;
  status: string;
  sources: string;
  scope: string;
}

export interface MemoryEntry {
  index: number;
  text: string;
  preview: string;
  private: boolean;
}

export interface MemoryStore {
  chars: number;
  limit: number;
  entries: MemoryEntry[];
}

export interface WikiPage {
  rel: string;
  title: string;
}

export function learningsDir(root: string): string {
  return path.join(root, STORE, "learnings");
}

export function learningFilePath(root: string, category: string, candidate: boolean): string {
  const dir = candidate
    ? path.join(learningsDir(root), "_candidates")
    : learningsDir(root);
  return path.join(dir, `${category}.md`);
}

export function memoryFilePath(root: string): string {
  return path.join(root, STORE, "memories", "MEMORY.md");
}

export function wikiPagePath(root: string, rel: string): string {
  return path.join(root, STORE, "memories", "pages", ...rel.split("/"));
}

export function listLearnings(root: string): LearningRecord[] {
  const dir = learningsDir(root);
  const rows = [
    ...readLearningDir(dir, false),
    ...readLearningDir(path.join(dir, "_candidates"), true),
  ].filter((row) => row.scope === "project" || !row.scope);
  rows.sort((a, b) => {
    const rank = learningStatusRank(a.status) - learningStatusRank(b.status);
    if (rank !== 0) {
      return rank;
    }
    return b.confidence - a.confidence;
  });
  return rows;
}

function learningStatusRank(status: string): number {
  switch (status) {
    case "candidate":
      return 0;
    case "active":
      return 1;
    default:
      return 2;
  }
}

function readLearningDir(dir: string, candidates: boolean): LearningRecord[] {
  let names: string[] = [];
  try {
    names = fs.readdirSync(dir).filter((name) => name.endsWith(".md") && !name.startsWith("_"));
  } catch {
    return [];
  }
  const rows: LearningRecord[] = [];
  for (const name of names) {
    const category = name.replace(/\.md$/, "");
    let text = "";
    try {
      text = fs.readFileSync(path.join(dir, name), "utf8");
    } catch {
      continue;
    }
    for (const line of text.split(/\r?\n/)) {
      const parsed = parseLearningBullet(line, category);
      if (!parsed) {
        continue;
      }
      if (candidates) {
        parsed.status = "candidate";
      }
      rows.push(parsed);
    }
  }
  return rows;
}

export function parseLearningBullet(line: string, category: string): LearningRecord | undefined {
  const trimmed = line.trim();
  if (!trimmed.startsWith("- **")) {
    return undefined;
  }
  const rest = trimmed.slice(4);
  const end = rest.indexOf("**");
  if (end < 1) {
    return undefined;
  }
  const statement = rest.slice(0, end).trim();
  const commentStart = rest.indexOf("<!--", end);
  const commentEnd = rest.indexOf("-->", commentStart);
  if (commentStart < 0 || commentEnd < 0) {
    return undefined;
  }
  const comment = rest.slice(commentStart + 4, commentEnd);
  let id = "";
  let confidence = 0;
  let sources = "";
  let scope = "project";
  let status = "active";
  for (const part of comment.split(";")) {
    const split = part.trim().indexOf(":");
    if (split < 0) {
      continue;
    }
    const key = part.trim().slice(0, split).trim();
    const value = part.trim().slice(split + 1).trim();
    if (key === "id") {
      id = value;
    } else if (key === "confidence") {
      confidence = Number(value) || 0;
    } else if (key === "sources") {
      sources = value;
    } else if (key === "scope" && value) {
      scope = value;
    } else if (key === "status" && value) {
      status = value;
    }
  }
  if (!id || !statement) {
    return undefined;
  }
  return { id, statement, category, confidence, status, sources, scope };
}

const MEMORY_CHAR_LIMIT = 8000;

export function listMemory(root: string): MemoryStore {
  let text = "";
  try {
    text = fs.readFileSync(memoryFilePath(root), "utf8");
  } catch {
    return { chars: 0, limit: MEMORY_CHAR_LIMIT, entries: [] };
  }
  const entries = splitMemoryEntries(text).map((entry, index) => ({
    index: index + 1,
    text: entry,
    preview: clipText(entry, 160),
    private: entry.includes("<!-- visibility: private -->"),
  }));
  return { chars: text.trim().length, limit: MEMORY_CHAR_LIMIT, entries };
}

export function splitMemoryEntries(text: string): string[] {
  let raw = text.trim();
  if (!raw) {
    return [];
  }
  const lines = raw.split(/\r?\n/);
  if (lines[0]?.trim().startsWith("#")) {
    raw = lines.slice(1).join("\n").trim();
  }
  if (!raw) {
    return [];
  }
  if (raw.includes("§")) {
    return raw
      .split("§")
      .map((entry) => entry.trim())
      .filter((entry) => entry && !isMemoryBoilerplate(entry));
  }
  const out: string[] = [];
  for (const line of raw.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || isMemoryBoilerplate(trimmed) || trimmed.startsWith("#")) {
      continue;
    }
    const entry = trimmed.replace(/^- /, "").trim();
    if (entry) {
      out.push(entry);
    }
  }
  return out;
}

function isMemoryBoilerplate(text: string): boolean {
  const t = text.trim();
  return (
    t === "# Project Memory" ||
    t === "Curated long-term memory for this project." ||
    t.toLowerCase() === "curated long-term memory for this project."
  );
}

export function listWikiPages(root: string): WikiPage[] {
  // OKF v0.2 (stateroot ≥ 0.1.14): pages live in the bundle at wiki/pages.
  // Pre-migration projects still have them at memories/pages until the CLI
  // migrates them on first touch — read both, deduped by relative path.
  const dirs = [
    path.join(root, STORE, "wiki", "pages"),
    path.join(root, STORE, "memories", "pages"),
  ];
  const rows: WikiPage[] = [];
  const seen = new Set<string>();
  for (const dir of dirs) {
    const fromDir: WikiPage[] = [];
    walkWiki(dir, "", fromDir);
    for (const row of fromDir) {
      if (!seen.has(row.rel)) {
        seen.add(row.rel);
        rows.push(row);
      }
    }
  }
  rows.sort((a, b) => a.rel.localeCompare(b.rel));
  return rows;
}

function walkWiki(dir: string, prefix: string, rows: WikiPage[]): void {
  let names: string[] = [];
  try {
    names = fs.readdirSync(dir);
  } catch {
    return;
  }
  for (const name of names) {
    const full = path.join(dir, name);
    let stat: fs.Stats;
    try {
      stat = fs.statSync(full);
    } catch {
      continue;
    }
    const rel = prefix ? `${prefix}/${name}` : name;
    if (stat.isDirectory()) {
      walkWiki(full, rel, rows);
      continue;
    }
    if (!name.endsWith(".md")) {
      continue;
    }
    rows.push({
      rel: rel.replace(/\\/g, "/"),
      title: name.replace(/\.md$/, ""),
    });
  }
}

export function clipText(text: string, max: number): string {
  const compact = text.replace(/\s+/g, " ").trim();
  if (compact.length <= max) {
    return compact;
  }
  return `${compact.slice(0, max - 1)}…`;
}

export function memoryNeedle(entry: string): string {
  const line = entry.split(/\r?\n/)[0]?.trim() || entry.trim();
  return line.length <= 120 ? line : line.slice(0, 120);
}
