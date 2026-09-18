const assert = require("node:assert/strict");
const test = require("node:test");
const vm = require("node:vm");

// out/lineageGraph.js is pure (type-only imports), so it loads directly.
const { layoutLineage, renderLineageGraph } = require("../out/lineageGraph");
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

test("rows are newest-first by created_at with id tiebreak descending", () => {
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
  layout.nodes.forEach((n, row) => assert.equal(n.row, row));
});

test("trunk roots sit on lane 0 and each fork chain gets its own lane in base_root recency order", () => {
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
  assert.equal(node(layout, "t0").lane, 0);
  assert.equal(node(layout, "t1").lane, 0);
  assert.equal(node(layout, "t2").lane, 0);
  assert.equal(node(layout, "t2").color, ACCENT);
  // f2 branched from the more recent base_root, so it gets the first fork lane.
  assert.equal(node(layout, "b1").lane, 1);
  assert.equal(node(layout, "a2").lane, 2);
  assert.equal(node(layout, "a1").lane, 2);
  assert.deepEqual(layout.laneNames, ["trunk", "f2", "f1"]);
  assert.notEqual(node(layout, "b1").color, ACCENT, "fork lane has its own color");
  // In-lane chain edge vs the branch-off arc back to the trunk base.
  const chain = layout.edges.find((e) => e.from === "a2" && e.to === "a1");
  assert.equal(chain.crossLane, false);
  const branchOff = layout.edges.find((e) => e.from === "a1" && e.to === "t0");
  assert.equal(branchOff.crossLane, true);
});

