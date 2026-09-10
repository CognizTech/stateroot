import * as vscode from "vscode";
import * as path from "path";
import * as fs from "fs";
import { spawnSync } from "child_process";
import {
  cliPath,
  isCliProbeAvailable,
  parseDelegateList,
  refreshCliProbe,
  runCliReport,
  runCli,
  useCli,
} from "./cli";
import { SidebarProvider } from "./sidebarProvider";
import {
  enableCopilotHooks,
  isVSCodeWithCopilot,
  maybeOfferCopilotHooks,
} from "./copilotAssist";
import {
  CLI_MODE_HARNESSES,
  learningFilePath,
  listDelegations,
  listLearnings,
  listMemory,
  listPlans,
  memoryFilePath,
  memoryNeedle,
  planBodyPath,
  projectRoot,
  shortHash,
  STORE,
  wikiPagePath,
  planExcerpt,
} from "./store";
import { snapshot, type Snapshot } from "./snapshot";
import { WorkbenchPanel } from "./workbench";
import { terminalPathUpdater } from "./terminalPath";
import { maybePing } from "./installPing";
import { editorHarness, ensureSetup, SWITCH_PROMPTS, type SetupState } from "./setup";

export function activate(context: vscode.ExtensionContext) {
  const output = vscode.window.createOutputChannel("StateRoot");
  const THIS_HARNESS = editorHarness(vscode.env.appName);
  maybePing(context);
  const updateTerminalPath = terminalPathUpdater(context.environmentVariableCollection);
  const sidebar = new SidebarProvider(context.extensionUri, (msg) => void onMessage(msg));
  const workbench = new WorkbenchPanel(context.extensionUri, (msg) => void onMessage(msg));

  let selectedPlanId: string | undefined;
  let selectedHarness: string | undefined;
  let selectedTab = "control";
  let selectedLearningId: string | undefined;
  let selectedMemoryIndex: number | undefined;
  let rootA: string | undefined;
  let rootB: string | undefined;
  let compareText: string | undefined;
  let liveDelegations: Array<{ id: string; harness: string; status: string; task: string }> | undefined;
  let poll: NodeJS.Timeout | undefined;
  let storePoll: NodeJS.Timeout | undefined;
  let cliAvailable = true;
  let setup: SetupState = { phase: "checking", detail: "Checking StateRoot setup…" };
  let setupPending: Promise<void> | undefined;

  const dismissedKey = (root: string) => `stateroot.inbox.dismissed:${root}`;
  const dismissedFor = (root?: string): string[] =>
    root ? context.globalState.get<string[]>(dismissedKey(root), []) : [];

  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 10);
  status.command = "stateroot.openWorkbench";

  const currentSnapshot = (): Snapshot | { initialized: false } =>
    snapshot({
      selectedPlanId,
      selectedHarness,
      rootA,
      rootB,
      compareText,
      liveDelegations,
      tab: selectedTab,
      dismissedInbox: dismissedFor(projectRoot()),
      thisHarness: THIS_HARNESS,
      selectedLearningId,
      selectedMemoryIndex,
    });

  const push = () => {
    const probed = isCliProbeAvailable();
    if (probed !== undefined) {
      cliAvailable = probed;
    }
    if (cliAvailable) updateTerminalPath(cliPath());
    const state = currentSnapshot();
    sidebar.post({ ...state, setup });
    workbench.post(state);
    updateStatus(state);
  };

  const updateStatus = (state: Snapshot | { initialized: false }) => {
    if (setup.phase !== "ready") {
      status.text = setup.phase === "error" ? "$(warning) StateRoot: retry setup" : `$(sync~spin) ${setup.detail}`;
      status.tooltip = setup.detail;
      status.command = setup.phase === "error" ? "stateroot.retrySetup" : "stateroot.openSetup";
      status.show();
      return;
    }
    if (!("initialized" in state) || !state.initialized) {
      status.text = "$(circle-slash) stateroot";
      status.tooltip = "No StateRoot project — click to initialize";
      status.command = "stateroot.init";
      status.show();
      return;
    }
    if (!cliAvailable) {
      status.text = "$(warning) stateroot CLI missing";
      status.tooltip = "Install the StateRoot CLI for writes and live delegation";
      status.command = "stateroot.installCli";
      status.show();
      return;
    }
    const n = state.inbox.length;
    const root = state.latestRoot ? ` · ${state.latestRoot}` : "";
    status.text = n ? `$(flame) ${n} need you${root}` : `$(flame) stateroot${root}`;
    status.tooltip = "StateRoot — open workbench";
    status.command = "stateroot.openWorkbench";
    status.show();
    syncPoll(state);
  };

  const syncPoll = (state: Snapshot | { initialized: false }) => {
    const running =
      state.initialized && state.delegations.some((d) => d.status === "running");
    if (running && !poll) {
      poll = setInterval(() => void refreshLive(), 3000);
    } else if (!running && poll) {
      clearInterval(poll);
      poll = undefined;
    }
  };

  const refreshLive = async () => {
    const root = projectRoot();
    if (!root) {
      return;
    }
    const text = await runCliReport(["delegate", "list"], root, output, 20_000, {
      allowInstall: false,
    });
    liveDelegations = text ? parseDelegateList(text) : undefined;
    push();
  };

  const withProject = async (fn: (root: string) => Promise<void>) => {
    const root = projectRoot();
    if (!root) {
      vscode.window.showInformationMessage("No StateRoot project here — run StateRoot: Initialize first.");
      return;
    }
    await fn(root);
  };

  const onMessage = async (msg: Record<string, unknown>) => {
    const type = String(msg.type || "");
    if (type === "ready") {
      push();
      return;
    }
    if (type === "openWorkbench") {
      const tab = typeof msg.tab === "string" ? msg.tab : "control";
      selectedTab = tab;
      if (typeof msg.planId === "string") {
        selectedPlanId = msg.planId;
      }
      if (typeof msg.rootId === "string") {
        rootA = msg.rootId;
      }
      if (typeof msg.learningId === "string") {
        selectedLearningId = msg.learningId;
      }
      if (msg.memoryIndex != null && msg.memoryIndex !== "") {
        selectedMemoryIndex = Number(msg.memoryIndex);
      }
      workbench.reveal(tab);
      push();
      return;
    }
    if (type === "openTab") {
      if (typeof msg.tab === "string") {
        selectedTab = msg.tab;
      }
      if (typeof msg.planId === "string") {
        selectedPlanId = msg.planId;
      }
      if (msg.kind === "accept-handoff") {
        await vscode.commands.executeCommand("stateroot.resume");
        return;
      }
      push();
      return;
    }
    if (type === "dismiss" && typeof msg.id === "string") {
      await withProject(async (root) => {
        const key = dismissedKey(root);
        const id = String(msg.id);
        const current = dismissedFor(root);
        if (!current.includes(id)) {
          await context.globalState.update(key, [...current, id]);
        }
        push();
      });
      return;
    }
    if (type === "init") {
      await vscode.commands.executeCommand("stateroot.init");
      return;
    }
    if (type === "retrySetup") {
      await vscode.commands.executeCommand("stateroot.retrySetup");
      return;
    }
    if (type === "copySwitchPrompt" && typeof msg.index === "number" && SWITCH_PROMPTS[msg.index]) {
      await vscode.env.clipboard.writeText(SWITCH_PROMPTS[msg.index]);
      void vscode.window.showInformationMessage("Prompt copied — paste it into your agent chat.");
      return;
    }
    if (type === "demo") {
      await vscode.env.openExternal(vscode.Uri.parse("https://stateroot.dev"));
      return;
    }
    if (type === "handoff") {
      await vscode.commands.executeCommand("stateroot.handoff");
      return;
    }
    if (type === "checkpoint") {
      await vscode.commands.executeCommand("stateroot.checkpoint");
      return;
    }
    if (type === "selectPlan" && typeof msg.id === "string") {
      selectedPlanId = msg.id;
      push();
      return;
    }
    if (type === "approvePlan" && typeof msg.id === "string") {
      await withProject(async (root) => {
        await runCliReport(["plan", "approve", msg.id as string], root, output);
        push();
      });
      return;
    }
    if (type === "donePlan" && typeof msg.id === "string") {
      await withProject(async (root) => {
        await runCliReport(["plan", "done", msg.id as string], root, output);
        push();
      });
      return;
    }
    if (type === "openPlan" && typeof msg.id === "string") {
      await withProject(async (root) => {
        const filePath = planBodyPath(root, msg.id as string);
        try {
          if (fs.existsSync(filePath)) {
            const uri = vscode.Uri.file(filePath);
            const doc = await vscode.workspace.openTextDocument(uri);
            await vscode.window.showTextDocument(doc, { preview: false, viewColumn: vscode.ViewColumn.Beside });
            return;
          }
          const excerpt = planExcerpt(root, msg.id as string, 400);
          if (!excerpt) {
            vscode.window.showErrorMessage(`Plan body not found: ${path.basename(filePath)}`);
            return;
          }
          const doc = await vscode.workspace.openTextDocument({ content: excerpt, language: "markdown" });
          await vscode.window.showTextDocument(doc, { preview: false, viewColumn: vscode.ViewColumn.Beside });
        } catch (err: unknown) {
          const message = err instanceof Error ? err.message : String(err);
          vscode.window.showErrorMessage(`Could not open plan: ${message}`);
        }
      });
      return;
    }
    if (type === "delegatePlan" && typeof msg.id === "string") {
      await delegatePlan(String(msg.id), typeof msg.harness === "string" ? msg.harness : undefined);
      return;
    }
    if (type === "reassign" && typeof msg.id === "string") {
      await reassign(String(msg.id));
      return;
    }
    if (type === "log" && typeof msg.id === "string") {
      await withProject(async (root) => {
        await runCliReport(["delegate", "status", String(msg.id)], root, output);
        output.show(true);
      });
      return;
    }
    if (type === "selectRoot" && typeof msg.id === "string") {
      const id = String(msg.id);
      if (!rootA || (rootA && rootB)) {
        rootA = id;
        rootB = undefined;
        compareText = undefined;
      } else if (id !== rootA) {
        rootB = id;
      } else {
        rootA = id;
      }
      push();
      return;
    }
    if (type === "compare") {
      await runPair(["compare"]);
      return;
    }
    if (type === "diff") {
      await openDiff();
      return;
    }
    if (type === "revert") {
      await revertRoot();
      return;
    }
    if (type === "fork") {
      await forkRoot();
      return;
    }
    if (type === "selectLearning" && typeof msg.id === "string") {
      selectedLearningId = String(msg.id);
      push();
      return;
    }
    if (type === "selectMemory" && msg.index != null) {
      selectedMemoryIndex = Number(msg.index);
      push();
      return;
    }
    if (type === "addLearning") {
      await addLearning();
      return;
    }
    if (type === "editLearning" && typeof msg.id === "string") {
      await editLearning(String(msg.id));
      return;
    }
    if (type === "acceptLearning" && typeof msg.id === "string") {
      await withProject(async (root) => {
        await runCliReport(["learnings", "accept", String(msg.id)], root, output);
        push();
      });
      return;
    }
    if (type === "rejectLearning" && typeof msg.id === "string") {
      await rejectLearning(String(msg.id));
      return;
    }
    if (type === "openLearning" && typeof msg.id === "string") {
      await openLearning(String(msg.id));
      return;
    }
    if (type === "addMemory") {
      await addMemory();
      return;
    }
    if (type === "editMemory" && msg.index != null) {
      await editMemory(Number(msg.index));
      return;
    }
    if (type === "removeMemory" && msg.index != null) {
      await removeMemory(Number(msg.index));
      return;
    }
    if (type === "openMemoryFile") {
      await withProject(async (root) => {
        await openProjectFile(memoryFilePath(root));
      });
      return;
    }
    if (type === "openWiki" && typeof msg.rel === "string") {
      await withProject(async (root) => {
        await openProjectFile(wikiPagePath(root, String(msg.rel)));
      });
      return;
    }
  };

  const delegatePlan = async (planId: string, harness?: string) => {
    await withProject(async (root) => {
      const plan = listPlans(root).find((p) => p.id === planId);
      if (!plan) {
        vscode.window.showErrorMessage("Unknown plan.");
        return;
      }
      const to =
        harness ||
        (await vscode.window.showQuickPick([...CLI_MODE_HARNESSES], {
          placeHolder: "Assign execution",
        }));
      if (!to) {
        vscode.window.showWarningMessage("Pick a harness before delegating.");
        return;
      }
      if (plan.status === "draft") {
        const ok = await vscode.window.showWarningMessage(
          "Plan is still a draft. Approve and delegate?",
          { modal: true },
          "Approve and delegate"
        );
        if (ok !== "Approve and delegate") {
          return;
        }
        const approved = await runCliReport(["plan", "approve", planId], root, output);
        if (approved === undefined) {
          return;
        }
      }
      const task = `Execute the plan at .stateroot/plans/${planId}.md. Read it first. Do not re-plan.`;
      await runCliReport(["delegate", "--to", to, "--task", task, "--json"], root, output);
      await refreshLive();
    });
  };

  const reassign = async (id: string) => {
    await withProject(async (root) => {
      const rec = listDelegations(root).find((d) => d.id === id || d.id.startsWith(id));
      if (!rec) {
        vscode.window.showErrorMessage("Unknown delegation.");
        return;
      }
      const to = await vscode.window.showQuickPick([...CLI_MODE_HARNESSES], {
        placeHolder: "Reassign to",
      });
      if (!to) {
        return;
      }
      await runCliReport(["delegate", "--to", to, "--task", rec.task, "--json"], root, output);
      await refreshLive();
    });
  };

  const runPair = async (args: string[]) => {
    await withProject(async (root) => {
      if (!rootA || !rootB) {
        vscode.window.showInformationMessage("Select two roots first.");
        return;
      }
      const out = await runCliReport([args[0], rootA, rootB], root, output, 60_000);
      if (out !== undefined) {
        compareText = out;
        push();
      }
    });
  };

  const openDiff = async () => {
    await withProject(async (root) => {
      if (!rootA || !rootB) {
        vscode.window.showInformationMessage("Select two roots first.");
        return;
      }
      const named = await runCliReport(["diff", rootA, rootB], root, output, 60_000);
      if (!named) {
        return;
      }
      const files = named
        .split(/\r?\n/)
        .map((l) => l.trim())
        .filter((l) => /^(added|deleted|modified|renamed)\s+/.test(l));
      if (files.length === 1) {
        const filePath = files[0].replace(/^\S+\s+/, "");
        try {
          const aUri = await gitShow(root, rootA, filePath);
          const bUri = await gitShow(root, rootB, filePath);
          if (aUri && bUri) {
            await vscode.commands.executeCommand(
              "vscode.diff",
              aUri,
              bUri,
              `${shortHash(rootA)} ↔ ${shortHash(rootB)} · ${filePath}`
            );
            return;
          }
        } catch {
          // fall through to unified diff
        }
      }
      const content = await runCliReport(["diff", rootA, rootB, "--content"], root, output, 120_000);
      if (!content) {
        return;
      }
      const doc = await vscode.workspace.openTextDocument({ content, language: "diff" });
      await vscode.window.showTextDocument(doc);
    });
  };

  const revertRoot = async () => {
    await withProject(async (root) => {
      const hash = rootA;
      if (!hash) {
        vscode.window.showInformationMessage("Select a root to restore.");
        return;
      }
      const ok = await vscode.window.showWarningMessage(
        `Restore creates a NEW root whose tree equals ${shortHash(hash)}. Existing roots are never rewritten.`,
        { modal: true },
        "Restore"
      );
      if (ok !== "Restore") {
        return;
      }
      await runCliReport(["revert", hash, "--yes"], root, output, 120_000);
      push();
    });
  };

  const forkRoot = async () => {
    await withProject(async (root) => {
      const hash = rootA;
      if (!hash) {
        vscode.window.showInformationMessage("Select a root to fork.");
        return;
      }
      const name = await vscode.window.showInputBox({
        prompt: "Fork branch name",
        value: `fork-${shortHash(hash)}`,
      });
      if (!name?.trim()) {
        return;
      }
      await runCliReport(["fork", hash, "--branch", name.trim()], root, output);
      push();
    });
  };

  const addLearning = async () => {
    await withProject(async (root) => {
      const note = await vscode.window.showInputBox({
        prompt: "Project learning (judgment / convention — not a fact)",
        placeHolder: "prefer X over Y when …",
        ignoreFocusOut: true,
      });
      if (!note?.trim()) {
        return;
      }
      await runCliReport(["learn", "record", "--", note.trim()], root, output);
      push();
    });
  };

  const editLearning = async (id: string) => {
    await withProject(async (root) => {
      const current = listLearnings(root).find((row) => row.id === id);
      if (!current) {
        vscode.window.showErrorMessage("Unknown learning.");
        return;
      }
      const statement = await vscode.window.showInputBox({
        prompt: `Edit ${id}`,
        value: current.statement,
        ignoreFocusOut: true,
      });
      if (!statement?.trim() || statement.trim() === current.statement) {
        return;
      }
      await runCliReport(
        ["learnings", "edit", id, "--statement", statement.trim()],
        root,
        output
      );
      push();
    });
  };

  const rejectLearning = async (id: string) => {
    await withProject(async (root) => {
      const ok = await vscode.window.showWarningMessage(
        `Reject learning ${id}? It is archived, not deleted.`,
        { modal: true },
        "Reject"
      );
      if (ok !== "Reject") {
        return;
      }
      await runCliReport(["learnings", "reject", id], root, output);
      push();
    });
  };

  const openLearning = async (id: string) => {
    await withProject(async (root) => {
      const current = listLearnings(root).find((row) => row.id === id);
      if (!current) {
        vscode.window.showErrorMessage("Unknown learning.");
        return;
      }
      await openProjectFile(learningFilePath(root, current.category, current.status === "candidate"));
    });
  };

  const addMemory = async () => {
    await withProject(async (root) => {
      const content = await vscode.window.showInputBox({
        prompt: "Project memory fact (not a learning)",
        placeHolder: "durable fact about this project",
        ignoreFocusOut: true,
      });
      if (!content?.trim()) {
        return;
      }
      await runCliReport(["memory", "add", "--", content.trim()], root, output);
      push();
    });
  };

  const memoryEntry = (root: string, index: number) =>
    listMemory(root).entries.find((entry) => entry.index === index);

  const editMemory = async (index: number) => {
    await withProject(async (root) => {
      const entry = memoryEntry(root, index);
      if (!entry) {
        vscode.window.showErrorMessage("Unknown memory entry.");
        return;
      }
      if (entry.text.length > 800) {
        await openProjectFile(memoryFilePath(root));
        vscode.window.showInformationMessage(
          "That entry is long — edit it in MEMORY.md, then save."
        );
        return;
      }
      const content = await vscode.window.showInputBox({
        prompt: `Edit memory ${index}`,
        value: entry.text,
        ignoreFocusOut: true,
      });
      if (!content?.trim() || content.trim() === entry.text) {
        return;
      }
      await runCliReport(
        ["memory", "replace", "--old", memoryNeedle(entry.text), "--", content.trim()],
        root,
        output
      );
      push();
    });
  };

  const removeMemory = async (index: number) => {
    await withProject(async (root) => {
      const entry = memoryEntry(root, index);
      if (!entry) {
        vscode.window.showErrorMessage("Unknown memory entry.");
        return;
      }
      const ok = await vscode.window.showWarningMessage(
        `Remove memory ${index}?`,
        { modal: true },
        "Remove"
      );
      if (ok !== "Remove") {
        return;
      }
      await runCliReport(["memory", "remove", "--", memoryNeedle(entry.text)], root, output);
      push();
    });
  };

  const openProjectFile = async (filePath: string) => {
    try {
      if (!fs.existsSync(filePath)) {
        vscode.window.showErrorMessage(`File not found: ${path.basename(filePath)}`);
        return;
      }
      const doc = await vscode.workspace.openTextDocument(vscode.Uri.file(filePath));
      await vscode.window.showTextDocument(doc, {
        preview: false,
        viewColumn: vscode.ViewColumn.Beside,
      });
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : String(err);
      vscode.window.showErrorMessage(`Could not open file: ${message}`);
    }
  };

  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(SidebarProvider.viewId, sidebar, {
      webviewOptions: { retainContextWhenHidden: true },
    }),
    vscode.commands.registerCommand("stateroot.openWorkbench", (tab?: string) => {
      if (typeof tab === "string") {
        selectedTab = tab;
      }
      workbench.reveal(selectedTab);
      push();
    }),
    vscode.commands.registerCommand("stateroot.delegatePlan", async () => {
      await withProject(async (root) => {
        const plans = listPlans(root);
        const picked = await vscode.window.showQuickPick(
          plans.map((p) => ({ label: p.title, description: p.status, id: p.id })),
          { placeHolder: "Plan to delegate" }
        );
        if (picked && "id" in picked) {
          await delegatePlan(picked.id as string);
        }
      });
    }),
    vscode.commands.registerCommand("stateroot.init", async () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      if (!folder) {
        vscode.window.showErrorMessage("Open a folder first.");
        return;
      }
      await runSetup();
      if (setup.phase === "error") return;
      const initialized = await runCliReport(["init"], folder.uri.fsPath, output, 60_000);
      if (initialized !== undefined) {
        await maybeOfferCopilotHooks(context, folder.uri.fsPath, output);
        void vscode.commands.executeCommand("stateroot.overview.focus");
      }
      push();
    }),
    vscode.commands.registerCommand("stateroot.refresh", () => {
      void refreshLive();
    }),
    vscode.commands.registerCommand("stateroot.checkpoint", () =>
      withProject(async (root) => {
        const note = await vscode.window.showInputBox({
          prompt: "Checkpoint note — what changed and why?",
        });
        if (!note?.trim()) {
          return;
        }
        await runCliReport(["checkpoint", "--note", note.trim()], root, output);
        push();
      })
    ),
    vscode.commands.registerCommand("stateroot.snap", () =>
      withProject(async (root) => {
        await runCliReport(["snap"], root, output, 120_000);
        push();
      })
    ),
    vscode.commands.registerCommand("stateroot.resume", () =>
      withProject(async (root) => {
        await runCliReport(["resume", "--harness", THIS_HARNESS, "--force"], root, output);
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
          THIS_HARNESS,
          "--objective",
          objective.trim(),
          "--task",
          task.trim(),
          "--context-summary",
          "written from the VS Code extension",
        ];
        if (next?.trim()) {
          args.push("--next", next.trim());
        }
        await runCliReport(args, root, output);
        push();
      })
    ),
    vscode.commands.registerCommand("stateroot.doctor", () =>
      withProject(async (root) => {
        await runCliReport(["doctor"], root, output);
        output.show(true);
      })
    ),
    vscode.commands.registerCommand("stateroot.installCli", async () => {
      await runSetup(true);
    }),
    vscode.commands.registerCommand("stateroot.retrySetup", () => runSetup(true)),
    vscode.commands.registerCommand("stateroot.openSetup", () =>
      vscode.commands.executeCommand("stateroot.overview.focus"))
  );

  context.subscriptions.push(
    vscode.commands.registerCommand("stateroot.enableCopilotHooks", () => {
      const folder = vscode.workspace.workspaceFolders?.[0];
      if (!folder) {
        void vscode.window.showInformationMessage(
          "Open a project folder first — Copilot hooks live in the workspace."
        );
        return;
      }
      if (!isVSCodeWithCopilot()) {
        void vscode.window.showInformationMessage(
          "Copilot hooks are only available in VS Code with the Copilot Chat extension installed."
        );
        return;
      }
      enableCopilotHooks(folder.uri.fsPath, output);
    })
  );

  const watcher = vscode.workspace.createFileSystemWatcher(`**/${STORE}/**`);
  let timer: NodeJS.Timeout | undefined;
  const debounced = () => {
    clearTimeout(timer);
    timer = setTimeout(() => {
      void refreshLive();
    }, 400);
  };
  watcher.onDidCreate(debounced);
  watcher.onDidChange(debounced);
  watcher.onDidDelete(debounced);
  context.subscriptions.push(watcher, status, output, {
    dispose: () => {
      if (poll) {
        clearInterval(poll);
      }
      if (storePoll) {
        clearInterval(storePoll);
      }
    },
  });

  // Workspace watchers can miss writes under hidden/ignored directories,
  // especially when Windows and WSL are on opposite sides of the workspace.
  // Keep the read-only StateRoot view eventually consistent without invoking
  // the CLI or requiring a manual refresh.
  storePoll = setInterval(push, 3000);
  function runSetup(retry = false): Promise<void> {
    if (setupPending) return setupPending;
    setupPending = (async () => {
      try {
        setup = { phase: "checking", detail: "Checking StateRoot setup…" };
        push();
        const available = await refreshCliProbe();
        const folder = vscode.workspace.workspaceFolders?.[0];
        const binary = await ensureSetup({
          state: context.globalState,
          extensionVersion: String(context.extension.packageJSON.version),
          binary: available ? cliPath() : undefined,
          noAutoUpdate: !!process.env.STATEROOT_NO_AUTO_UPDATE,
          retry,
          install: async () => {
            const { installCli } = await import("./cliInstall");
            const result = await installCli(output);
            if (!result.ok) throw new Error(result.error);
            useCli(result.binaryPath);
            return result.binaryPath;
          },
          run: async (args, selected) => {
            const text = await runCli(args, folder?.uri.fsPath || path.dirname(context.extensionPath),
              600_000, selected);
            output.appendLine(`$ ${selected} ${args.join(" ")}`);
            output.appendLine(text);
            return text;
          },
          report: (state) => { setup = state; push(); },
        });
        useCli(binary);
        if ((!available || retry) && folder && !fs.existsSync(path.join(folder.uri.fsPath, STORE, "manifest.json"))) {
          const result = await runCliReport(["init"], folder.uri.fsPath, output, 60_000);
          if (result === undefined) throw new Error("Project initialization failed. Retry setup or initialize the project again.");
          void vscode.commands.executeCommand("stateroot.overview.focus");
        }
        if (folder) void maybeOfferCopilotHooks(context, folder.uri.fsPath, output);
      } catch (err) {
        const detail = err instanceof Error ? err.message : String(err);
        setup = { phase: "error", detail };
        output.appendLine(`Setup incomplete: ${detail}`);
        void vscode.window.showErrorMessage(`StateRoot setup incomplete: ${detail}`, "Retry setup", "Show details")
          .then(pick => {
            if (pick === "Retry setup") void runSetup(true);
            if (pick === "Show details") output.show(true);
          });
      } finally {
        push();
      }
    })().finally(() => { setupPending = undefined; });
    return setupPending;
  }
  void runSetup();
  void refreshLive();
}

async function gitShow(
  root: string,
  commit: string,
  filePath: string
): Promise<vscode.Uri | undefined> {
  const result = spawnSync("git", ["-C", root, "show", `${commit}:${filePath}`], {
    encoding: "utf8",
    maxBuffer: 4 * 1024 * 1024,
  });
  if (result.status !== 0) {
    return undefined;
  }
  const doc = await vscode.workspace.openTextDocument({
    content: result.stdout,
    language: languageFor(filePath),
  });
  return doc.uri;
}

function languageFor(filePath: string): string {
  const ext = path.extname(filePath).toLowerCase();
  const map: Record<string, string> = {
    ".ts": "typescript",
    ".tsx": "typescriptreact",
    ".js": "javascript",
    ".rs": "rust",
    ".md": "markdown",
    ".json": "json",
    ".py": "python",
  };
  return map[ext] || "plaintext";
}

export function deactivate() {
  // subscriptions dispose the rest
}
