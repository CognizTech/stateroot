// StateRoot — Project Continuity view.
//
// Thin client by design: the sidebar READS the `.stateroot/` store files
// (plain JSON/JSONL/Markdown — stable, documented formats) and every write
// goes through the `stateroot` CLI. Nothing here reimplements the engine.

import * as vscode from "vscode";
import * as cp from "child_process";
import * as fs from "fs";
import * as path from "path";

const STORE = ".stateroot";

// ---------------------------------------------------------------------------
// store reads (defensive: every file may be absent or half-written)
// ---------------------------------------------------------------------------

function readJson<T = any>(file: string): T | undefined {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return undefined;
  }
}

function projectRoot(): string | undefined {
  for (const folder of vscode.workspace.workspaceFolders ?? []) {
    const dir = folder.uri.fsPath;
    if (fs.existsSync(path.join(dir, STORE, "manifest.json"))) {
      return dir;
    }
  }
  return undefined;
}

interface EpisodicRecord {
  ts?: string;
  harness?: string;
  note?: string;
}

function recentEpisodic(root: string, limit: number): EpisodicRecord[] {
  try {
    const text = fs.readFileSync(path.join(root, STORE, "memories", "episodic.jsonl"), "utf8");
    const lines = text.split("\n").filter((l) => l.trim());
    return lines
      .slice(-limit)
      .map((l) => {
        try {
          return JSON.parse(l) as EpisodicRecord;
        } catch {
          return undefined;
        }
      })
      .filter((r): r is EpisodicRecord => !!r)
      .reverse();
  } catch {
    return [];
  }
}

