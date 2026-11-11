import * as vscode from "vscode";
import { cliPath, runCli } from "./cli";
import { liveStatus, type DelegationRecord } from "./store";

export interface LineageRoot {
  id: string;
  ref?: string;
  parents: string[];
  created_at?: string;
  created_by_harness?: string;
  created_reason?: string;
  coverage?: string;
  files_pinned?: number;
  mainline: boolean;
  fork_point: boolean;
}

export interface ParallelFork {
  name: string;
  ref: string;
  tip?: string | null;
  base_root?: string | null;
  plan?: string | null;
  created_at?: string | null;
  created_by_harness?: string | null;
  contained: boolean;
  worktree?: { registered: boolean; path: string } | null;
  cleanup?: { state: string; pending?: unknown };
}

export interface LineageProjection {
  schema_version: "stateroot.lineage.v1";
  trunk: { ref: string; tip?: string | null };
  forks: ParallelFork[];
  roots: LineageRoot[];
  topology?: LineageRoot[];
  pagination?: {
    schema_version: "stateroot.lineage.pagination.v1";
    offset: number;
    limit: number;
    total: number;
    has_more: boolean;
    history: string;
  };
}

export type ParallelWorkPhase =
  | "provisioning"
  | "running"
  | "cancelling"
  | "capturing"
  | "unknown"
  | "attention"
  | "ready"
  | "merged"
  | "cleanup_pending";

export interface ParallelWorkCard {
  fork: ParallelFork;
  attempts: DelegationRecord[];
  phase: ParallelWorkPhase;
  outcomeRoot?: string;
  /** Exact retry command for a fork whose cleanup did not finish. */
  cleanupCommand?: string;
}

/** Evidence-only phase derivation. A terminal worker is never merge-ready
 * until its captured outcome root is present. */
export function deriveParallelWork(
  lineage: LineageProjection | undefined,
  delegations: DelegationRecord[]
): ParallelWorkCard[] {
  if (!lineage) return [];
  return lineage.forks.map((fork) => {
    const attempts = delegations.filter((row) => row.fork_id === fork.name);
    const latest = attempts[0];
    const status = latest ? liveStatus(latest) : undefined;
    const outcomeRoot = attempts.find((row) => row.outcome_root)?.outcome_root;
    const captureError = attempts.some((row) => row.events?.some((event) => event.event === "capture-error"));
    const phase: ParallelWorkPhase = fork.cleanup?.state === "pending"
      ? "cleanup_pending"
      : fork.contained
        ? "merged"
        : status === "cancelling"
          ? "cancelling"
          : status === "running"
            ? "running"
            // Explicitly unverified current work must not inherit readiness
            // from an older capture. Legacy root-only rows remain usable.
            : status === "unknown" && latest?.status !== undefined
              ? "unknown"
              : captureError || (["failed", "lost", "timed_out"].includes(status || "") && !outcomeRoot)
                ? "attention"
                : outcomeRoot
                  ? "ready"
                  : latest?.outcome
                    ? "capturing"
                    : "provisioning";
    const cleanupCommand =
      phase === "cleanup_pending" ? `stateroot merge --cleanup ${fork.name}` : undefined;
    return { fork, attempts, phase, outcomeRoot, cleanupCommand };
  });
}

const legacyReaders = new Map<string, number>();
export const LINEAGE_PAGE_SIZE = 50;

/** Read CLI-owned topology. Only a proven unsupported flag triggers legacy fallback. */
export async function readParallelWork(
  root: string,
  output: vscode.OutputChannel,
  offset = 0
): Promise<LineageProjection | undefined> {
  try {
    const binary = cliPath();
    let text: string;
    if ((legacyReaders.get(binary) || 0) > Date.now()) {
      text = await runCli(["log", "--json"], root, 60_000, binary);
    } else {
      try {
        text = await runCli(["log", "--json", "--limit", String(LINEAGE_PAGE_SIZE), "--offset", String(offset)], root, 60_000, binary);
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        if (!/unexpected argument ['‘’"]?--limit/.test(message)) throw err;
        // Re-probe after an in-place CLI update without repeatedly failing old CLIs.
        legacyReaders.set(binary, Date.now() + 60_000);
        text = await runCli(["log", "--json"], root, 60_000, binary);
      }
    }
    const value = JSON.parse(text) as LineageProjection;
    if (value.schema_version !== "stateroot.lineage.v1" || !Array.isArray(value.roots)) {
      throw new Error("unsupported lineage projection");
    }
    const page = value.pagination;
    if (page && (page.schema_version !== "stateroot.lineage.pagination.v1" ||
        !Number.isSafeInteger(page.total) || page.total < 0 ||
        page.offset !== offset || page.limit !== LINEAGE_PAGE_SIZE ||
        !Array.isArray(value.topology) || value.topology.length !== page.total ||
        value.roots.length !== Math.min(page.limit, Math.max(0, page.total - offset)) ||
        page.has_more !== (offset + page.limit < page.total) || typeof page.history !== "string")) {
      throw new Error("invalid lineage pagination");
    }
    return value;
  } catch (err: unknown) {
    const message = err instanceof Error ? err.message : String(err);
    output.appendLine(`Parallel lineage unavailable: ${message}`);
    return undefined;
  }
}
