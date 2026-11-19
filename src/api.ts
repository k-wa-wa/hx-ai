// Rust 側のコマンドを型付きで呼ぶ薄いラッパー。

import { invoke } from "@tauri-apps/api/core";

export type PermissionKind = "microphone" | "screen_recording" | "accessibility";
export type PermissionState = "granted" | "denied" | "not_determined";

export interface PermissionStatus {
  microphone: PermissionState;
  screen_recording: PermissionState;
  accessibility: PermissionState;
}

export interface InputDevice {
  id: string;
  name: string;
  is_default: boolean;
}

export interface SettingsView {
  stored: {
    api_base_url: string;
    model: string;
    target_app: string;
    has_api_key: boolean;
  };
  effective_base_url: string;
  effective_model: string;
  effective_target_app: string;
  api_key_source: "settings" | "env" | "missing";
}

export interface SettingsInput {
  apiBaseUrl: string;
  /** null なら保存済みのキーを変えない。空文字なら消す。 */
  apiKey: string | null;
  model: string;
  targetApp: string;
}

export interface TargetWindow {
  window_id: number;
  pid: number;
  app_name: string;
  title: string;
  rect: { x: number; y: number; width: number; height: number };
}

export interface ReferenceInfo {
  name: string;
  mime: string;
  size: number;
}

export interface DoneAction {
  kind: "set_value" | "select_block";
  block: string;
  parameter: string;
  value: string;
  previous_value: string;
}

export interface RestoreTarget {
  block: string;
  parameter: string;
  value: string;
}

export interface CycleReport {
  explanations: string[];
  changes: DoneAction[];
  error: string | null;
  recording_peak_dbfs: number | null;
  rounds: number;
  undo_depth: number;
  not_restored: RestoreTarget[];
}

export interface Progress {
  stage: "record" | "capture" | "ai" | "operate" | "done";
  message: string;
}

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  saveSettings: (input: SettingsInput) => invoke<SettingsView>("save_settings", { input }),
  listInputDevices: () => invoke<InputDevice[]>("list_input_devices"),
  permissionStatus: () => invoke<PermissionStatus>("permission_status"),
  requestPermission: (kind: PermissionKind) => invoke<void>("request_permission", { kind }),
  openPermissionSettings: (kind: PermissionKind) =>
    invoke<void>("open_permission_settings", { kind }),
  findTargetWindow: () => invoke<TargetWindow>("find_target_window"),
  setReference: (path: string) => invoke<ReferenceInfo>("set_reference", { path }),
  getReferenceAudio: () => invoke<ArrayBuffer>("get_reference_audio"),
  clearReference: () => invoke<void>("clear_reference"),
  undoDepth: () => invoke<number>("undo_depth"),
  clearConversation: () => invoke<void>("clear_conversation"),
  runCycle: (instruction: string, deviceId: string | null, seconds: number) =>
    invoke<CycleReport>("run_cycle", { instruction, deviceId, seconds }),
  undoLast: () => invoke<CycleReport>("undo_last"),
};

/** invoke の失敗は文字列で届く。念のため Error なども文字列にする。 */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
