/**
 * Merge-attempt contract shared with the CLI (stateroot.merge-attempt.v1).
 * The extension parses and displays these payloads as evidence; it never
 * executes, continues, or aborts a merge itself.
 */

export const MERGE_ATTEMPT_SCHEMA = "stateroot.merge-attempt.v1";

export interface MergeAttemptConflict {
  fork: string;
  path: string;
  kind?: string;
  ancestor?: string;
  ours?: string;
  theirs?: string;
}

export interface MergeAttemptFork {
  name: string;
  tip: string;
}

export interface MergeAttempt {
  schema_version: typeof MERGE_ATTEMPT_SCHEMA;
  id: string;
  created_at?: string;
  harness?: string;
  trunk_tip?: string;
  forks: MergeAttemptFork[];
  state: "ready" | "attention";
  conflicts: MergeAttemptConflict[];
  folded_forks?: MergeAttemptFork[];
  pending_forks?: MergeAttemptFork[];
  /** Absolute machine-local reconciliation worktree, present only for attention. */
  worktree?: string;
}

/** Single sentinel so a stale CLI is recognizable from the thrown message. */
export const UNSUPPORTED_ATTEMPT_ERROR = "unsupported merge-attempt payload";

/** Guard for the versioned contract — same pattern as the lineage projection:
 * anything that is not exactly stateroot.merge-attempt.v1 is a stale CLI. */
export function parseMergeAttempt(text: string): MergeAttempt {
  let value: MergeAttempt | undefined;
  try {
    value = JSON.parse(text) as MergeAttempt;
  } catch {
    throw new Error(UNSUPPORTED_ATTEMPT_ERROR);
  }
  if (
    !value ||
    value.schema_version !== MERGE_ATTEMPT_SCHEMA ||
    typeof value.id !== "string" ||
    (value.state !== "ready" && value.state !== "attention") ||
    !Array.isArray(value.forks) ||
    !Array.isArray(value.conflicts)
  ) {
    throw new Error(UNSUPPORTED_ATTEMPT_ERROR);
  }
  return value;
}

/** True when the installed CLI predates the integration contracts: either it
 * rejects the prepare invocation (unknown argument/subcommand) or its payload
 * does not match stateroot.merge-attempt.v1. */
export function isStaleIntegrationCli(err: unknown): boolean {
  const message = err instanceof Error ? err.message : String(err);
  if (message.includes(UNSUPPORTED_ATTEMPT_ERROR)) {
    return true;
  }
  return /unexpected argument|unrecognized (subcommand|option|argument)|unknown (option|flag|argument|subcommand)|which wasn't expected|was not expected/i.test(
    message
  );
}