test("a 2-fork merge root draws exactly one arc per fork tip and renders as a double-ring bubble", () => {
  const proj = projection(
    [
      root("m1", {
        mainline: true,
        parents: ["t1", "a1", "b1"],
        created_at: "2026-09-18T10:00:00Z",
        created_reason: "merge f1, f2",
      }),
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
  assert.equal(merge.lane, 0, "merge roots stay on the trunk lane");
  const arcs = layout.edges.filter((e) => e.from === "m1" && e.crossLane);
  assert.equal(arcs.length, 2, "one arc per fork tip");
  assert.deepEqual(
    arcs.map((e) => e.to).sort(),
    ["a1", "b1"]
  );
  const trunkEdge = layout.edges.find((e) => e.from === "m1" && e.to === "t1");
  assert.equal(trunkEdge.crossLane, false, "mainline continuation stays vertical");

  const html = renderLineageGraph(proj);
  const group = html.match(/<g class="node merge"[^>]*>([\s\S]*?)<\/g>/);
  assert.ok(group, "merge node group renders");
  assert.equal((group[1].match(/<circle/g) || []).length, 2, "double-ring bubble");
  assert.ok(group[1].includes(`stroke="${ACCENT}"`), "bubble in the accent color");
});

test("a contained (merged) fork still shows its lane and the arc into the merge bubble", () => {
  const proj = projection(
    [
      root("m2", {
        mainline: true,
        parents: ["t1", "a1"],
        created_at: "2026-09-18T10:00:00Z",
      }),
      root("a1", { parents: ["t0"], created_at: "2026-09-18T09:30:00Z" }),
      root("t1", { mainline: true, parents: ["t0"], created_at: "2026-09-18T09:00:00Z" }),
      root("t0", { mainline: true, created_at: "2026-09-18T08:00:00Z" }),
    ],
    [fork("f1", "a1", "t0", { contained: true })]
  );
  const layout = layoutLineage(proj);
  assert.equal(node(layout, "m2").kind, "merge", "2-parent root whose second parent is not the previous mainline root is a merge");
  const tip = node(layout, "a1");
  assert.equal(tip.lane, 1, "contained fork keeps its lane");
  assert.equal(tip.kind, "fork");
  const arc = layout.edges.find((e) => e.from === "m2" && e.to === "a1");
  assert.ok(arc && arc.crossLane, "arc from merge node to fork tip");
});

test("more than 60 non-structural roots fill the budget and the footer reports the true count", () => {
  const roots = [];
  for (let i = 0; i < 65; i++) {
    roots.push(
      root("r" + String(i).padStart(3, "0"), {
        mainline: true,
        parents: i ? ["r" + String(i - 1).padStart(3, "0")] : [],
        created_at: new Date(Date.UTC(2026, 8, 18, 0, i)).toISOString(),
      })
    );
  }
  const layout = layoutLineage(projection(roots));
  assert.equal(layout.totalRoots, 65);
  assert.equal(layout.shownRoots, 60);
  assert.equal(layout.nodes.length, 60);
  assert.equal(layout.nodes[0].id, "r064", "newest first after truncation");
  const html = renderLineageGraph(projection(roots));
  assert.ok(html.includes("showing 60 of 65 roots · 5 older roots hidden"));
});

test("structural nodes survive the window: 3 fork tips beyond position 60 still arc into the merge bubble", () => {
  // Mirrors the owner's repo shape: 100 roots, the 4-parent merge sits at
  // row ~9 in newest-first order while all three fork tips fall beyond
  // position 60 — the case the old newest-60 slice amputated.
  const ts = (minute) => new Date(Date.UTC(2026, 8, 18, 0, minute)).toISOString();
  const roots = [];
  for (let i = 0; i <= 94; i++) {
    roots.push(
      root("t" + String(i).padStart(2, "0"), {
        mainline: true,
        parents: i ? ["t" + String(i - 1).padStart(2, "0")] : [],
        created_at: ts(i),
      })
    );
  }
  roots.push(root("fap", { parents: ["t05"], created_at: ts(20) }));
  roots.push(root("fa", { parents: ["fap"], created_at: ts(21) }));
  roots.push(root("fb", { parents: ["t12"], created_at: ts(22) }));
  roots.push(root("fc", { parents: ["t18"], created_at: ts(23) }));
  roots.push(
    root("m-big", {
      mainline: true,
      parents: ["t84", "fa", "fb", "fc"],
      created_at: ts(85),
      created_reason: "merge three forks",
    })
  );
  const proj = projection(roots, [
    fork("f-a", "fa", "t05", { contained: true }),
    fork("f-b", "fb", "t12", { contained: true }),
    fork("f-c", "fc", "t18", { contained: true }),
  ]);
  assert.equal(proj.roots.length, 100);

  // Precondition: the fixture really exercises the bug — under the old
  // newest-60 slice the tips were outside the window while the merge was in.
  const sorted = [...proj.roots].sort(
    (a, b) => b.created_at.localeCompare(a.created_at) || b.id.localeCompare(a.id)
  );
  const pos = (id) => sorted.findIndex((r) => r.id === id);
  assert.ok(pos("m-big") < 15, "merge near the top");
  assert.ok(pos("fa") >= 60 && pos("fb") >= 60 && pos("fc") >= 60, "tips beyond position 60");

  const layout = layoutLineage(proj);
  assert.equal(layout.totalRoots, 100);
  assert.equal(layout.shownRoots, 60, "structure plus filler still caps at 60");
  assert.deepEqual(layout.laneNames, ["trunk", "f-c", "f-b", "f-a"], "lanes in base_root recency order");
  const merge = node(layout, "m-big");
  assert.equal(merge.kind, "merge");
  assert.equal(merge.lane, 0);
  const lanes = [node(layout, "fa").lane, node(layout, "fb").lane, node(layout, "fc").lane];
  assert.ok(lanes.every((lane) => lane > 0), "every tip gets a fork lane");
  assert.equal(new Set(lanes).size, 3, "three distinct fork lanes");
  assert.equal(node(layout, "fap").lane, node(layout, "fa").lane, "chain node shares the tip lane");
  // Trunk bases arrive via the parent-edge closure, not the budget.
  node(layout, "t05");
  node(layout, "t12");
  node(layout, "t18");
  const arcs = layout.edges.filter((e) => e.from === "m-big" && e.crossLane);
  assert.equal(arcs.length, 3, "exactly one arc per fork tip into the merge bubble");
  assert.deepEqual(
    arcs.map((e) => e.to).sort(),
    ["fa", "fb", "fc"]
  );
  const continuation = layout.edges.find((e) => e.from === "m-big" && e.to === "t84");
  assert.ok(continuation && !continuation.crossLane, "trunk continuation draws vertically");

  const html = renderLineageGraph(proj);
  assert.ok(
    html.startsWith('<div class="lineage-graph">'),
    "wrapper div owns scroll + footer; the webview concatenates the fragment into panel.innerHTML"
  );
  assert.ok(html.includes("<svg"));
  assert.ok(html.includes("showing 60 of 100 roots · 40 older roots hidden"));
});

test("structural overflow renders beyond 60 and the footer reports the true count", () => {
  const ts = (minute) => new Date(Date.UTC(2026, 8, 18, 0, minute)).toISOString();
  const roots = [];
  for (let i = 0; i <= 79; i++) {
    roots.push(
      root("s" + String(i).padStart(2, "0"), {
        mainline: true,
        fork_point: i >= 10, // s10..s79: 70 structural roots
        parents: i ? ["s" + String(i - 1).padStart(2, "0")] : [],
        created_at: ts(i),
      })
    );
  }
  const layout = layoutLineage(projection(roots));
  assert.equal(layout.totalRoots, 80);
  assert.equal(layout.shownRoots, 71, "70 fork_points plus the trunk parent of the oldest one");
  assert.equal(layout.nodes.length, 71, "structure alone may exceed the 60 budget");
  assert.equal(layout.nodes[0].id, "s79", "newest first even in overflow");
  const html = renderLineageGraph(projection(roots));
  assert.ok(html.includes("showing 71 of 80 roots · 9 older roots hidden"));
});

test("an empty projection renders the empty-state line", () => {
  const html = renderLineageGraph(projection([]));
  assert.ok(html.includes("No lineage yet"));
  assert.ok(html.includes("stateroot snap"));
  assert.ok(!html.includes("<svg"));
  assert.equal(renderLineageGraph(undefined), renderLineageGraph(projection([])));
});

test("long reasons truncate to 64 chars and the hover title carries full detail", () => {
  const reason = "x".repeat(100);
  const html = renderLineageGraph(
    projection([
      root("abc12345def0", {
        mainline: true,
        created_at: "2026-09-18T10:00:00Z",
        created_by_harness: "claude-code",
        created_reason: reason,
        files_pinned: 42,
        coverage: "src/**",
      }),
    ])
  );
  assert.ok(html.includes("x".repeat(63) + "…"), "reason clipped at 63 chars + ellipsis");
  assert.ok(!html.includes("x".repeat(64) + "…"), "no longer clip");
  const title = html.match(/<title>([\s\S]*?)<\/title>/)[1];
  assert.ok(title.includes("abc12345def0"), "title has the full id");
  assert.ok(title.includes("2026-09-18T10:00:00Z"), "title has created_at");
  assert.ok(title.includes("claude"), "title has the harness");
  assert.ok(title.includes(reason), "title has the untruncated reason");
  assert.ok(title.includes("42"), "title has files_pinned");
  assert.ok(title.includes("src/**"), "title has coverage");
  assert.ok(html.includes(" · claude · abc12345"), "row label carries muted harness + shortid");
  assert.ok(html.includes("all roots shown"), "honest footer when nothing is hidden");
});

test("fork_point roots render as a hollow ring and lane headers sit above the lane's first node", () => {
  const html = renderLineageGraph(
    projection(
      [
        root("b1", { parents: ["t0"], created_at: "2026-09-18T09:30:00Z" }),
        root("t0", {
          mainline: true,
          fork_point: true,
          created_at: "2026-09-18T08:00:00Z",
        }),
      ],
      [fork("side-quest", "b1", "t0")]
    )
  );
  const group = html.match(/<g class="node trunk" data-act="showRoot" data-id="t0">([\s\S]*?)<\/g>/);
  assert.ok(group, "fork_point trunk node renders");
  assert.ok(group[1].includes('fill="none"'), "hollow ring marker");
  assert.ok(!group[1].includes(`r="5"`), "no filled circle for a fork_point");
  assert.ok(html.includes("lane-head"), "fork lane header renders");
  assert.ok(html.includes("side-que"), "header carries the fork name");
});

test("the Lineage tab renders the graph and node clicks post showRoot to the extension", () => {
  const html = workbenchHtml("testnonce");
  const src = html.match(/<script nonce="testnonce">([\s\S]*?)<\/script>/)[1];
  const panel = {
    innerHTML: "",
    listeners: {},
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
  messageHandler({
    data: {
      tab: "lineage",
      lineage: projection(
        [
          root("m1", { mainline: true, parents: ["t1", "a1"], created_at: "2026-09-18T10:00:00Z" }),
          root("a1", { parents: ["t1"], created_at: "2026-09-18T09:30:00Z" }),
          root("t1", { mainline: true, created_at: "2026-09-18T09:00:00Z" }),
        ],
        [fork("f1", "a1", "t1")]
      ),
      roots: [],
    },
  });
  assert.ok(panel.innerHTML.includes("<svg"), "graph renders in the lineage tab");
  assert.ok(panel.innerHTML.includes('data-id="m1"'), "nodes are clickable targets");
  const fakeNode = {
    getAttribute: (name) =>
      name === "data-act" ? "showRoot" : name === "data-id" ? "m1" : null,
  };
  panel.listeners.click({ target: { nodeType: 1, closest: () => fakeNode } });
  const show = posted.filter((msg) => msg.type === "showRoot");
  assert.equal(show.length, 1);
  assert.equal(show[0].id, "m1");
});
