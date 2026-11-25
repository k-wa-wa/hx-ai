// Rust 側のコマンドを型付きで呼ぶ薄いラッパー。

import { invoke } from "@tauri-apps/api/core";

export type PermissionKind = "microphone";
export type PermissionState = "granted" | "denied" | "not_determined";

export interface PermissionStatus {
  microphone: PermissionState;
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
    has_api_key: boolean;
  };
  effective_base_url: string;
  effective_model: string;
  api_key_source: "settings" | "env" | "missing";
}

export interface SettingsInput {
  apiBaseUrl: string;
  /** null なら保存済みのキーを変えない。空文字なら消す。 */
  apiKey: string | null;
  model: string;
}

export interface HxStatus {
  connected: boolean;
  device_name: string | null;
  session_open: boolean;
}

export interface ParamUiView {
  name: string;
  value: string;
  kind: "number" | "choice" | "readonly";
  min: string | null;
  max: string | null;
  locked: boolean;
}

export interface PairedCabUiView {
  name: string;
  params: ParamUiView[];
}

export interface BlockUiView {
  id: string;
  slot: number;
  name: string;
  category: string;
  colorHex: string;
  enabled: boolean;
  locked: boolean;
  lockReason: string | null;
  pairedCab?: PairedCabUiView | null;
  readonlyNotes: string[];
  params: ParamUiView[];
}

export interface ChainView {
  slotLabel: string;
  presetName: string;
  tempo: number | null;
  snapshots: string[];
  freeBlockSlots: number;
  blocks: BlockUiView[];
}

export interface ReferenceInfo {
  name: string;
  mime: string;
  size: number;
  /** 音源全体の長さ (秒)。 */
  duration: number;
  /** AI に送る区間の開始位置と長さ (秒)。 */
  clipStart: number;
  clipSeconds: number;
}

export interface DoneAction {
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
  undo_depth: number;
  not_restored: RestoreTarget[];
  /** AI が「目標に届いた」と判断したか。 */
  goal_reached: boolean;
  /** 送った録音が使われずに残っているか。 */
  recording_kept: boolean;
}

export interface RecordingInfo {
  seconds: number;
  peak_dbfs: number;
}

export interface LoopSummary {
  iterations: number;
  reason: string;
  undo_depth: number;
}

/** 自動ループの 1 回分の結果 (イベント "loop_iteration")。 */
export interface LoopIteration {
  iteration: number;
  max: number;
  report: CycleReport;
}

export interface Progress {
  stage: "record" | "capture" | "ai" | "operate" | "done";
  message: string;
}

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  openSettingsWindow: () => invoke<void>("open_settings_window"),
  saveSettings: (input: SettingsInput) => invoke<SettingsView>("save_settings", { input }),
  listInputDevices: () => invoke<InputDevice[]>("list_input_devices"),
  permissionStatus: () => invoke<PermissionStatus>("permission_status"),
  /** 許可を求める。一度拒否されているなどダイアログが出ないときはシステム設定を開く。 */
  grantPermission: (kind: PermissionKind) => invoke<void>("grant_permission", { kind }),
  hxStatus: () => invoke<HxStatus>("hx_status"),
  getChain: () => invoke<ChainView>("get_chain"),
  disconnectHx: () => invoke<HxStatus>("disconnect_hx"),
  setReference: (path: string) => invoke<ReferenceInfo>("set_reference", { path }),
  setReferenceStart: (seconds: number) => invoke<ReferenceInfo>("set_reference_start", { seconds }),
  getReferenceAudio: () => invoke<ArrayBuffer>("get_reference_audio"),
  clearReference: () => invoke<void>("clear_reference"),
  undoDepth: () => invoke<number>("undo_depth"),
  clearConversation: () => invoke<void>("clear_conversation"),
  startRecording: (deviceId: string | null) => invoke<void>("start_recording", { deviceId }),
  stopRecording: () => invoke<RecordingInfo>("stop_recording"),
  discardRecording: () => invoke<void>("discard_recording"),
  sendMessage: (instruction: string) => invoke<CycleReport>("send_message", { instruction }),
  startLoop: (instruction: string, deviceId: string | null, seconds: number, maxIterations: number) =>
    invoke<LoopSummary>("start_loop", { instruction, deviceId, seconds, maxIterations }),
  stopLoop: () => invoke<void>("stop_loop"),
  addLoopInstruction: (text: string) => invoke<void>("add_loop_instruction", { text }),
  undoLast: () => invoke<CycleReport>("undo_last"),
};

/** invoke の失敗は文字列で届く。念のため Error なども文字列にする。 */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return JSON.stringify(e);
}
