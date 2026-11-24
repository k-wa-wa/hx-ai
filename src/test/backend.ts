// テスト用の偽のバックエンド。Rust のコマンドを台本どおりに返し、呼ばれた内容を記録する。

import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";
import type { InvokeArgs } from "@tauri-apps/api/core";
import type {
  CycleReport,
  HxStatus,
  InputDevice,
  PermissionStatus,
  SettingsView,
} from "../api";

export const DEVICES: InputDevice[] = [
  { id: "coreaudio:builtin", name: "MacBook Pro のマイク", is_default: true },
  { id: "coreaudio:stomp", name: "HX Stomp", is_default: false },
];

export const GRANTED: PermissionStatus = {
  microphone: "granted",
};

export const SETTINGS: SettingsView = {
  stored: { api_base_url: "", model: "", has_api_key: false },
  effective_base_url: "https://example.test/v1/",
  effective_model: "gemini-3.8-flash",
  api_key_source: "env",
};

export const HX_CONNECTED: HxStatus = {
  connected: true,
  device_name: "HX Stomp XL",
  session_open: false,
};

export function report(overrides: Partial<CycleReport> = {}): CycleReport {
  return {
    explanations: ["高域を下げた"],
    changes: [],
    error: null,
    recording_peak_dbfs: -12.3,
    undo_depth: 0,
    not_restored: [],
    goal_reached: false,
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
    hx_status: () => HX_CONNECTED,
    disconnect_hx: () => HX_CONNECTED,
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
