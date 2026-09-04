import * as vscode from "vscode";
import { workbenchHtml, nonce } from "./ui";
import type { Snapshot } from "./snapshot";

export class WorkbenchPanel {
  public static readonly viewType = "stateroot.workbench";
  private panel?: vscode.WebviewPanel;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly onMessage: (msg: Record<string, unknown>) => void
  ) {}

  reveal(tab?: string): vscode.WebviewPanel {
    if (this.panel) {
      this.panel.reveal(vscode.ViewColumn.One);
      if (tab) {
        this.panel.webview.postMessage({ tab });
      }
      return this.panel;
    }
    const panel = vscode.window.createWebviewPanel(
      WorkbenchPanel.viewType,
      "StateRoot",
      vscode.ViewColumn.One,
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [this.extensionUri] }
    );
    panel.webview.onDidReceiveMessage((msg) => this.onMessage(msg));
    panel.webview.html = workbenchHtml(nonce());
    panel.onDidDispose(() => {
      this.panel = undefined;
    });
    this.panel = panel;
    if (tab) {
      void panel.webview.postMessage({ tab });
    }
    return panel;
  }

  post(state: Snapshot | { initialized: false }): void {
    this.panel?.webview.postMessage(state);
  }

  get visible(): boolean {
    return !!this.panel;
  }
}
