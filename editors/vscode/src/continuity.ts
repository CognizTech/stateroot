/**
 * Continuity contract shared with the CLI (stateroot.continuity.v1).
 * The CLI reconciles already-recorded state into a machine-local projection
 * under `.stateroot/local/projections/`; the extension reads it read-only and
 * surfaces its attention items. A missing, unreadable, or foreign-schema file
 * is "no items" — the same forward-compat posture as the merge-attempt
 * contract. Pure module: no vscode, no CLI, so tests load it directly.
 */

export const CONTINUITY_SCHEMA = "stateroot.continuity.v1";
export const CONTINUITY_ADVISORY_SCHEMA = "stateroot.continuity-advisory.v1";

/** Attention kinds — stable vocabulary, one per evidence source (CLI-owned). */
export const ATTENTION_KINDS = [
  "obligation_due",
  "plan_receipt_pending",
  "plan_closure",
  "handoff_routed",
  "delegation_failed",
  "boundary_journal_manual",
  "plan_unassigned",
  "handoff_stale",
  "service_unhealthy",
  "registry_project_missing",
] as const;

export type AttentionKind = (typeof ATTENTION_KINDS)[number];

/** One derived attention item. `id` is stable (`<kind>:<entity>`), so
 * view-level dismissal survives CLI reconciles. */
export interface AttentionItem {
  id: string;
  kind: string;
  rank: number;
  title: string;
  detail: string;
  /** The concrete next CLI action, when one exists. Display-only hint. */
  action?: string;
  plan_id?: string;
  obligation_id?: string;
  handoff_seq?: number;
  delegation_id?: string;
}

export interface ContinuityProjection {
  schema_version: typeof CONTINUITY_SCHEMA;
  generated_at?: string;
  inputs_hash: string;
  attention: AttentionItem[];
  open_obligations?: number;
  corrupt_obligation_events?: number;
  current_plan_id?: string;
  current_plan_status?: string;
  plan_directive?: string;
  service_registered?: boolean;
  service_kind?: string;
  service_running?: boolean;
  service_last_beat_at?: string;
}

/** Synthesis-written advisory. Provenance is part of the contract: it renders
 * labeled as a synthesized advisory, never as instructions. */
export interface ContinuityAdvisory {
  schema_version: typeof CONTINUITY_ADVISORY_SCHEMA;
  inputs_hash: string;
  text: string;
  generated_at?: string;
  source: string;
}

function parseAttentionItem(value: unknown): AttentionItem | undefined {
  if (!value || typeof value !== "object") {
    return undefined;
  }
  const item = value as Record<string, unknown>;
  if (
    typeof item.id !== "string" ||
    typeof item.kind !== "string" ||
    typeof item.title !== "string"
  ) {
    return undefined;
  }
  return {
    id: item.id,
    kind: item.kind,
    rank: typeof item.rank === "number" ? item.rank : 0,
    title: item.title,
    detail: typeof item.detail === "string" ? item.detail : "",
    action: typeof item.action === "string" && item.action ? item.action : undefined,
    plan_id: typeof item.plan_id === "string" ? item.plan_id : undefined,
    obligation_id: typeof item.obligation_id === "string" ? item.obligation_id : undefined,
    handoff_seq: typeof item.handoff_seq === "number" ? item.handoff_seq : undefined,
    delegation_id: typeof item.delegation_id === "string" ? item.delegation_id : undefined,
  };
}

/** Guard for the versioned contract: anything that is not exactly
 * stateroot.continuity.v1 is treated as absent, never as an error. */
export function parseContinuityProjection(value: unknown): ContinuityProjection | undefined {
  if (!value || typeof value !== "object") {
    return undefined;
  }
  const doc = value as Record<string, unknown>;
  if (doc.schema_version !== CONTINUITY_SCHEMA || !Array.isArray(doc.attention)) {
    return undefined;
  }
  const attention = doc.attention
    .map(parseAttentionItem)
    .filter((item): item is AttentionItem => !!item);
  return {
    ...(doc as unknown as ContinuityProjection),
    inputs_hash: typeof doc.inputs_hash === "string" ? doc.inputs_hash : "",
    attention,
  };
}

export function parseContinuityAdvisory(value: unknown): ContinuityAdvisory | undefined {
  if (!value || typeof value !== "object") {
    return undefined;
  }
  const doc = value as Record<string, unknown>;
  if (
    doc.schema_version !== CONTINUITY_ADVISORY_SCHEMA ||
    typeof doc.inputs_hash !== "string" ||
    typeof doc.text !== "string" ||
    typeof doc.source !== "string"
  ) {
    return undefined;
  }
  return doc as unknown as ContinuityAdvisory;
}

/** The advisory text, only when it labels the CURRENT projection inputs — a
 * stale advisory (hash mismatch) or a non-synthesis source is never shown. */
export function advisoryTextFor(
  projection: ContinuityProjection,
  advisory: ContinuityAdvisory | undefined
): string | undefined {
  if (!advisory || advisory.source !== "synthesis") {
    return undefined;
  }
  if (!projection.inputs_hash || advisory.inputs_hash !== projection.inputs_hash) {
    return undefined;
  }
  const text = advisory.text.trim();
  return text || undefined;
}
