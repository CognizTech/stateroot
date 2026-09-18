import type { LineageProjection, LineageRoot, ParallelFork } from "./parallelWork";

/**
 * Lane topology + per-row rail rendering for the lineage projection
 * (stateroot.lineage.v1). Pure functions — no vscode, no CLI — so the same
 * code runs in node:test and inside the workbench webview, where
 * workbenchHtml injects assignLanes/compactRails/renderRail by source.
 * Everything those three reference must be local, or one of esc/clip, which
 * intentionally mirror the client script's helpers and resolve to them when
 * injected. layoutLineage (the full-graph layout) is exported for tests and
 * is not injected — the Lineage tab renders compact rails, not the big graph.
 */

function esc(value: unknown): string {
  return String(value ?? "").replace(
    /[&<>"']/g,
    (c) =>
      ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[
        c
      ] as string)
  );
}

function clip(value: unknown, n: number): string {
  const text = String(value || "").trim();
  if (text.length <= n) return text;
  return text.slice(0, n - 1) + "…";
}

function harnessName(id: unknown): string {
  if (id === "kimi-code") return "kimi";
  if (id === "claude-code") return "claude";
  return String(id || "?");
}

export type LineageNodeKind = "trunk" | "fork" | "merge";

export interface LaneAssignment {
  laneOf: Map<string, number>;
  laneNames: string[];
  merges: Set<string>;
  chainOf: Map<string, string[]>;
  laneColor: (lane: number) => string;
}

/** Shared lane topology over roots in display order (index 0 = newest — the
 * caller picks the order, so compactRails can align to the list's array and
 * layoutLineage to its sorted window). Mainline roots are lane 0, merge roots
 * stay on lane 0, and each fork chain (first-parent walk from tip back to,
 * excluding, base_root) claims its own lane in order of fork activity — most
 * recent base_root first. Exported so workbenchHtml can inject it ahead of
 * compactRails; both views must agree on lanes, so both share this code. */
export function assignLanes(roots: LineageRoot[], forks: ParallelFork[]): LaneAssignment {
  const ACCENT = "#7ee0c8";
  const LANE_COLORS = ["#f2a65a", "#7aa2f7", "#bb9af7", "#f7768e", "#9ece6a", "#4fd6be", "#ff9e64"];
  const byId = new Map(roots.map((r) => [r.id, r]));
  const indexOf = new Map(roots.map((r, i) => [r.id, i]));
  const isMainline = (r: LineageRoot): boolean => !!r.mainline;
  const prevMainline = (row: number): string | undefined => {
    for (let j = row + 1; j < roots.length; j++) {
      if (isMainline(roots[j])) return roots[j].id;
    }
    return undefined;
  };
  const isMerge = (r: LineageRoot, row: number): boolean => {
    const parents = r.parents || [];
    if (parents.length > 2) return true;
    if (parents.length === 2) return parents[1] !== prevMainline(row);
    return false;
  };

  // Fork lanes in activity order: most recent base_root first.
  const orderedForks = [...forks];
  const activityOf = (f: ParallelFork): number => {
    const base = f.base_root ? indexOf.get(f.base_root) : undefined;
    if (base !== undefined) return base;
    const tip = f.tip ? indexOf.get(f.tip) : undefined;
    if (tip !== undefined) return tip;
    return Number.MAX_SAFE_INTEGER;
  };
  orderedForks.sort(
    (a, b) =>
      activityOf(a) - activityOf(b) ||
      (b.created_at || "").localeCompare(a.created_at || "") ||
      a.name.localeCompare(b.name)
  );

  const laneOf = new Map<string, number>();
  roots.forEach((r) => {
    if (isMainline(r)) laneOf.set(r.id, 0);
  });
  const claimed = new Set<string>();
  const chainOf = new Map<string, string[]>();
  const perFork = orderedForks.map((f) => {
    const ids: string[] = [];
    let cur = f.tip ? byId.get(f.tip) : undefined;
    let guard = 0;
    while (cur && guard++ <= roots.length) {
      if (cur.id === f.base_root) break;
      if (isMainline(cur)) break;
      if (claimed.has(cur.id)) break;
      claimed.add(cur.id);
      ids.push(cur.id);
      const next = cur.parents?.[0];
      if (!next) break;
      cur = byId.get(next);
    }
    chainOf.set(f.name, ids);
    return { name: f.name, ids };
  });
  const laneNames = ["trunk"];
  for (const claim of perFork) {
    if (!claim.ids.length) continue;
    laneNames.push(claim.name);
    const lane = laneNames.length - 1;
    claim.ids.forEach((id) => laneOf.set(id, lane));
  }
  // Merge roots always sit on the trunk lane.
  const merges = new Set<string>();
  roots.forEach((r, i) => {
    if (isMerge(r, i)) {
      merges.add(r.id);
      laneOf.set(r.id, 0);
    }
  });
  const laneColor = (lane: number): string => {
    if (lane === 0) return ACCENT;
    const name = laneNames[lane] || String(lane);
    let h = 0;
    for (const ch of name) {
      h = (h * 31 + ch.charCodeAt(0)) >>> 0;
    }
    return LANE_COLORS[h % LANE_COLORS.length];
  };
  return { laneOf, laneNames, merges, chainOf, laneColor };
}