function newestPlan(root: string): { title: string; file: string } | undefined {
  try {
    const dir = path.join(root, STORE, "plans");
    const files = fs
      .readdirSync(dir)
      .filter((f) => f.endsWith(".md"))
      .map((f) => ({ f, mtime: fs.statSync(path.join(dir, f)).mtimeMs }))
      .sort((a, b) => b.mtime - a.mtime);
    const first = files[0];
    if (!first) {
      return undefined;
    }
    const text = fs.readFileSync(path.join(dir, first.f), "utf8");
    const title =
      text.split("\n").find((l) => l.startsWith("# "))?.replace(/^#\s+/, "").trim() ??
      first.f.replace(/\.md$/, "");
    return { title, file: path.join(dir, first.f) };
  } catch {
    return undefined;
  }
}

function relTime(iso?: string): string {
  if (!iso) {
    return "unknown";
  }
  const then = Date.parse(iso);
  if (Number.isNaN(then)) {
    return iso;
  }
  const mins = Math.max(0, Math.round((Date.now() - then) / 60000));
  if (mins < 1) {
    return "just now";
  }
  if (mins < 60) {
    return `${mins}m ago`;
  }
  const hours = Math.round(mins / 60);
  if (hours < 48) {
    return `${hours}h ago`;
  }
  return `${Math.round(hours / 24)}d ago`;
}

function asLine(value: unknown): string {
  if (typeof value === "string") {
    return value;
  }
  if (value && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    return String(obj.text ?? obj.note ?? obj.title ?? JSON.stringify(value));
  }
  return String(value ?? "");
}

// ---------------------------------------------------------------------------
// CLI (writes only)
// ---------------------------------------------------------------------------

function cliPath(): string {
  const configured = vscode.workspace.getConfiguration("stateroot").get<string>("cliPath");
  return configured && configured.trim() ? configured.trim() : "stateroot";
}

function runCli(args: string[], cwd: string): Promise<string> {
  return new Promise((resolve, reject) => {
    cp.execFile(cliPath(), args, { cwd }, (err, stdout, stderr) => {
      if (err) {
        reject(new Error(stderr.trim() || err.message));
      } else {
        resolve(stdout);
      }
    });
  });
}

let output: vscode.OutputChannel;

async function runCliReport(args: string[], cwd: string, refresh: () => void) {
  try {
    const out = await runCli(args, cwd);
    output.appendLine(`$ stateroot ${args.join(" ")}`);
    output.appendLine(out);
    refresh();
  } catch (err: any) {
    if (err?.code === "ENOENT" || /not found|ENOENT/.test(String(err?.message))) {
      const pick = await vscode.window.showErrorMessage(
        "The `stateroot` binary is not on PATH (or stateroot.cliPath).",
        "Install StateRoot"
      );
      if (pick) {
        vscode.env.openExternal(
          vscode.Uri.parse("https://stateroot.dev/docs/getting-started/installation")
        );
      }
    } else {
      vscode.window.showErrorMessage(`stateroot ${args[0]} failed: ${err.message ?? err}`);
    }
  }
}

// ---------------------------------------------------------------------------
// tree view
// ---------------------------------------------------------------------------

class Row extends vscode.TreeItem {
  children?: Row[];

  constructor(label: string, opts?: Partial<Row>) {
    super(label, opts?.collapsibleState ?? vscode.TreeItemCollapsibleState.None);
    Object.assign(this, opts);
  }
}

class ContinuityProvider implements vscode.TreeDataProvider<Row> {
  private emitter = new vscode.EventEmitter<Row | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  refresh() {
    this.emitter.fire(undefined);
  }

  getTreeItem(element: Row): vscode.TreeItem {
    return element;
  }

  getChildren(element?: Row): Row[] {
    if (element) {
      return element.children ?? [];
    }
    const root = projectRoot();
    if (!root) {
      return [
        new Row("Initialize StateRoot in this workspace…", {
          command: { command: "stateroot.init", title: "Initialize" },
          iconPath: new vscode.ThemeIcon("sparkle"),
        }),
      ];
    }
    const store = path.join(root, STORE);
    const manifest = readJson(path.join(store, "manifest.json"));
    const state = readJson(path.join(store, "project", "state.json"));
    const handoff = readJson(path.join(store, "handoffs", "current.json"));
    const plan = newestPlan(root);

    const rows: Row[] = [];
    const phase = state?.current_phase ?? manifest?.phase ?? "—";
    const objective = (state?.objective || handoff?.objective || "").trim();
    rows.push(
      new Row(`${manifest?.name ?? path.basename(root)} · ${phase}`, {
        description: objective ? objective.slice(0, 80) : undefined,
        tooltip: objective || undefined,
        iconPath: new vscode.ThemeIcon("target"),
      })
    );

    if (plan) {
      rows.push(
        new Row(`Active plan: ${plan.title}`, {
          iconPath: new vscode.ThemeIcon("notebook"),
          command: {
            command: "vscode.open",
            title: "Open plan",
            arguments: [vscode.Uri.file(plan.file)],
          },
        })
      );
    }

    if (handoff) {
      const activity = handoff.last_activity;
      const activityLine = activity?.at
        ? `${activity.harness ?? "?"} · ${activity.kind ?? "?"} · ${relTime(activity.at)}`
        : undefined;
      rows.push(
        new Row(`Handoff #${handoff.seq ?? "?"} by ${handoff.created_by_harness ?? "?"}`, {
          description: relTime(handoff.written_at ?? handoff.created_at),
          iconPath: new vscode.ThemeIcon("arrow-swap"),
          tooltip: handoff.objective ?? "",
          children: [
            activityLine ? new Row(`latest: ${activityLine}`) : undefined,
            handoff.task ? new Row(`task: ${String(handoff.task).slice(0, 120)}`) : undefined,
            ...(Array.isArray(handoff.next_actions)
              ? handoff.next_actions.slice(0, 3).map((a: unknown) => new Row(`next: ${asLine(a).slice(0, 120)}`))
              : []),
          ].filter(Boolean) as Row[],
        })
      );
    }

    const checkpoints = recentEpisodic(root, 8);
    if (checkpoints.length) {
      rows.push(
        new Row("Recent checkpoints", {
          collapsibleState: vscode.TreeItemCollapsibleState.Collapsed,
          iconPath: new vscode.ThemeIcon("history"),
          children: checkpoints.map(
            (c) =>
              new Row(`${c.harness ?? "cli"} · ${(c.note ?? "").slice(0, 80)}`, {
                tooltip: `${c.ts ?? ""}\n${c.note ?? ""}`,
                description: relTime(c.ts),
              })
          ),
        })
      );
    }

    rows.push(
      new Row("Checkpoint…", {
        command: { command: "stateroot.checkpoint", title: "Checkpoint" },
        iconPath: new vscode.ThemeIcon("check"),
      }),
      new Row("Snapshot (snap)", {
        command: { command: "stateroot.snap", title: "Snap" },
        iconPath: new vscode.ThemeIcon("git-branch"),
      }),
      new Row("Write handoff…", {
        command: { command: "stateroot.handoff", title: "Handoff" },
        iconPath: new vscode.ThemeIcon("send"),
      }),
      new Row("Doctor", {
        command: { command: "stateroot.doctor", title: "Doctor" },
        iconPath: new vscode.ThemeIcon("pulse"),
      })
    );
    return rows;
  }
}

// ---------------------------------------------------------------------------
// activate
// ---------------------------------------------------------------------------

export function activate(context: vscode.ExtensionContext) {
  output = vscode.window.createOutputChannel("StateRoot");
  const provider = new ContinuityProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("stateroot.overview", provider)
  );

  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10);
  status.command = "stateroot.refresh";
  const updateStatus = () => {
    const root = projectRoot();
    if (!root) {
      status.text = "$(circle-slash) stateroot";
      status.tooltip = "No StateRoot project in this workspace — click to initialize";
      status.command = "stateroot.init";
    } else {
      const state = readJson(path.join(root, STORE, "project", "state.json"));
      status.text = `$(flame) stateroot: ${state?.current_phase ?? "ready"}`;
      status.tooltip = "StateRoot — project continuity (click to refresh)";
      status.command = "stateroot.refresh";
    }
    status.show();
  };
  updateStatus();

  const refreshAll = () => {
    provider.refresh();
    updateStatus();
  };

  const withProject = (fn: (root: string) => void) => {
    const root = projectRoot();
    if (!root) {
      vscode.window.showInformationMessage("No StateRoot project here — run StateRoot: Initialize first.");
      return;
    }
    fn(root);
  };

  context.subscriptions.push(
    vscode.commands.registerCommand("stateroot.init", async () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      if (!folder) {
        vscode.window.showErrorMessage("Open a folder first.");
        return;
      }
      await runCliReport(["init"], folder.uri.fsPath, refreshAll);
    }),
    vscode.commands.registerCommand("stateroot.refresh", refreshAll),
    vscode.commands.registerCommand("stateroot.checkpoint", () =>
      withProject(async (root) => {
        const note = await vscode.window.showInputBox({
          prompt: "Checkpoint note — what changed and why?",
          placeHolder: "wired auth middleware — unblocks handlers",
        });
        if (!note?.trim()) {
          return;
        }
        await runCliReport(["checkpoint", "--note", note.trim()], root, refreshAll);
      })
    ),
    vscode.commands.registerCommand("stateroot.snap", () =>
      withProject(async (root) => {
        await runCliReport(["snap"], root, refreshAll);
      })
    ),
    vscode.commands.registerCommand("stateroot.resume", () =>
      withProject(async (root) => {
        await runCliReport(["resume", "--harness", "vscode-copilot", "--force"], root, refreshAll);
        output.show(true);
      })
    ),
    vscode.commands.registerCommand("stateroot.handoff", () =>
      withProject(async (root) => {
        const objective = await vscode.window.showInputBox({ prompt: "Handoff objective" });
        if (!objective?.trim()) {
          return;
        }
        const task = await vscode.window.showInputBox({ prompt: "Current task" });
        if (!task?.trim()) {
          return;
        }
        const next = await vscode.window.showInputBox({ prompt: "Next actions (optional)" });
        const args = [
          "handoff",
          "write",
          "--from",
          "vscode-copilot",
          "--objective",
          objective.trim(),
          "--task",
          task.trim(),
          "--context-summary",
          "written from the VSCode extension",
        ];
        if (next?.trim()) {
          args.push("--next", next.trim());
        }
        await runCliReport(args, root, refreshAll);
      })
    ),
    vscode.commands.registerCommand("stateroot.doctor", () =>
      withProject(async (root) => {
        await runCliReport(["doctor"], root, refreshAll);
        output.show(true);
      })
    )
  );

  // Live continuity: another harness writing state refreshes the view.
  const watcher = vscode.workspace.createFileSystemWatcher(`**/${STORE}/**`);
  let timer: NodeJS.Timeout | undefined;
  const debounced = () => {
    clearTimeout(timer);
    timer = setTimeout(refreshAll, 400);
  };
  watcher.onDidCreate(debounced);
  watcher.onDidChange(debounced);
  watcher.onDidDelete(debounced);
  context.subscriptions.push(watcher, status, output);
}

export function deactivate() {
  // nothing to dispose beyond subscriptions
}
