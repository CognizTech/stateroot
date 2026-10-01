import * as vscode from "vscode";
import { glanceHtml, nonce } from "./ui";
import type { Snapshot } from "./snapshot";
import type { SetupState } from "./setup";

export class SidebarProvider implements vscode.WebviewViewProvider {
  public static readonly viewId = "stateroot.overview";
  private view?: vscode.WebviewView;

  constructor(
    private readonly extensionUri: vscode.Uri,
    private readonly onMessage: (msg: Record<string, unknown>) => void
  ) {}

  resolveWebviewView(webviewView: vscode.WebviewView): void {
    this.view = webviewView;
    webviewView.webview.options = { enableScripts: true, localResourceRoots: [this.extensionUri] };
    webviewView.webview.html = glanceHtml(nonce());
    webviewView.webview.onDidReceiveMessage((msg) => this.onMessage(msg));
  }

  post(state: (Snapshot | { initialized: false }) & { setup?: SetupState }): void {
    const needs = "initialized" in state && state.initialized ? state.inbox.length : 0;
    if (this.view) {
      this.view.badge = needs
        ? { value: needs, tooltip: `${needs} ${needs === 1 ? "item needs" : "items need"} attention` }
        : undefined;
    }
    this.view?.webview.postMessage(state);
  }
}