export interface CompactRailLane {
  lane: number;
  x: number;
  color: string;
  /** True when the lane's vertical line continues into older rows. */
  activeThrough: boolean;
}

export interface CompactRailNode {
  lane: number;
  x: number;
  color: string;
  kind: LineageNodeKind;
  forkPoint: boolean;
  title: string;
}

export interface CompactRailDiagonal {
  fromLane: number;
  toLane: number;
  x1: number;
  x2: number;
  color: string;
  /** merge: node down-right into the fork lane below; branch: fork lane into the trunk node. */
  kind: "merge" | "branch";
}

export interface CompactRail {
  rootId: string;
  width: number;
  lanes: CompactRailLane[];
  node: CompactRailNode;
  diagonals: CompactRailDiagonal[];
  /** Truncated fork name on tip rows when the tip's lane is the row's rightmost. */
  label?: string;
}

/** Per-row, self-contained rail slices aligned to the projection's roots
 * array order — the same array the lineage list renders, so the rail cannot
 * drift from the list. A fork lane's vertical runs from its newest connection
 * row (the newest merge referencing its chain, else its tip row) down to the
 * branch-off row, which carries the off-diagonal instead of the vertical. */
export function compactRails(projection?: LineageProjection): CompactRail[] {
  const LANE_W = 10;
  const PAD = 4;
  const roots = projection?.roots ?? [];
  if (!roots.length) return [];
  const forks = projection?.forks ?? [];
  const { laneOf, laneNames, merges, chainOf, laneColor } = assignLanes(roots, forks);
  const rowOf = new Map(roots.map((r, i) => [r.id, i]));
  const xAt = (lane: number): number => PAD + lane * LANE_W;

  // Merge diagonals: one per merged parent on a non-trunk lane. The newest
  // merge referencing a chain becomes that lane's top connection row.
  const mergeDiags = new Map<number, CompactRailDiagonal[]>();
  const topConnection = new Map<number, number>();
  roots.forEach((r, row) => {
    if (!merges.has(r.id)) return;
    for (const parentId of (r.parents || []).slice(1)) {
      const lane = laneOf.get(parentId);
      if (!lane) continue;
      const list = mergeDiags.get(row) ?? [];
      list.push({
        fromLane: 0,
        toLane: lane,
        x1: xAt(0),
        x2: xAt(lane),
        color: laneColor(lane),
        kind: "merge",
      });
      mergeDiags.set(row, list);
      const prev = topConnection.get(lane);
      if (prev === undefined || row < prev) {
        topConnection.set(lane, row);
      }
    }
  });

  // Per-fork vertical spans and branch-off diagonals.
  const spans = new Map<number, { top: number; end: number }>();
  const branchDiags = new Map<number, CompactRailDiagonal[]>();
  const tipName = new Map<string, string>();
  for (const f of forks) {
    const chain = chainOf.get(f.name) ?? [];
    if (!chain.length) continue;
    const lane = laneNames.indexOf(f.name);
    const chainRows = chain
      .map((id) => rowOf.get(id))
      .filter((r): r is number => r !== undefined);
    if (!chainRows.length) continue;
    if (f.tip) tipName.set(f.tip, f.name);
    const tipRow = Math.min(...chainRows);
    const bottom = Math.max(...chainRows);
    const top = Math.min(tipRow, topConnection.get(lane) ?? tipRow);
    const baseRow = f.base_root ? rowOf.get(f.base_root) : undefined;
    const end = baseRow !== undefined ? Math.max(baseRow, bottom + 1) : bottom + 1;
    spans.set(lane, { top, end });
    if (baseRow !== undefined) {
      const list = branchDiags.get(baseRow) ?? [];
      list.push({
        fromLane: 0,
        toLane: lane,
        x1: xAt(0),
        x2: xAt(lane),
        color: laneColor(lane),
        kind: "branch",
      });
      branchDiags.set(baseRow, list);
    }
  }

  const maxLane = Math.max(0, ...spans.keys());
  const baseWidth = Math.max(14, xAt(maxLane) + 6);

  return roots.map((r, row) => {
    const lanes: CompactRailLane[] = [
      { lane: 0, x: xAt(0), color: laneColor(0), activeThrough: row < roots.length - 1 },
    ];
    const orderedSpans = [...spans.entries()].sort((a, b) => a[0] - b[0]);
    for (const [lane, span] of orderedSpans) {
      if (row < span.top || row >= span.end) continue;
      lanes.push({
        lane,
        x: xAt(lane),
        color: laneColor(lane),
        activeThrough: row + 1 < span.end,
      });
    }
    const nodeLane = laneOf.get(r.id) ?? 0;
    const isMergeRoot = merges.has(r.id);
    const fullName = tipName.get(r.id);
    const rightmost = lanes[lanes.length - 1]?.lane ?? 0;
    const label = fullName && nodeLane === rightmost ? clip(fullName, 8) : undefined;
    const node: CompactRailNode = {
      lane: nodeLane,
      x: xAt(nodeLane),
      color: laneColor(nodeLane),
      kind: isMergeRoot ? "merge" : nodeLane === 0 ? "trunk" : "fork",
      forkPoint: !!r.fork_point && !isMergeRoot,
      title: fullName ? `${r.id} · ${fullName}` : r.id,
    };
    const diagonals = [...(mergeDiags.get(row) ?? []), ...(branchDiags.get(row) ?? [])];
    const width = label ? Math.max(baseWidth, node.x + 8 + label.length * 6 + 4) : baseWidth;
    return { rootId: r.id, width, lanes, node, diagonals, label };
  });
}

