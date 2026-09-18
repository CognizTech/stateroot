const assert = require("node:assert/strict");
const test = require("node:test");
const vm = require("node:vm");

// out/lineageGraph.js is pure (type-only imports), so it loads directly.
const {
  assignLanes,
  compactRails,
  layoutLineage,
  renderRail,
} = require("../out/lineageGraph");
const { workbenchHtml } = require("../out/ui");

const ACCENT = "#7ee0c8";

function root(id, extra) {
  return Object.assign(
    {
      id,
      parents: [],
      created_at: "2026-09-18T00:00:00Z",
      created_by_harness: "kimi",
      created_reason: "root " + id,
      mainline: false,
      fork_point: false,
    },
    extra
  );
}

function projection(roots, forks) {
  return {
    schema_version: "stateroot.lineage.v1",
    trunk: { ref: "refs/stateroot/trunk", tip: roots[0] && roots[0].id },
    forks: forks || [],
    roots,
  };
}

function fork(name, tip, baseRoot, extra) {
  return Object.assign(
    { name, ref: "refs/stateroot/forks/" + name, tip, base_root: baseRoot, contained: false },
    extra
  );
}

function node(layout, id) {
  const found = layout.nodes.find((n) => n.id === id);
  assert.ok(found, "node " + id + " present");
  return found;
}

function rail(rails, id) {
  const found = rails.find((r) => r.rootId === id);
  assert.ok(found, "rail for " + id + " present");
  return found;
}

function laneNumbers(railEntry) {
  return railEntry.lanes.map((l) => l.lane);
}

/** The owner's repo shape, compact: trunk + 3 forks (one 2-deep chain) +
 * a 4-parent merge near the top. Array order is newest-first, as the CLI
 * emits it; created_at values are deliberately scrambled to prove the rail
 * follows the array, not a re-sort. */
function mirrorFixture() {
  const roots = [
    root("t4", { mainline: true, parents: ["m1"], created_at: "2026-09-18T12:00:00Z" }),
    root("m1", {
      mainline: true,
      parents: ["t3", "fa1", "fb", "fc"],
      created_at: "2026-09-17T01:00:00Z",
      created_reason: "merge three forks",
    }),
    root("t3", { mainline: true, parents: ["t2"], created_at: "2026-09-18T10:30:00Z" }),
    root("fa1", { parents: ["fa0"], created_at: "2026-09-16T10:00:00Z" }),
    root("fb", { parents: ["t1"], created_at: "2026-09-18T09:45:00Z" }),
    root("fc", { parents: ["t2"], created_at: "2026-09-15T09:30:00Z" }),
    root("fa0", { parents: ["t0"], created_at: "2026-09-18T09:30:00Z" }),
    root("t2", { mainline: true, fork_point: true, parents: ["t1"], created_at: "2026-09-18T09:00:00Z" }),
    root("t1", { mainline: true, parents: ["t0"], created_at: "2026-09-14T08:00:00Z" }),
    root("t0", { mainline: true, created_at: "2026-09-18T07:00:00Z" }),
  ];
  const forks = [
    fork("api-redesign", "fa1", "t0", { contained: true }),
    fork("ui-polish", "fb", "t1", { contained: true }),
    fork("feature-c-long", "fc", "t2", { contained: true }),
  ];
  return projection(roots, forks);
}

// --- layoutLineage: regression net over the shared lane topology ---

test("layoutLineage rows are newest-first by created_at with id tiebreak descending", () => {
  const layout = layoutLineage(
    projection([
      root("t2", { mainline: true, parents: ["t1"], created_at: "2026-09-18T11:00:00Z" }),
      root("r-aaa", { mainline: true, created_at: "2026-09-18T09:00:00Z" }),
      root("t3", { mainline: true, parents: ["t2"], created_at: "2026-09-18T12:00:00Z" }),
      root("r-bbb", { mainline: true, created_at: "2026-09-18T09:00:00Z" }),
      root("t1", { mainline: true, created_at: "2026-09-18T10:00:00Z" }),
    ])
  );
  assert.deepEqual(
    layout.nodes.map((n) => n.id),
    ["t3", "t2", "t1", "r-bbb", "r-aaa"]
  );
});

