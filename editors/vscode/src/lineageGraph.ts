import type { LineageProjection, LineageRoot, ParallelFork } from "./parallelWork";

/**
 * Git-style DAG layout + SVG rendering for the lineage projection
 * (stateroot.lineage.v1). Pure functions — no vscode, no CLI — so the same
 * code runs in node:test and inside the workbench webview, where
 * workbenchHtml injects these functions by source. Everything they reference
 * must therefore be local, or one of esc/clip/harnessName, which intentionally
 * mirror the client script's helpers and resolve to them when injected.
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

/** Evidence-only layout: lanes come from the CLI projection's mainline flags
 * and fork chains (first-parent walk from fork tip to base_root); the
 * extension never infers topology from files. Newest first, no lane reuse.
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
  const ACCENT = "#7ee0c8";
  const LANE_COLORS = ["#f2a65a", "#7aa2f7", "#bb9af7", "#f7768e", "#9ece6a", "#4fd6be", "#ff9e64"];

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

  const byId = new Map(roots.map((r) => [r.id, r]));
  const indexOf = new Map(roots.map((r, i) => [r.id, i]));
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
  const forks = [...(projection?.forks ?? [])];
  const activityOf = (f: ParallelFork): number => {
    const base = f.base_root ? indexOf.get(f.base_root) : undefined;
    if (base !== undefined) return base;
    const tip = f.tip ? indexOf.get(f.tip) : undefined;
    if (tip !== undefined) return tip;
    return Number.MAX_SAFE_INTEGER;
  };
  forks.sort(
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
  const perFork = forks.map((f) => {
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
  roots.forEach((r, i) => {
    if (isMerge(r, i)) laneOf.set(r.id, 0);
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

  const placed = roots.map((r, row) => {
    const merge = isMerge(r, row);
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

/** SVG string for the Lineage tab. Nodes carry data-act="showRoot" so the
 * panel's existing click delegation opens `stateroot show <id>` output. */
export function renderLineageGraph(projection?: LineageProjection): string {
  const layout = layoutLineage(projection);
  if (layout.empty) {
    return '<div class="muted">No lineage yet — <code>stateroot snap</code> records the first root.</div>';
  }
  const edgeMarkup = layout.edges
    .map((e) => {
      if (!e.crossLane) {
        return `<path d="M ${e.x1} ${e.y1} L ${e.x2} ${e.y2}" fill="none" stroke="${e.color}" stroke-width="1.6"/>`;
      }
      const bend = Math.min(40, Math.max(12, Math.abs(e.y2 - e.y1) / 2));
      return `<path d="M ${e.x1} ${e.y1} C ${e.x1} ${e.y1 + bend}, ${e.x2} ${e.y2 - bend}, ${e.x2} ${e.y2}" fill="none" stroke="${e.color}" stroke-width="1.6"/>`;
    })
    .join("");
  const headerMarkup = layout.headers
    .map(
      (h) =>
        `<text class="lane-head" x="${h.x}" y="${h.y}" text-anchor="middle">${esc(clip(h.name, 8))}<title>${esc(h.name)}</title></text>`
    )
    .join("");
  const nodeMarkup = layout.nodes
    .map((n) => {
      const marker =
        n.kind === "merge"
          ? `<circle cx="${n.x}" cy="${n.y}" r="6.5" fill="none" stroke="${n.color}" stroke-width="1.8"/><circle cx="${n.x}" cy="${n.y}" r="3" fill="none" stroke="${n.color}" stroke-width="1.8"/>`
          : n.forkPoint
            ? `<circle cx="${n.x}" cy="${n.y}" r="4" fill="none" stroke="${n.color}" stroke-width="1.6"/>`
            : `<circle cx="${n.x}" cy="${n.y}" r="5" fill="${n.color}"/>`;
      return `<g class="node ${n.kind}" data-act="showRoot" data-id="${esc(n.id)}"><title>${esc(n.title)}</title>${marker}<text x="${layout.labelX}" y="${n.y + 4}">${esc(n.label)}<tspan class="muted">${esc(n.meta)}</tspan></text></g>`;
    })
    .join("");
  const footer =
    layout.totalRoots > layout.shownRoots
      ? `<div class="muted">showing ${layout.shownRoots} of ${layout.totalRoots} roots · ${layout.totalRoots - layout.shownRoots} older roots hidden</div>`
      : `<div class="muted">all roots shown</div>`;
  return `<div class="lineage-graph"><svg width="${layout.width}" height="${layout.height}" viewBox="0 0 ${layout.width} ${layout.height}" role="img">${edgeMarkup}${headerMarkup}${nodeMarkup}</svg>${footer}</div>`;
}