/** One rail cell: full-height lane lines as flex divs plus a small
 * top-anchored SVG for the node marker and short diagonals — per-row
 * self-contained, so variable row heights and list scrolling never require
 * cross-row measurements. The node group carries data-act="showRoot". */
export function renderRail(rail?: CompactRail): string {
  if (!rail) return "";
  const CY = 9;
  const DIAG_BOTTOM = 26;
  const lines: string[] = [];
  let prevX = 0;
  let first = true;
  for (const lane of rail.lanes) {
    const delta = first ? lane.x : lane.x - prevX - 2;
    lines.push(`<div class="lane-line" style="margin-left:${delta}px;background:${lane.color}"></div>`);
    prevX = lane.x;
    first = false;
  }
  const diagonals = rail.diagonals
    .map((d) =>
      d.kind === "merge"
        ? `<path d="M ${d.x1 + 1} ${CY} C ${d.x1 + 1} ${CY + 11}, ${d.x2 + 1} ${CY + 7}, ${d.x2 + 1} ${DIAG_BOTTOM}" fill="none" stroke="${d.color}" stroke-width="1.5"/>`
        : `<path d="M ${d.x2 + 1} 0 C ${d.x2 + 1} 5, ${d.x1 + 1} 4, ${d.x1 + 1} ${CY}" fill="none" stroke="${d.color}" stroke-width="1.5"/>`
    )
    .join("");
  const n = rail.node;
  const cx = n.x + 1;
  const marker =
    n.kind === "merge"
      ? `<circle cx="${cx}" cy="${CY}" r="5" fill="none" stroke="${n.color}" stroke-width="1.5"/><circle cx="${cx}" cy="${CY}" r="2.2" fill="none" stroke="${n.color}" stroke-width="1.5"/>`
      : n.forkPoint
        ? `<circle cx="${cx}" cy="${CY}" r="3" fill="none" stroke="${n.color}" stroke-width="1.5"/>`
        : `<circle cx="${cx}" cy="${CY}" r="3.2" fill="${n.color}"/>`;
  const label = rail.label
    ? `<text class="rail-label" x="${n.x + 8}" y="${CY + 3}"><title>${esc(n.title)}</title>${esc(rail.label)}</text>`
    : "";
  return `<span class="rail" style="width:${rail.width}px">${lines.join("")}<svg width="${rail.width}" height="${DIAG_BOTTOM}">${diagonals}<g class="node ${n.kind}" data-act="showRoot" data-id="${esc(rail.rootId)}"><title>${esc(n.title)}</title>${marker}</g>${label}</svg></span>`;
}

