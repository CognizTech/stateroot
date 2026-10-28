/** Thin workbench projection of the CLI's existing inventory and health readers. */
export interface InventorySource {
  state: "ready" | "unavailable";
  detail?: string;
}

export interface SkillRow {
  identity: string;
  slug: string;
  origin: string;
  scope: string;
  lifecycle: string;
  digest: string;
  path: string;
  compatible?: boolean;
  compatibilityReasons: string[];
  nativeRoute?: string;
}

export interface RuleRow {
  slug: string;
  title: string;
  origin: string;
  scope: string;
  digest: string;
  path: string;
}

export interface McpRow {
  name: string;
  scope: string;
  digest: string;
  transport: string;
  origins: string[];
}

export interface LocalToolRow {
  name: string;
  description: string;
}

export interface McpActionRow {
  action: string;
  name: string;
  detail: string;
}

export interface HarnessRow {
  harness: string;
  display: string;
  status: string;
  config: string;
  skills: string;
  mcp: string;
  hooks: string;
  instructions: string;
  problems: string[];
  repair: string[];
  lastCapture?: string;
  lastDelivery?: string;
}

export interface IntelligenceInventory {
  sources: Record<"skills" | "rules" | "mcp" | "mcpHealth" | "tools" | "health", InventorySource>;
  skills: SkillRow[];
  rules: RuleRow[];
  mcp: McpRow[];
  mcpIssues: McpActionRow[];
  mcpActions: McpActionRow[];
  tools: LocalToolRow[];
  harnesses: HarnessRow[];
}

const object = (value: unknown): Record<string, unknown> | undefined =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
const string = (value: unknown): string => typeof value === "string" ? value : "";
const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];

export function emptyInventory(): IntelligenceInventory {
  const source = (): InventorySource => ({ state: "unavailable", detail: "Not inspected yet" });
  return {
    sources: { skills: source(), rules: source(), mcp: source(), mcpHealth: source(), tools: source(), health: source() },
    skills: [], rules: [], mcp: [], mcpIssues: [], mcpActions: [], tools: [], harnesses: [],
  };
}

/** Parse the exact CLI wire shapes. A malformed producer stays unavailable. */
export function parseInventory(kind: keyof IntelligenceInventory["sources"], text: string):
  Pick<IntelligenceInventory, "skills" | "rules" | "mcp" | "tools" | "harnesses"> {
  const raw: unknown = JSON.parse(text);
  if (kind === "skills") {
    if (!Array.isArray(raw)) throw new Error("skill scan returned no array");
    return { skills: raw.map((value) => {
      const row = object(value);
      if (!row || !string(row.identity_key) || !string(row.slug)) throw new Error("invalid Skill identity");
      const compatibility = object(row.compatibility);
      return {
        identity: string(row.identity_key), slug: string(row.slug), origin: string(row.harness),
        scope: string(row.scope), lifecycle: string(row.lifecycle), digest: string(row.package_digest),
        path: string(row.source_path), compatible: typeof compatibility?.compatible === "boolean" ? compatibility.compatible : undefined,
        compatibilityReasons: strings(compatibility?.reasons), nativeRoute: string(row.native_invocation) || undefined,
      };
    }), rules: [], mcp: [], tools: [], harnesses: [] };
  }
  if (kind === "rules") {
    if (!Array.isArray(raw)) throw new Error("rules list returned no array");
    return { skills: [], rules: raw.map((value) => {
      const row = object(value);
      if (!row || !string(row.slug)) throw new Error("invalid rule identity");
      return {
        slug: string(row.slug), title: string(row.title), origin: string(row.origin),
        scope: string(row.scope), digest: string(row.sha256), path: string(row.origin_path),
      };
    }), mcp: [], tools: [], harnesses: [] };
  }
  if (kind === "tools") {
    if (!Array.isArray(raw)) throw new Error("local MCP tools returned no array");
    return { skills: [], rules: [], mcp: [], tools: raw.map((value) => {
      const row = object(value);
      if (!row || !string(row.name)) throw new Error("invalid local MCP tool");
      return { name: string(row.name), description: string(row.description) };
    }), harnesses: [] };
  }
  if (kind === "mcp") {
    const servers = object(object(raw)?.servers);
    if (!servers || !Array.isArray(servers.global) || !Array.isArray(servers.project)) {
      throw new Error("MCP status returned no scoped servers");
    }
    const rows: McpRow[] = [];
    for (const scope of ["global", "project"] as const) {
      for (const value of servers[scope] as unknown[]) {
        const row = object(value);
        if (!row || !string(row.name)) throw new Error("invalid MCP server identity");
        rows.push({
          name: string(row.name), scope, digest: string(row.entry_digest),
          transport: string(row.transport_hint),
          origins: Array.isArray(row.origins) ? row.origins.map((origin) => {
            const item = object(origin);
            return item ? [string(item.harness), string(item.path)].filter(Boolean).join(" · ") : "";
          }).filter(Boolean) : [],
        });
      }
    }
    return { skills: [], rules: [], mcp: rows, tools: [], harnesses: [] };
  }
  const doc = object(raw);
  const health = object(doc?.integrations);
  if (!doc || doc.schema_version !== "stateroot.doctor.v1" || !health || !Array.isArray(health.harnesses)) {
    throw new Error("doctor returned no integration health");
  }
  return { skills: [], rules: [], mcp: [], tools: [], harnesses: health.harnesses.map((value) => {
    const row = object(value);
    if (!row || !string(row.harness)) throw new Error("invalid harness identity");
    const state = (field: string): string => string(object(row[field])?.state) || "unknown";
    return {
      harness: string(row.harness), display: string(row.display) || string(row.harness),
      status: string(row.status) || "unknown", config: state("config"), skills: state("skills"),
      mcp: state("mcp"), hooks: state("hooks"), instructions: state("instructions"),
      problems: strings(row.problems), repair: strings(row.repair),
      lastCapture: string(row.last_capture) || undefined, lastDelivery: string(row.last_delivery) || undefined,
    };
  }) };
}

export function parseMcpDiagnosis(text: string): Pick<IntelligenceInventory, "mcpIssues" | "mcpActions"> {
  const doc = object(JSON.parse(text));
  if (!doc || !Array.isArray(doc.issues) || !Array.isArray(doc.actions_preview)) {
    throw new Error("MCP doctor returned no typed diagnosis");
  }
  const rows = (items: unknown[]): McpActionRow[] => items.map((value) => {
    const row = object(value);
    if (!row || !string(row.action) || !string(row.name)) throw new Error("invalid MCP action");
    return { action: string(row.action), name: string(row.name), detail: string(row.detail) };
  });
  return { mcpIssues: rows(doc.issues), mcpActions: rows(doc.actions_preview) };
}