test("layoutLineage keeps trunk on lane 0 and fork chains on their own lanes in base_root recency order", () => {
  const layout = layoutLineage(
    projection(
      [
        root("t2", { mainline: true, parents: ["t1"], created_at: "2026-09-18T10:00:00Z" }),
        root("b1", { parents: ["t1"], created_at: "2026-09-18T09:50:00Z" }),
        root("a2", { parents: ["a1"], created_at: "2026-09-18T09:45:00Z" }),
        root("a1", { parents: ["t0"], created_at: "2026-09-18T09:30:00Z" }),
        root("t1", { mainline: true, parents: ["t0"], created_at: "2026-09-18T09:00:00Z" }),
        root("t0", { mainline: true, created_at: "2026-09-18T08:00:00Z" }),
      ],
      [fork("f1", "a2", "t0"), fork("f2", "b1", "t1")]
    )
  );
  assert.equal(node(layout, "t2").lane, 0);
  assert.equal(node(layout, "b1").lane, 1);
  assert.equal(node(layout, "a2").lane, 2);
  assert.equal(node(layout, "a1").lane, 2);
  assert.deepEqual(layout.laneNames, ["trunk", "f2", "f1"]);
  assert.notEqual(node(layout, "b1").color, ACCENT, "fork lane has its own color");
});

test("layoutLineage: a 2-fork merge root arcs once per fork tip and stays on lane 0", () => {
  const proj = projection(
    [
      root("m1", { mainline: true, parents: ["t1", "a1", "b1"], created_at: "2026-09-18T10:00:00Z" }),
      root("b1", { parents: ["t0"], created_at: "2026-09-18T09:40:00Z" }),
      root("a1", { parents: ["t0"], created_at: "2026-09-18T09:30:00Z" }),
      root("t1", { mainline: true, parents: ["t0"], created_at: "2026-09-18T09:00:00Z" }),
      root("t0", { mainline: true, created_at: "2026-09-18T08:00:00Z" }),
    ],
    [fork("f1", "a1", "t0"), fork("f2", "b1", "t0")]
  );
  const layout = layoutLineage(proj);
  const merge = node(layout, "m1");
  assert.equal(merge.kind, "merge");
  assert.equal(merge.lane, 0);
  const arcs = layout.edges.filter((e) => e.from === "m1" && e.crossLane);
  assert.equal(arcs.length, 2, "one arc per fork tip");
  assert.deepEqual(
    arcs.map((e) => e.to).sort(),
    ["a1", "b1"]
  );
  const trunkEdge = layout.edges.find((e) => e.from === "m1" && e.to === "t1");
  assert.equal(trunkEdge.crossLane, false);
});

test("layoutLineage: a contained fork keeps its lane and the arc into the merge", () => {
  const layout = layoutLineage(
    projection(
      [
        root("m2", { mainline: true, parents: ["t1", "a1"], created_at: "2026-09-18T10:00:00Z" }),
        root("a1", { parents: ["t0"], created_at: "2026-09-18T09:30:00Z" }),
        root("t1", { mainline: true, parents: ["t0"], created_at: "2026-09-18T09:00:00Z" }),
        root("t0", { mainline: true, created_at: "2026-09-18T08:00:00Z" }),
      ],
      [fork("f1", "a1", "t0", { contained: true })]
    )
  );
  assert.equal(node(layout, "m2").kind, "merge");
  const tip = node(layout, "a1");
  assert.equal(tip.lane, 1);
  assert.equal(tip.kind, "fork");
  const arc = layout.edges.find((e) => e.from === "m2" && e.to === "a1");
  assert.ok(arc && arc.crossLane);
});

// --- compactRails / renderRail: the per-row rail inside the lineage list ---

test("trunk-only project: every row carries one accent lane and a trunk node, no diagonals", () => {
  const proj = projection([
    root("c3", { mainline: true, parents: ["c2"] }),
    root("c2", { mainline: true, parents: ["c1"] }),
    root("c1", { mainline: true }),
  ]);
  const rails = compactRails(proj);
  assert.equal(rails.length, 3);
  for (const entry of rails) {
    assert.deepEqual(laneNumbers(entry), [0]);
    assert.equal(entry.lanes[0].color, ACCENT);
    assert.equal(entry.node.kind, "trunk");
    assert.equal(entry.node.lane, 0);
    assert.deepEqual(entry.diagonals, []);
  }
  assert.equal(rails[0].lanes[0].activeThrough, true);
  assert.equal(rails[2].lanes[0].activeThrough, false, "lane ends on the last row");
  const html = renderRail(rails[1]);
  assert.equal((html.match(/lane-line/g) || []).length, 1, "one vertical line");
  assert.ok(html.includes(`fill="${ACCENT}"`), "filled accent dot");
  assert.ok(!html.includes("<path"), "no diagonals");
});

