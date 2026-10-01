import {
  CLI_MODE_HARNESSES,
  liveStatus,
  type DelegationRecord,
  type HandoffPacket,
  type PlanMeta,
} from "./store";
import type { AttentionItem, ContinuityProjection } from "./continuity";

export type InboxTab = "plans" | "crew" | "control";

export interface InboxItem {
  id: string;
  /** Derived kinds (choose-executor/reassign/accept-handoff) or a projection
   * attention kind verbatim (obligation_due, handoff_routed, …). */
  kind: string;
  title: string;
  detail: string;
  tab: InboxTab;
  planId?: string;
  delegationId?: string;
  obligationId?: string;
  handoffSeq?: number;
  /** The CLI's concrete next action, when the projection carries one. */
  action?: string;
}

export function assembleInbox(input: {
  plans: PlanMeta[];
  delegations: DelegationRecord[];
  handoff?: HandoffPacket;
  thisHarness?: string;
  dismissed?: readonly string[];
  continuity?: ContinuityProjection;
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

  return mergeContinuity(items, input.continuity).filter((item) => !dismissed.has(item.id));
}

/** Which workbench tab a projection attention item opens. */
export function continuityItemTab(item: AttentionItem): InboxTab {
  switch (item.kind) {
    case "plan_receipt_pending":
    case "plan_closure":
    case "plan_unassigned":
      return "plans";
    case "delegation_failed":
      return "crew";
    default:
      return "control";
  }
}

export function continuityInboxItem(item: AttentionItem): InboxItem {
  return {
    id: item.id,
    kind: item.kind,
    title: item.title,
    detail: item.detail,
    tab: continuityItemTab(item),
    planId: item.plan_id,
    delegationId: item.delegation_id,
    obligationId: item.obligation_id,
    handoffSeq: item.handoff_seq,
    action: item.action,
  };
}

/** Fold the CLI's continuity projection into the derived inbox. The
 * projection is the CLI's single assessment, so its items win over the
 * extension's local derivations of the same evidence (routed/stale handoff
 * over accept-handoff, failed delegation over reassign, plan attention over
 * choose-executor). */
export function mergeContinuity(
  derived: InboxItem[],
  continuity?: ContinuityProjection
): InboxItem[] {
  const attention = continuity?.attention ?? [];
  if (!attention.length) {
    return derived;
  }
  const projected: InboxItem[] = [];
  const seen = new Set<string>();
  for (const item of attention) {
    if (seen.has(item.id)) {
      continue;
    }
    seen.add(item.id);
    projected.push(continuityInboxItem(item));
  }
  const superseded = new Set<string>();
  for (const item of projected) {
    if (item.kind === "handoff_routed" || item.kind === "handoff_stale") {
      for (const row of derived) {
        if (row.kind === "accept-handoff") {
          superseded.add(row.id);
        }
      }
    } else if (item.kind === "delegation_failed" && item.delegationId) {
      for (const row of derived) {
        if (
          row.kind === "reassign" &&
          row.delegationId &&
          (row.delegationId.startsWith(item.delegationId) ||
            item.delegationId.startsWith(row.delegationId))
        ) {
          superseded.add(row.id);
        }
      }
    } else if (
      (item.kind === "plan_receipt_pending" ||
        item.kind === "plan_closure" ||
        item.kind === "plan_unassigned") &&
      item.planId
    ) {
      for (const row of derived) {
        if (row.kind === "choose-executor" && row.planId === item.planId) {
          superseded.add(row.id);
        }
      }
    }
  }
  return [...projected, ...derived.filter((row) => !superseded.has(row.id))];
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
