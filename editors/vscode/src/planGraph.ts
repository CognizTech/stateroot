/** The existing CLI owns readiness decisions; the extension only displays them. */
export interface PlanGraphRow {
  id: string;
  title: string;
  status: string;
  readiness: string;
  workerState: string;
  targetFork?: string;
  dependenciesDeclared?: boolean;
  dependencies?: Array<{ planId: string; bodyDigest: string; kind: string; state: string; reason: string; evidenceBasis: string }>;
  assignment?: { harness: string; fork?: string; automaticDispatch: "declared" | "not_declared" | "unknown" } | null;
  blockers: Array<{ code: string; detail: string }>;
}

export interface PlanGraphView {
  observationId: string;
  rows: PlanGraphRow[];
  unavailable?: string;
}

export function parsePlanGraph(text: string): PlanGraphView {
  const doc: unknown = JSON.parse(text);
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) throw new Error("plan graph is not an object");
  const value = doc as Record<string, unknown>;
  if (value.schema_version !== "stateroot.plan-graph.v1" || !Array.isArray(value.nodes)) {
    throw new Error("unsupported plan graph schema");
  }
  return {
    observationId: typeof value.observation_id === "string" ? value.observation_id : "",
    rows: value.nodes.map((node: unknown) => {
      if (!node || typeof node !== "object" || Array.isArray(node)) throw new Error("invalid plan graph node");
      const row = node as Record<string, unknown>;
      if (typeof row.id !== "string") throw new Error("plan graph node has no ID");
      const dependencies = Array.isArray(row.dependencies) ? row.dependencies.map((entry: unknown) => {
        if (!entry || typeof entry !== "object" || Array.isArray(entry)) throw new Error("invalid plan dependency");
        const item = entry as Record<string, unknown>;
        if (!item.dependency || typeof item.dependency !== "object" || Array.isArray(item.dependency)) throw new Error("invalid plan dependency identity");
        const dependency = item.dependency as Record<string, unknown>;
        if (typeof dependency.plan_id !== "string" || typeof dependency.body_digest !== "string") throw new Error("invalid plan dependency identity");
        return {
          planId: dependency.plan_id,
          bodyDigest: dependency.body_digest,
          kind: typeof dependency.kind === "string" ? dependency.kind : "unknown",
          state: typeof item.state === "string" ? item.state : "unknown",
          reason: typeof item.reason === "string" ? item.reason : "",
          evidenceBasis: typeof item.evidence_basis === "string" ? item.evidence_basis : "unknown",
        };
      }) : undefined;
      let assignment: PlanGraphRow["assignment"];
      if (row.assignment === null) assignment = null;
      else if (row.assignment && typeof row.assignment === "object" && !Array.isArray(row.assignment)) {
        const value = row.assignment as Record<string, unknown>;
        if (typeof value.harness === "string") assignment = {
          harness: value.harness,
          fork: typeof value.fork === "string" ? value.fork : undefined,
          automaticDispatch: value.dispatch === undefined || value.dispatch === null ? "not_declared"
            : typeof value.dispatch === "object" && !Array.isArray(value.dispatch) ? "declared" : "unknown",
        };
      }
      return {
        id: row.id,
        title: typeof row.title === "string" ? row.title : row.id,
        status: typeof row.status === "string" ? row.status : "unknown",
        readiness: typeof row.readiness === "string" ? row.readiness : "unknown",
        workerState: typeof row.worker_state === "string" ? row.worker_state : "unknown",
        targetFork: typeof row.target_fork === "string" ? row.target_fork : undefined,
        dependenciesDeclared: typeof row.dependencies_declared === "boolean" ? row.dependencies_declared : undefined,
        dependencies,
        assignment,
        blockers: Array.isArray(row.blockers) ? row.blockers.map((blocker: unknown) => {
          if (!blocker || typeof blocker !== "object" || Array.isArray(blocker)) return { code: "unknown", detail: "Unreadable blocker" };
          const value = blocker as Record<string, unknown>;
          return { code: typeof value.code === "string" ? value.code : "unknown", detail: typeof value.detail === "string" ? value.detail : "" };
        }) : [],
      };
    }),
  };
}