test("real-repo mirror: merge row draws double-ring + 3 diagonals, spans and branch-offs line up", () => {
  const rails = compactRails(mirrorFixture());

  // Row 0 (t4): above the merge, no fork activity yet.
  assert.deepEqual(laneNumbers(rail(rails, "t4")), [0]);
  assert.deepEqual(rail(rails, "t4").diagonals, []);

  // Row 1 (m1): the 4-parent merge — double-ring on lane 0, one diagonal
  // per merged fork lane, all four lanes present from here down.
  const merge = rail(rails, "m1");
  assert.equal(merge.node.kind, "merge");
  assert.equal(merge.node.lane, 0);
  assert.deepEqual(laneNumbers(merge), [0, 1, 2, 3]);
  const mergeDiags = merge.diagonals.filter((d) => d.kind === "merge");
  assert.equal(mergeDiags.length, 3, "one diagonal per fork lane");
  assert.deepEqual(
    mergeDiags.map((d) => d.toLane).sort(),
    [1, 2, 3]
  );
  for (const d of mergeDiags) {
    assert.equal(d.fromLane, 0);
    assert.ok(d.x2 > d.x1, "diagonal runs down-right");
  }
  const mergeHtml = renderRail(merge);
  assert.equal((mergeHtml.match(/<circle/g) || []).length, 2, "double-ring bubble");
  assert.equal((mergeHtml.match(/<path/g) || []).length, 3, "three diagonal connectors");

  // Rows between the merge and the tips carry all fork lanes vertically.
  assert.deepEqual(laneNumbers(rail(rails, "t3")), [0, 1, 2, 3]);

  // Tip rows: dots on their lanes; only the row's rightmost lane shows a label.
  const fa1 = rail(rails, "fa1");
  assert.equal(fa1.node.kind, "fork");
  assert.equal(fa1.node.lane, 3);
  assert.equal(fa1.label, "api-red…", "tip label truncated to 8 chars when space allows");
  assert.ok(fa1.node.title.includes("api-redesign"), "full name stays in the hover title");
  const fb = rail(rails, "fb");
  assert.equal(fb.node.lane, 2);
  assert.equal(fb.label, undefined, "not the rightmost active lane — just the dot");
  assert.ok(fb.node.title.includes("ui-polish"));
  const fc = rail(rails, "fc");
  assert.equal(fc.node.lane, 1);
  assert.equal(fc.label, undefined);
  assert.ok(fc.node.title.includes("feature-c-long"));

  // Chain middle node shares the tip's lane.
  assert.equal(rail(rails, "fa0").node.lane, 3);
  assert.deepEqual(laneNumbers(rail(rails, "fa0")), [0, 1, 2, 3]);

  // Branch-off rows: the diagonal replaces the lane's vertical in that cell.
  const t2 = rail(rails, "t2");
  assert.equal(t2.node.forkPoint, true, "fork_point flag survives");
  assert.deepEqual(laneNumbers(t2), [0, 2, 3], "the branching lane is not vertical here");
  const branchC = t2.diagonals.find((d) => d.kind === "branch");
  assert.ok(branchC && branchC.toLane === 1, "off-diagonal into the new fork lane");
  const t2Html = renderRail(t2);
  assert.ok(t2Html.includes('fill="none"'), "fork_point renders the hollow ring");
  assert.equal((t2Html.match(/lane-line/g) || []).length, 3);
  assert.equal((t2Html.match(/<path/g) || []).length, 1);
  assert.deepEqual(laneNumbers(rail(rails, "t1")), [0, 3]);
  assert.equal(rail(rails, "t1").diagonals.find((d) => d.kind === "branch").toLane, 2);
  assert.deepEqual(laneNumbers(rail(rails, "t0")), [0]);
  assert.equal(rail(rails, "t0").diagonals.find((d) => d.kind === "branch").toLane, 3);

  // Lane span ends: f-c's vertical stops after its oldest chain row; its
  // branch diagonal at t2 completes it.
  const fa0Lanes = rail(rails, "fa0").lanes;
  assert.equal(fa0Lanes.find((l) => l.lane === 1).activeThrough, false);
  assert.equal(fa0Lanes.find((l) => l.lane === 3).activeThrough, true);
});

