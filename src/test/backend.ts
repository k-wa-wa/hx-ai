// テスト用の偽のバックエンド。Rust のコマンドを台本どおりに返し、呼ばれた内容を記録する。

import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { InvokeArgs } from "@tauri-apps/api/core";
import type {
  CycleReport,
  InputDevice,
  PermissionStatus,
  SettingsView,
  TargetWindow,
} from "../api";

export const DEVICES: InputDevice[] = [
  { id: "coreaudio:builtin", name: "MacBook Pro のマイク", is_default: true },
  { id: "coreaudio:stomp", name: "HX Stomp", is_default: false },
];

export const GRANTED: PermissionStatus = {
  microphone: "granted",
  screen_recording: "granted",
  accessibility: "granted",
};

export const SETTINGS: SettingsView = {
  stored: { device: "", api_base_url: "", model: "", target_app: "", has_api_key: false },
  device: "hx_stomp",
  effective_base_url: "https://example.test/v1/",
  effective_model: "gemini-3.8-flash",
  effective_target_app: "HX Edit",
  api_key_source: "env",
};

export const WINDOW: TargetWindow = {
  window_id: 1,
  pid: 2,
  app_name: "HX Edit",
  title: "HX Edit",
  rect: { x: 0, y: 0, width: 1200, height: 800 },
};

export function report(overrides: Partial<CycleReport> = {}): CycleReport {
  return {
    explanations: ["高域を下げた"],
    changes: [],
    error: null,
    recording_peak_dbfs: -12.3,
    rounds: 1,
    undo_depth: 0,
    not_restored: [],
    goal_reached: false,
    interrupted: false,
    recording_kept: false,
    ...overrides,
  };
}

type Handler = (args: InvokeArgs | undefined) => unknown;

export interface Backend {
  calls: { cmd: string; args: InvokeArgs | undefined }[];
  callsTo(cmd: string): (InvokeArgs | undefined)[];
}

/**
 * 偽のバックエンドを立てる。`handlers` で既定の応答を上書きする。
 * アプリのコマンドで台本に無いものは例外にする (コマンド名の打ち間違いに気づくため)。
 */
export function setupBackend(handlers: Record<string, Handler> = {}): Backend {
  const defaults: Record<string, Handler> = {
    get_settings: () => SETTINGS,
    list_input_devices: () => DEVICES,
    permission_status: () => GRANTED,
    find_target_window: () => WINDOW,
    undo_depth: () => 0,
  };
  const all = { ...defaults, ...handlers };
  const calls: Backend["calls"] = [];
  mockWindows("main");
  mockIPC(
    (cmd, args) => {
      calls.push({ cmd, args });
      if (cmd in all) return all[cmd](args);
      // Tauri 本体やプラグインの内部コマンド (ウィンドウ操作・ショートカット登録など)
      if (cmd.startsWith("plugin:")) return null;
      throw new Error(`台本に無いコマンド: ${cmd}`);
    },
    { shouldMockEvents: true },
  );
  return {
    calls,
    callsTo: (cmd) => calls.filter((c) => c.cmd === cmd).map((c) => c.args),
  };
}