export interface LineageGraphNode {
  id: string;
  lane: number;
  row: number;
  x: number;
  y: number;
  color: string;
  kind: LineageNodeKind;
  forkPoint: boolean;
  label: string;
  meta: string;
  title: string;
}

export interface LineageGraphEdge {
  from: string;
  to: string;
  fromLane: number;
  toLane: number;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  color: string;
  crossLane: boolean;
}

export interface LineageLaneHeader {
  lane: number;
  name: string;
  x: number;
  y: number;
}

export interface LineageGraphLayout {
  empty: boolean;
  nodes: LineageGraphNode[];
  edges: LineageGraphEdge[];
  headers: LineageLaneHeader[];
  laneNames: string[];
  totalRoots: number;
  shownRoots: number;
  width: number;
  height: number;
  labelX: number;
}

/** Evidence-only full-graph layout, kept as exported API (tests cover it;
 * the Lineage tab itself renders compactRails). Lanes come from the CLI
 * projection's mainline flags and fork chains via assignLanes; the extension
 * never infers topology from files. Newest first, no lane reuse.
 *
 * Window rule: structural nodes are never dropped. Structural = every fork
 * tip, every merge root (parents.length > 1), every fork_point root, and every
 * parent of an included node — recursing through non-mainline parents so fork
 * chains stay whole from tip to trunk base, stopping at mainline parents so
 * trunk history below stays filler. Remaining budget (of 60) is filled with
 * newest-first non-structural roots; if structure alone exceeds 60 it is all
 * shown. Fully transitive closure would drain the entire trunk ancestry of any
 * merge and make the cap dead code, so filler nodes never pull in ancestors —
 * edges to excluded parents are skipped. */
