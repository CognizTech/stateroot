import type * as vscode from "vscode";
import { osTarget } from "./installPing";

export const EDITOR_ID_KEY = "stateroot.editorId";
export const QUEUE_KEY = "stateroot.editorTelemetryQueue";
export const TELEMETRY_URL = "https://stateroot.dev/api/telemetry/v2/editor";

export type CliStatus = "missing" | "unrunnable" | "working";
export type PathClass = "default_stable" | "custom" | "cargo" | "nightly" | "unknown";
export type ProfileClass =
  | "verified_legacy"
  | "existing_cli"
  | "verified_project"
  | "unknown_first_seen";
export type SetupResult = "ready" | "failed";
export type FailureStage =
  | "unsupported_platform"
  | "install_failed"
  | "update_failed"
  | "integration_failed"
  | "identity_link_failed"
  | "project_init_failed"
  | "cli_unrunnable"
  | "timeout"
  | "unknown";
export type EditorHost = "vscode" | "cursor";

export interface EditorEvent {
  schema_version: 2;
  event_id: string;
  editor_id: string;
  event: "editor_seen" | "setup_started" | "setup_finished";
  occurred_on: string;
  ext_version: string;
  host: EditorHost;
  os_arch: string;
  profile_class?: ProfileClass;
  cli_status?: CliStatus;
  path_class?: PathClass;
  result?: SetupResult;
  stage?: FailureStage;
  install_id?: string;
}

export interface Preflight {
  previousVersion?: string;
  previousReceipt?: { extensionVersion?: string; binary?: string; version?: string };
  host: EditorHost;
  workspaceInitialized: boolean;
  cliStatus: CliStatus;
  cliVersion?: string;
  pathClass: PathClass;
  profileClass: ProfileClass;
}

const MAX_QUEUE = 50;

export function editorHost(appName: string): EditorHost {
  return /cursor/i.test(appName) ? "cursor" : "vscode";
}

export function classifyPath(binary: string | undefined, defaultDest?: string): PathClass {
  if (!binary) return "unknown";
  const normalized = binary.replace(/\\/g, "/");
  if (defaultDest && binary === defaultDest) return "default_stable";
  if (normalized.includes("/.cargo/bin/") || /\/cargo\/bin\//.test(normalized)) return "cargo";
  if (/nightly/i.test(normalized)) return "nightly";
  if (defaultDest && binary !== defaultDest) return "custom";
  return "custom";
}

export function classifyProfile(preflight: {
  previousVersion?: string;
  previousReceipt?: { extensionVersion?: string };
  workspaceInitialized: boolean;
  cliStatus: CliStatus;
}): ProfileClass {
  if (preflight.previousVersion || preflight.previousReceipt) return "verified_legacy";
  if (preflight.workspaceInitialized) return "verified_project";
  if (preflight.cliStatus === "working") return "existing_cli";
  return "unknown_first_seen";
}

export function todayUtc(now = new Date()): string {
  return now.toISOString().slice(0, 10);
}

function newId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (c) => {
    const r = (Math.random() * 16) | 0;
    const v = c === "x" ? r : (r & 0x3) | 0x8;
    return v.toString(16);
  });
}

export async function getOrCreateEditorId(state: vscode.Memento): Promise<string> {
  const existing = state.get<string>(EDITOR_ID_KEY);
  if (existing) return existing;
  const id = newId();
  await state.update(EDITOR_ID_KEY, id);
  return id;
}

export function eventHeaders(event: EditorEvent): Record<string, string> {
  const headers: Record<string, string> = {
    "x-sr-schema": "2",
    "x-sr-event-id": event.event_id,
    "x-sr-editor-id": event.editor_id,
    "x-sr-event": event.event,
    "x-sr-occurred-on": event.occurred_on,
    "x-sr-extension-version": event.ext_version,
    "x-sr-editor-host": event.host,
    "x-sr-os-arch": event.os_arch,
  };
  if (event.profile_class) headers["x-sr-profile-class"] = event.profile_class;
  if (event.cli_status) headers["x-sr-cli-status"] = event.cli_status;
  if (event.path_class) headers["x-sr-path-class"] = event.path_class;
  if (event.result) headers["x-sr-result"] = event.result;
  if (event.stage) headers["x-sr-stage"] = event.stage;
  if (event.install_id) headers["x-sr-install-id"] = event.install_id;
  return headers;
}

export async function enqueue(state: vscode.Memento, event: EditorEvent): Promise<void> {
  if (process.env.STATEROOT_NO_PING) return;
  const queue = state.get<EditorEvent[]>(QUEUE_KEY, []);
  if (queue.some((item) => item.event_id === event.event_id)) return;
  queue.push(event);
  while (queue.length > MAX_QUEUE) queue.shift();
  await state.update(QUEUE_KEY, queue);
}

export async function ack(state: vscode.Memento, eventId: string): Promise<void> {
  const queue = state.get<EditorEvent[]>(QUEUE_KEY, []).filter((item) => item.event_id !== eventId);
  await state.update(QUEUE_KEY, queue);
}

export async function flushQueue(
  state: vscode.Memento,
  fetchImpl: typeof fetch = fetch,
  url = process.env.STATEROOT_TELEMETRY_URL || TELEMETRY_URL
): Promise<void> {
  if (process.env.STATEROOT_NO_PING) return;
  const queue = state.get<EditorEvent[]>(QUEUE_KEY, []);
  for (const event of queue) {
    try {
      const controller = new AbortController();
      const timer = setTimeout(() => controller.abort(), 3000);
      const response = await fetchImpl(url, {
        method: "POST",
        headers: eventHeaders(event),
        body: "",
        signal: controller.signal,
      });
      clearTimeout(timer);
      if (response.ok) await ack(state, event.event_id);
    } catch {
      // retry on next activation
    }
  }
}

export function buildEvent(
  editorId: string,
  kind: EditorEvent["event"],
  extVersion: string,
  host: EditorHost,
  extras: Partial<EditorEvent> = {}
): EditorEvent {
  return {
    schema_version: 2,
    event_id: newId(),
    editor_id: editorId,
    event: kind,
    occurred_on: todayUtc(),
    ext_version: extVersion,
    host,
    os_arch: osTarget(process.platform, process.arch),
    ...extras,
  };
}

export function parseInstallId(raw: string): string | undefined {
  try {
    const parsed = JSON.parse(raw) as { install_id?: unknown };
    return typeof parsed.install_id === "string" && parsed.install_id
      ? parsed.install_id
      : undefined;
  } catch {
    return undefined;
  }
}

export function capturePreflight(input: {
  previousVersion?: string;
  previousReceipt?: Preflight["previousReceipt"];
  host: EditorHost;
  workspaceInitialized: boolean;
  cliStatus: CliStatus;
  cliVersion?: string;
  binary?: string;
  defaultDest?: string;
}): Preflight {
  const pathClass = classifyPath(input.binary, input.defaultDest);
  const profileClass = classifyProfile(input);
  return {
    previousVersion: input.previousVersion,
    previousReceipt: input.previousReceipt,
    host: input.host,
    workspaceInitialized: input.workspaceInitialized,
    cliStatus: input.cliStatus,
    cliVersion: input.cliVersion,
    pathClass,
    profileClass,
  };
}
