import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import {
  api,
  CycleReport,
  errorText,
  InputDevice,
  PermissionStatus,
  Progress,
  SettingsView,
  TargetWindow,
} from "./api";
import { ChatLog, ChatMessage } from "./components/ChatLog";
import { pickDevice } from "./devices";
import { allGranted, PermissionDots, PermissionPanel } from "./components/PermissionPanel";
import { ReferenceDrop } from "./components/ReferenceDrop";
import { SettingsPanel } from "./components/SettingsPanel";
import "./App.css";

const SHORTCUT = "CommandOrControl+Alt+H";
const SHORTCUT_LABEL = "⌘⌥H";

// 画面の好みだけを覚える。消えても困らないので失敗は無視する
function loadPref(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function savePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* 保存できなくても動作には影響しない */
  }
}

let nextId = 1;

export default function App() {
  const [devices, setDevices] = useState<InputDevice[]>([]);
  const [deviceId, setDeviceId] = useState<string | null>(null);
  const [seconds, setSeconds] = useState<number>(() => (loadPref("seconds") === "3" ? 3 : 5));
  const [onTop, setOnTop] = useState<boolean>(() => loadPref("onTop") !== "false");
  const [permissions, setPermissions] = useState<PermissionStatus | null>(null);
  const [settings, setSettings] = useState<SettingsView | null>(null);
  const [target, setTarget] = useState<TargetWindow | null>(null);
  const [targetError, setTargetError] = useState<string | null>(null);
  const [panel, setPanel] = useState<"none" | "permissions" | "settings">("none");
  const [instruction, setInstruction] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [progress, setProgress] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [undoDepth, setUndoDepth] = useState(0);

  const push = useCallback((m: Omit<ChatMessage, "id">) => {
    setMessages((prev) => [...prev, { ...m, id: nextId++ }]);
  }, []);
  const pushError = useCallback((text: string) => push({ role: "error", text }), [push]);

  const refreshPermissions = useCallback(async () => {
    try {
      setPermissions(await api.permissionStatus());
    } catch (e) {
      pushError(errorText(e));
    }
  }, [pushError]);

  const refreshDevices = useCallback(async () => {
    try {
      const list = await api.listInputDevices();
      setDevices(list);
      setDeviceId((cur) => pickDevice(list, cur ?? loadPref("deviceId")));
    } catch (e) {
      pushError(errorText(e));
    }
  }, [pushError]);

  const refreshTarget = useCallback(async () => {
    try {
      setTarget(await api.findTargetWindow());
      setTargetError(null);
    } catch (e) {
      setTarget(null);
      setTargetError(errorText(e));
    }
  }, []);

  // 起動時
  useEffect(() => {
    (async () => {
      await refreshPermissions();
      await refreshDevices();
      // 画面を読み込み直しても、Rust 側に残っている Undo 履歴を使えるようにする
      api.undoDepth().then(setUndoDepth).catch((e) => pushError(errorText(e)));
      try {
        const s = await api.getSettings();
        setSettings(s);
        if (s.api_key_source === "missing") setPanel("settings");
      } catch (e) {
        pushError(errorText(e));
      }
    })();
  }, [refreshPermissions, refreshDevices, pushError]);

  // 権限が揃っていなければ最初に案内する
  useEffect(() => {
    if (permissions && !allGranted(permissions) && panel === "none" && messages.length === 0) {
      setPanel("permissions");
    }
    // 起動直後の 1 回だけでよい
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [permissions]);

  // ウィンドウに戻ってきたら権限を確かめ直す (システム設定で許可した後など)
  useEffect(() => {
    const onFocus = () => {
      refreshPermissions();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshPermissions]);

  // HX Edit の検出状態を定期的に見る
  useEffect(() => {
    if (busy) return;
    refreshTarget();
    const t = setInterval(refreshTarget, 5000);
    return () => clearInterval(t);
  }, [busy, refreshTarget, settings]);

  useEffect(() => {
    getCurrentWindow()
      .setAlwaysOnTop(onTop)
      .catch((e) => pushError(`常に手前に表示を切り替えられません: ${errorText(e)}`));
    savePref("onTop", String(onTop));
  }, [onTop, pushError]);

  useEffect(() => {
    if (deviceId) savePref("deviceId", deviceId);
  }, [deviceId]);
  useEffect(() => savePref("seconds", String(seconds)), [seconds]);

  useEffect(() => {
    const un = listen<Progress>("progress", (e) => {
      setProgress(e.payload.stage === "done" ? null : e.payload.message);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const showReport = useCallback(
    (r: CycleReport, kind: "adjust" | "undo") => {
      const meta: string[] = [];
      if (r.recording_peak_dbfs !== null) meta.push(`録音ピーク ${r.recording_peak_dbfs.toFixed(1)} dBFS`);
      if (r.rounds > 1) meta.push(`撮影 ${r.rounds} 回`);
      const hasResult = r.explanations.length > 0 || r.changes.length > 0 || r.not_restored.length > 0;
      // AI に届く前に止まったときは、エラーだけを出す
      if (hasResult || !r.error) {
        const text =
          r.explanations.join("\n\n") ||
          (kind === "undo" ? "直前の変更を元に戻した。" : "変更はなかった。");
        push({
          role: "assistant",
          text,
          changes: r.changes,
          notRestored: r.not_restored,
          meta: meta.join(" · ") || undefined,
        });
      }
      if (r.error) {
        push({
          role: "error",
          text:
            r.changes.length > 0 && kind === "adjust"
              ? `${r.error}\n(ここまでの ${r.changes.length} 件の変更は「元に戻す」で戻せる)`
              : r.error,
        });
      }
      setUndoDepth(r.undo_depth);
    },
    [push],
  );

  const runCycle = useCallback(async () => {
    if (busy) return;
    const text = instruction.trim();
    setBusy(true);
    push({ role: "user", text: text || "(指示なし)" });
    setInstruction("");
    try {
      showReport(await api.runCycle(text, deviceId, seconds), "adjust");
    } catch (e) {
      pushError(errorText(e));
    } finally {
      setProgress(null);
      setBusy(false);
      api.undoDepth().then(setUndoDepth).catch(() => {});
    }
  }, [busy, instruction, deviceId, seconds, push, pushError, showReport]);

  const undo = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    push({ role: "system", text: "1 つ前の状態に戻す" });
    try {
      showReport(await api.undoLast(), "undo");
    } catch (e) {
      pushError(errorText(e));
    } finally {
      setProgress(null);
      setBusy(false);
      api.undoDepth().then(setUndoDepth).catch(() => {});
    }
  }, [busy, push, pushError, showReport]);

  // グローバルショートカットからも同じ処理を呼ぶ。登録は一度だけにし、最新の関数は ref で引く
  const runRef = useRef(runCycle);
  runRef.current = runCycle;
  useEffect(() => {
    let registered = false;
    register(SHORTCUT, (e) => {
      if (e.state === "Pressed") runRef.current();
    })
      .then(() => (registered = true))
      .catch((e) => pushError(`ショートカット ${SHORTCUT_LABEL} を登録できません: ${errorText(e)}`));
    return () => {
      if (registered) unregister(SHORTCUT).catch(() => {});
    };
  }, [pushError]);

  const clearChat = async () => {
    try {
      await api.clearConversation();
      setMessages([]);
    } catch (e) {
      pushError(errorText(e));
    }
  };

  const togglePanel = (p: "permissions" | "settings") => setPanel((cur) => (cur === p ? "none" : p));

  return (
    <div className="app">
      <header className="header">
        <div className="brand">HX AI</div>
        <div className="header-actions">
          <PermissionDots status={permissions} onClick={() => togglePanel("permissions")} />
          <button
            className={`icon-btn ${onTop ? "active" : ""}`}
            onClick={() => setOnTop((v) => !v)}
            title={onTop ? "常に手前に表示: オン" : "常に手前に表示: オフ"}
          >
            📌
          </button>
          <button
            className={`icon-btn ${panel === "settings" ? "active" : ""}`}
            onClick={() => togglePanel("settings")}
            title="設定"
          >
            ⚙︎
          </button>
        </div>
      </header>

      {panel === "permissions" && (
        <PermissionPanel status={permissions} onRefresh={refreshPermissions} onError={pushError} />
      )}
      {panel === "settings" && (
        <SettingsPanel
          view={settings}
          onSaved={(v) => {
            setSettings(v);
            push({ role: "system", text: `設定を保存した (モデル: ${v.effective_model})` });
          }}
          onError={pushError}
        />
      )}

      <section className="controls">
        <div className="row">
          <select
            className="device"
            value={deviceId ?? ""}
            onChange={(e) => setDeviceId(e.target.value || null)}
            disabled={busy}
            title="オーディオ入力"
          >
            {devices.length === 0 && <option value="">入力デバイスがありません</option>}
            {devices.map((d) => (
              <option key={d.id} value={d.id}>
                🎸 {d.name}
              </option>
            ))}
          </select>
          <button className="icon-btn" onClick={refreshDevices} disabled={busy} title="デバイスを再読込">
            ↻
          </button>
        </div>
        <div className="row">
          <div className="segmented" role="radiogroup" aria-label="録音時間">
            {[3, 5].map((s) => (
              <button
                key={s}
                role="radio"
                aria-checked={seconds === s}
                className={seconds === s ? "on" : ""}
                onClick={() => setSeconds(s)}
                disabled={busy}
              >
                {s} 秒
              </button>
            ))}
          </div>
          <div className={`target ${target ? "ok" : "ng"}`} title={targetError ?? target?.title ?? ""}>
            {target
              ? `${target.app_name} ${Math.round(target.rect.width)}×${Math.round(target.rect.height)}`
              : `${settings?.effective_target_app ?? "HX Edit"} 未検出`}
          </div>
        </div>
        <ReferenceDrop disabled={busy} onError={pushError} />
      </section>

      <ChatLog messages={messages} progress={progress} />

      <footer className="composer">
        <textarea
          value={instruction}
          onChange={(e) => setInstruction(e.target.value)}
          placeholder="例: もっとハイを削って太く / ソロ用にディレイを足して"
          rows={2}
          disabled={busy}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              runCycle();
            }
          }}
        />
        <div className="row">
          <button className="primary grow" onClick={runCycle} disabled={busy}>
            {busy ? "処理中…" : `🎧 聴いて調整する`}
          </button>
          <button className="secondary" onClick={undo} disabled={busy || undoDepth === 0} title="1 つ前の状態に戻す">
            ↶ 戻す{undoDepth > 0 ? ` (${undoDepth})` : ""}
          </button>
        </div>
        <div className="hint muted small">
          <span>
            {SHORTCUT_LABEL} でどこからでも実行 · ⌘↩ で送信
          </span>
          {messages.length > 0 && (
            <button className="link" onClick={clearChat} disabled={busy}>
              会話をリセット
            </button>
          )}
        </div>
      </footer>
    </div>
  );
}
