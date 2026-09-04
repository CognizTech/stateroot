import * as path from "path";
import {
  CLI_MODE_HARNESSES,
  currentTodoListsFrom,
  listLearnings,
  listMemory,
  listPlans,
  listDelegations,
  listRoots,
  listTodoRecords,
  listWikiPages,
  liveStatus,
  planBoundIndex,
  planExcerpt,
  projectRoot,
  readHandoff,
  readLatestActivity,
  readManifest,
  readState,
  shortHash,
  todoCounts,
  type DelegationRecord,
  type LearningRecord,
  type MemoryStore,
  type PlanMeta,
  type RootManifest,
  type TodoItem,
  type TodoRecord,
  type WikiPage,
} from "./store";
import { assembleInbox, delegationTargetsClosedPlan, type InboxItem } from "./inbox";
import { handoffBoundary, handoffIsStale, staleHandoffNote } from "./freshness";

export interface Snapshot {
  initialized: boolean;
  now: {
    objective: string;
    task?: string;
    nextActions: string[];
    writtenBy?: string;
    writtenAt?: string;
    staleNote?: string;
    todosLabel?: string;
    latest?: { harness?: string; kind?: string; ts: string };
  };
  emptyProject: boolean;
  inbox: InboxItem[];
  latestRoot: string;
  plans: PlanMeta[];
  selectedPlanId?: string;
  planExcerpt?: string;
  planTodos?: TodoItem[];
  todos: TodoRecord[];
  harnesses: string[];
  selectedHarness?: string;
  delegations: Array<DelegationRecord & { status: string; closedPlan?: boolean }>;
  roots: RootManifest[];
  rootA?: string;
  rootB?: string;
  compareText?: string;
  tab?: string;
  learnings: LearningRecord[];
  selectedLearningId?: string;
  memory: MemoryStore;
  selectedMemoryIndex?: number;
  wikiPages: WikiPage[];
}

export function snapshot(opts?: {
  selectedPlanId?: string;
  selectedHarness?: string;
  rootA?: string;
  rootB?: string;
  compareText?: string;
  liveDelegations?: Array<{ id: string; harness: string; status: string; task: string }>;
  tab?: string;
  dismissedInbox?: string[];
  selectedLearningId?: string;
  selectedMemoryIndex?: number;
}): Snapshot | { initialized: false } {
  const root = projectRoot();
  if (!root) {
    return { initialized: false };
  }
  const state = readState(root);
  const handoff = readHandoff(root);
  const manifest = readManifest(root);
  const todoRecords = listTodoRecords(root);
  const todos = currentTodoListsFrom(todoRecords);
  const bound = planBoundIndex(todoRecords);
  const plans = listPlans(root).map((plan) => withPlanTodos(plan, bound.get(plan.id)));
  const storeDelegations = listDelegations(root);
  const live = opts?.liveDelegations;
  const delegations: Array<DelegationRecord & { status: string; closedPlan?: boolean }> =
    storeDelegations.map((rec) => {
      const fromLive = live?.find((row) => rec.id.startsWith(row.id) || row.id.startsWith(rec.id));
      const row = { ...rec, status: fromLive?.status || liveStatus(rec) };
      return { ...row, closedPlan: delegationTargetsClosedPlan(row, plans) };
    });
  if (live) {
    for (const row of live) {
      if (!delegations.some((d) => d.id === row.id || d.id.startsWith(row.id))) {
        const rec = {
          id: row.id,
          harness: row.harness,
          // Live `delegate list` truncates task text; store JSON is preferred
          // above. Live-only rows still go through prefix closed-plan matching.
          task: row.task,
          status: row.status,
        };
        delegations.push({
          ...rec,
          closedPlan: delegationTargetsClosedPlan(rec, plans),
        });
      }
    }
  }
  const roots = listRoots(root);
  const learnings = listLearnings(root);
  const memory = listMemory(root);
  const wikiPages = listWikiPages(root);
  const latestActivity = readLatestActivity(root);
  const inbox = assembleInbox({
    plans,
    delegations,
    handoff,
    thisHarness: "cursor",
    dismissed: opts?.dismissedInbox,
  });
  const writtenAt = handoff ? handoffBoundary(handoff) : undefined;
  const objective =
    (typeof handoff?.objective === "string" && handoff.objective.trim()) ||
    (typeof state?.objective === "string" && String(state.objective).trim()) ||
    (typeof manifest?.name === "string" ? String(manifest.name) : path.basename(root));
  const stale = Boolean(handoff && handoffIsStale(writtenAt, latestActivity?.ts));
  const now = {
    objective,
    task: typeof handoff?.task === "string" ? handoff.task.trim() || undefined : undefined,
    nextActions: Array.isArray(handoff?.next_actions)
      ? handoff.next_actions.filter((action): action is string => typeof action === "string")
      : [],
    writtenBy:
      typeof handoff?.created_by_harness === "string"
        ? handoff.created_by_harness
        : undefined,
    writtenAt,
    staleNote:
      stale && handoff
        ? staleHandoffNote(handoff.seq, handoff.created_by_harness)
        : undefined,
    todosLabel: activePlanTodosLabel(plans),
    latest: latestActivity,
  };
  const selectedPlanId =
    opts?.selectedPlanId ||
    plans.find((p) => p.status === "active")?.id ||
    plans.find((p) => p.status === "approved")?.id ||
    plans.find((p) => p.status === "draft")?.id ||
    plans[0]?.id;
  const selectedTodos = selectedPlanId ? bound.get(selectedPlanId) : undefined;
  return {
    initialized: true,
    now,
    emptyProject: !handoff && roots.length === 0 && !latestActivity,
    inbox,
    latestRoot: roots[0] ? shortHash(roots[0].id) : "",
    plans,
    selectedPlanId,
    planExcerpt: selectedPlanId ? planExcerpt(root, selectedPlanId) : "",
    planTodos: selectedTodos?.items,
    todos,
    harnesses: [...CLI_MODE_HARNESSES],
    selectedHarness: opts?.selectedHarness,
    delegations,
    roots,
    rootA: opts?.rootA,
    rootB: opts?.rootB,
    compareText: opts?.compareText,
    tab: opts?.tab || "control",
    learnings,
    selectedLearningId: opts?.selectedLearningId,
    memory,
    selectedMemoryIndex: opts?.selectedMemoryIndex,
    wikiPages,
  };
}

function withPlanTodos(plan: PlanMeta, record?: TodoRecord): PlanMeta {
  if (!record) {
    return plan;
  }
  const { done, total } = todoCounts(record.items);
  return { ...plan, todosDone: done, todosTotal: total };
}

function activePlanTodosLabel(plans: PlanMeta[]): string | undefined {
  const plan =
    plans.find((row) => row.status === "active") ||
    plans.find((row) => row.status === "approved") ||
    plans.find((row) => row.status === "draft");
  if (!plan || plan.todosTotal === undefined) {
    return undefined;
  }
  return `todos ${plan.todosDone ?? 0}/${plan.todosTotal} complete`;
}