export function layoutLineage(projection?: LineageProjection): LineageGraphLayout {
  const MAX_ROOTS = 60;
  const PAD_X = 14;
  const LANE_W = 22;
  const ROW_H = 26;
  const HEADER_H = 20;
  const TOP = 12;
  const LABEL_GAP = 10;
  const LABEL_W = 560;

  const emptyLayout: LineageGraphLayout = {
    empty: true,
    nodes: [],
    edges: [],
    headers: [],
    laneNames: ["trunk"],
    totalRoots: projection?.roots?.length ?? 0,
    shownRoots: 0,
    width: 0,
    height: 0,
    labelX: 0,
  };

  const all = [...(projection?.roots ?? [])];
  all.sort(
    (a, b) => (b.created_at || "").localeCompare(a.created_at || "") || b.id.localeCompare(a.id)
  );
  if (!all.length) {
    return emptyLayout;
  }
  const byIdAll = new Map(all.map((r) => [r.id, r]));
  const isMainline = (r: LineageRoot): boolean => !!r.mainline;

  // Structural seeds, regardless of where they fall in newest-first order.
  const included = new Set<string>();
  const pending: LineageRoot[] = [];
  const seed = (r: LineageRoot | undefined): void => {
    if (!r || included.has(r.id)) return;
    included.add(r.id);
    pending.push(r);
  };
  for (const f of projection?.forks ?? []) {
    seed(f.tip ? byIdAll.get(f.tip) : undefined);
  }
  for (const r of all) {
    if ((r.parents || []).length > 1 || r.fork_point) {
      seed(r);
    }
  }
  // Parent-edge closure: parents of included nodes join the window; the walk
  // continues through non-mainline parents (fork chains) and stops at
  // mainline parents (trunk ancestors stay subject to the cap).
  while (pending.length) {
    const r = pending.pop() as LineageRoot;
    for (const parentId of r.parents || []) {
      const parent = byIdAll.get(parentId);
      if (!parent || included.has(parent.id)) continue;
      included.add(parent.id);
      if (!isMainline(parent)) pending.push(parent);
    }
  }
  // Fill the remaining budget with newest-first non-structural roots.
  const budget = Math.max(0, MAX_ROOTS - included.size);
  let filled = 0;
  for (const r of all) {
    if (filled >= budget) break;
    if (!included.has(r.id)) {
      included.add(r.id);
      filled++;
    }
  }
  const roots = all.filter((r) => included.has(r.id));

  const indexOf = new Map(roots.map((r, i) => [r.id, i]));
  const { laneOf, laneNames, merges, laneColor } = assignLanes(roots, projection?.forks ?? []);

  const placed = roots.map((r, row) => {
    const merge = merges.has(r.id);
    const lane = laneOf.get(r.id) ?? 0;
    return { r, row, lane, merge };
  });
  const maxLane = Math.max(0, ...placed.map((p) => p.lane));
  const topPad = maxLane > 0 ? HEADER_H + 8 : TOP;
  const labelX = PAD_X + (maxLane + 1) * LANE_W + LABEL_GAP;
  const xAt = (lane: number): number => PAD_X + lane * LANE_W;
  const yAt = (row: number): number => topPad + row * ROW_H;

  const nodes: LineageGraphNode[] = placed.map(({ r, row, lane, merge }) => {
    const titleLines = [
      r.id,
      [r.created_at, harnessName(r.created_by_harness)].filter(Boolean).join(" · "),
      (r.created_reason || "").trim(),
      typeof r.files_pinned === "number" ? `files: ${r.files_pinned}` : "",
      r.coverage ? `coverage: ${r.coverage}` : "",
    ].filter(Boolean);
    return {
      id: r.id,
      lane,
      row,
      x: xAt(lane),
      y: yAt(row),
      color: laneColor(lane),
      kind: merge ? "merge" : lane === 0 ? "trunk" : "fork",
      forkPoint: !!r.fork_point && !merge,
      label: clip(r.created_reason || "", 64),
      meta: ` · ${harnessName(r.created_by_harness)} · ${r.id.slice(0, 8)}`,
      title: titleLines.join("\n"),
    };
  });
  const nodeById = new Map(nodes.map((n) => [n.id, n]));

  const edges: LineageGraphEdge[] = [];
  for (const { r } of placed) {
    const child = nodeById.get(r.id);
    if (!child) continue;
    for (const parentId of r.parents || []) {
      const parent = nodeById.get(parentId);
      if (!parent) continue; // parent fell outside the render window
      const crossLane = parent.lane !== child.lane;
      const color = crossLane
        ? laneColor(child.lane !== 0 ? child.lane : parent.lane)
        : laneColor(child.lane);
      edges.push({
        from: child.id,
        to: parent.id,
        fromLane: child.lane,
        toLane: parent.lane,
        x1: child.x,
        y1: child.y,
        x2: parent.x,
        y2: parent.y,
        color,
        crossLane,
      });
    }
  }

  const headers: LineageLaneHeader[] = [];
  for (let lane = 1; lane < laneNames.length; lane++) {
    const topmost = nodes.filter((n) => n.lane === lane).sort((a, b) => a.row - b.row)[0];
    if (!topmost) continue;
    headers.push({ lane, name: laneNames[lane], x: xAt(lane), y: topmost.y - 14 });
  }

  return {
    empty: false,
    nodes,
    edges,
    headers,
    laneNames,
    totalRoots: all.length,
    shownRoots: roots.length,
    width: labelX + LABEL_W,
    height: yAt(roots.length - 1) + 14,
    labelX,
  };
}