test("compactRails follows the projection's root array order exactly — no re-sort", () => {
  const proj = mirrorFixture();
  // The fixture's created_at values are scrambled; the array order is the
  // newest-first display order the list renders.
  const rails = compactRails(proj);
  assert.deepEqual(
    rails.map((r) => r.rootId),
    proj.roots.map((r) => r.id)
  );
});

test("a row with no fork activity renders the plain trunk lane only", () => {
  const proj = projection(
    [
      root("new1", { mainline: true, parents: ["base"] }),
      root("base", { mainline: true }),
      root("old1", { parents: ["base"] }),
    ],
    // Fork with no chain in-window and no merge: never claimed, never drawn.
    [fork("ghost", "missing-tip", "base")]
  );
  const rails = compactRails(proj);
  for (const entry of rails) {
    assert.deepEqual(laneNumbers(entry), [0]);
    assert.deepEqual(entry.diagonals, []);
  }
});

test("empty projections render no rails and renderRail tolerates a missing rail", () => {
  assert.deepEqual(compactRails(undefined), []);
  assert.deepEqual(compactRails(projection([])), []);
  assert.equal(renderRail(undefined), "");
});

test("unmerged fork lane runs from its tip row down to the branch-off row", () => {
  const proj = projection(
    [
      root("t2", { mainline: true, parents: ["t1"] }),
      root("tip1", { parents: ["t0"] }),
      root("t1", { mainline: true, parents: ["t0"] }),
      root("t0", { mainline: true }),
    ],
    [fork("side", "tip1", "t0")]
  );
  const rails = compactRails(proj);
  assert.deepEqual(laneNumbers(rail(rails, "t2")), [0], "no lane above the unmerged tip");
  assert.deepEqual(laneNumbers(rail(rails, "tip1")), [0, 1]);
  assert.equal(rail(rails, "tip1").node.kind, "fork");
  assert.equal(rail(rails, "tip1").label, "side", "tip labeled on its own row");
  assert.deepEqual(laneNumbers(rail(rails, "t1")), [0, 1], "lane continues toward the base");
  const base = rail(rails, "t0");
  assert.deepEqual(laneNumbers(base), [0]);
  assert.equal(base.diagonals.length, 1);
  assert.equal(base.diagonals[0].kind, "branch");
  assert.equal(base.diagonals[0].toLane, 1);
});

test("the Lineage tab renders list rows with rail cells and no big graph block; node clicks post showRoot", () => {
  const html = workbenchHtml("testnonce");
  const src = html.match(/<script nonce="testnonce">([\s\S]*?)<\/script>/)[1];
  const panel = {
    innerHTML: "",
    listeners: {},
    scrollTop: 0,
    scrollLeft: 0,
    querySelector: () => null,
    addEventListener(type, fn) {
      this.listeners[type] = fn;
    },
  };
  const posted = [];
  let messageHandler;
  vm.runInNewContext(src, {
    acquireVsCodeApi: () => ({
      postMessage: (msg) => posted.push(msg),
      getState: () => ({}),
      setState: () => {},
    }),
    window: {
      addEventListener: (type, fn) => {
        if (type === "message") messageHandler = fn;
      },
    },
    document: {
      querySelectorAll: () => [],
      getElementById: (id) => (id === "panel" ? panel : null),
    },
  });
  messageHandler({ data: { tab: "lineage", lineage: mirrorFixture(), roots: [] } });
  const out = panel.innerHTML;
  assert.ok(out.includes('class="rail"'), "rows carry rail cells");
  assert.ok(out.includes("lane-line"), "lane lines render");
  assert.ok(out.includes('data-act="showRoot"'), "node markers are clickable");
  assert.ok(!out.includes("lineage-graph"), "the separate big graph block is gone");
  assert.ok(out.includes('data-act="compare"'), "legacy compare UI intact");
  assert.ok(out.includes('data-act="diff"'), "legacy diff UI intact");
  assert.ok(out.includes("Select two roots"), "legacy list instructions intact");
  const fakeNode = {
    getAttribute: (name) =>
      name === "data-act" ? "showRoot" : name === "data-id" ? "m1" : null,
  };
  panel.listeners.click({ target: { nodeType: 1, closest: () => fakeNode } });
  const show = posted.filter((msg) => msg.type === "showRoot");
  assert.equal(show.length, 1);
  assert.equal(show[0].id, "m1");
});
