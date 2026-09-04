import {
  CLI_MODE_HARNESSES,
  liveStatus,
  type DelegationRecord,
  type HandoffPacket,
  type PlanMeta,
} from "./store";

export type InboxTab = "plans" | "crew" | "control";

export interface InboxItem {
  id: string;
  kind: "choose-executor" | "reassign" | "accept-handoff";
  title: string;
  detail: string;
  tab: InboxTab;
  planId?: string;
  delegationId?: string;
}

export function assembleInbox(input: {
  plans: PlanMeta[];
  delegations: DelegationRecord[];
  handoff?: HandoffPacket;
  thisHarness?: string;
  dismissed?: readonly string[];
}): InboxItem[] {
  const items: InboxItem[] = [];
  const thisHarness = input.thisHarness ?? "cursor";
  const dismissed = new Set(input.dismissed ?? []);

  for (const plan of input.plans) {
    if (plan.status !== "active" && plan.status !== "approved") {
      continue;
    }
    if (hasSuccessfulDelegation(plan.id, input.delegations)) {
      continue;
    }
    items.push({
      id: `choose-executor:${plan.id}`,
      kind: "choose-executor",
      title: "Choose executor",
      detail: `${plan.status} · ${plan.title}`,
      tab: "plans",
      planId: plan.id,
    });
  }

  for (const rec of input.delegations) {
    const status = liveStatus(rec);
    if (status !== "failed" && status !== "lost" && status !== "timed_out") {
      continue;
    }
    if (delegationTargetsClosedPlan(rec, input.plans)) {
      continue;
    }
    items.push({
      id: `reassign:${rec.id}`,
      kind: "reassign",
      title: "Reassign",
      detail: `${rec.harness} · ${status} · ${(rec.task || "").slice(0, 80)}`,
      tab: "crew",
      delegationId: rec.id,
    });
  }

  if (input.handoff && !handoffAcceptedBy(input.handoff, thisHarness)) {
    const task = (input.handoff.task || input.handoff.objective || "").trim();
    if (task) {
      items.push({
        id: `accept-handoff:${input.handoff.seq ?? "none"}`,
        kind: "accept-handoff",
        title: "Handoff waiting",
        detail: `#${input.handoff.seq ?? "?"} · ${(input.handoff.created_by_harness || "?").trim()} → ${task.slice(0, 80)}`,
        tab: "control",
      });
    }
  }

  return items.filter((item) => !dismissed.has(item.id));
}

export function isClosedPlanStatus(status: string): boolean {
  return status === "done" || status === "abandoned";
}

/** Plan ids mentioned in a delegation task, including CLI-truncated prefixes. */
export function planRefsFromTask(task: string): string[] {
  const refs: string[] = [];
  const seen = new Set<string>();
  const add = (raw: string, minLen: number) => {
    const cleaned = raw.replace(/\.md$/i, "").replace(/\.+$/, "");
    if (cleaned.length < minLen || seen.has(cleaned)) {
      return;
    }
    seen.add(cleaned);
    refs.push(cleaned);
  };
  // `delegate list` truncates to 60 chars, so this may be `plan_2` — still
  // usable when every matching plan is already closed.
  const pathRe = /\.stateroot\/plans\/(plan_[^\s…]*)/g;
  for (const match of task.matchAll(pathRe)) {
    add(match[1], "plan_".length + 1);
  }
  const matches = task.match(/plan_[^\s…]*/g) ?? [];
  for (const match of matches) {
    add(match, "plan_YYYY-MM-DD".length);
  }
  return refs;
}

function planMatchesRef(planId: string, ref: string): boolean {
  return planId === ref || planId.startsWith(ref);
}

export function delegationTargetsClosedPlan(
  rec: DelegationRecord,
  plans: PlanMeta[]
): boolean {
  const task = rec.task || "";
  if (
    plans.some(
      (plan) =>
        isClosedPlanStatus(plan.status) &&
        (task.includes(plan.id) || task.includes(`.stateroot/plans/${plan.id}`))
    )
  ) {
    return true;
  }
  // `stateroot delegate list` truncates tasks (`plan_2026-08-26…`). A prefix
  // is closed only when every plan it could name is already done/abandoned.
  for (const ref of planRefsFromTask(task)) {
    const hits = plans.filter((plan) => planMatchesRef(plan.id, ref));
    if (hits.length > 0 && hits.every((plan) => isClosedPlanStatus(plan.status))) {
      return true;
    }
  }
  return false;
}

export function hasSuccessfulDelegation(
  planId: string,
  delegations: DelegationRecord[]
): boolean {
  const needle = `.stateroot/plans/${planId}`;
  return delegations.some((rec) => {
    if (liveStatus(rec) !== "completed") {
      return false;
    }
    const task = rec.task || "";
    return task.includes(planId) || task.includes(needle);
  });
}

export function handoffAcceptedBy(handoff: HandoffPacket, harness: string): boolean {
  const raw = handoff.accepted_by;
  if (!raw) {
    return false;
  }
  if (Array.isArray(raw)) {
    return raw.some((entry) => {
      if (typeof entry === "string") {
        return entry === harness || entry.startsWith(harness);
      }
      if (entry && typeof entry === "object" && "harness" in entry) {
        return String((entry as { harness?: string }).harness) === harness;
      }
      return false;
    });
  }
  if (typeof raw === "string") {
    return raw.includes(harness);
  }
  return false;
}

export function isCliMode(id: string): boolean {
  return (CLI_MODE_HARNESSES as readonly string[]).includes(id);
}
