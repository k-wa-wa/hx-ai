import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  ChainView,
  CycleReport,
  DoneAction,
  errorText,
  HxStatus,
  InputDevice,
  LoopIteration,
  Progress,
  RecordingInfo,
} from "./api";
import { ChainOverview } from "./components/ChainOverview";
import { ChatLog, ChatMessage } from "./components/ChatLog";
import { pickDevice } from "./devices";
import { formatTime, ReferenceDrop } from "./components/ReferenceDrop";
import "./App.css";

/** 手動の録音の上限 (秒)。Rust 側の MAX_RECORD_SECONDS と同じ。 */
export const MAX_RECORD_SECONDS = 30;
const LOOP_SECONDS = [3, 5, 10];
const LOOP_ITERATIONS = [5, 10, 20];

type Mode = "manual" | "auto";

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
  const [mode, setMode] = useState<Mode>(() => (loadPref("mode") === "auto" ? "auto" : "manual"));
  const [seconds, setSeconds] = useState<number>(() => {
    const v = Number(loadPref("seconds"));
    return LOOP_SECONDS.includes(v) ? v : 5;
  });
  const [maxIterations, setMaxIterations] = useState<number>(() => {
    const v = Number(loadPref("maxIterations"));
    return LOOP_ITERATIONS.includes(v) ? v : 10;
  });
  const [hx, setHx] = useState<HxStatus | null>(null);
  const [hxError, setHxError] = useState<string | null>(null);
  const [chain, setChain] = useState<ChainView | null>(null);
  const [chainLoading, setChainLoading] = useState(false);
  const [chainError, setChainError] = useState<string | null>(null);
  const [selectedBlockId, setSelectedBlockId] = useState<string | null>(null);
  const [recentChanges, setRecentChanges] = useState<DoneAction[]>([]);
  const autoLoadedChainRef = useRef(false);
  const [instruction, setInstruction] = useState("");
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [progress, setProgress] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [looping, setLooping] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [undoDepth, setUndoDepth] = useState(0);
  /** 録音を始めた時刻 (録音中だけ)。 */
  const [recordingSince, setRecordingSince] = useState<number | null>(null);
  const [elapsed, setElapsed] = useState(0);
  /** 録り終えて、まだ送っていない録音。 */
  const [pending, setPending] = useState<RecordingInfo | null>(null);

  const push = useCallback((m: Omit<ChatMessage, "id">) => {
    setMessages((prev) => [...prev, { ...m, id: nextId++ }]);
  }, []);
  const pushError = useCallback((text: string) => push({ role: "error", text }), [push]);

  const refreshDevices = useCallback(async () => {
    try {
      const list = await api.listInputDevices();
      setDevices(list);
      setDeviceId((cur) => pickDevice(list, cur ?? loadPref("deviceId")));
    } catch (e) {
      pushError(errorText(e));
    }
  }, [pushError]);

  const refreshChain = useCallback(async () => {
    if (busy) return;
    setChainLoading(true);
    setChainError(null);
    try {
      const view = await api.getChain();
      if (view) setChain(view);
    } catch (e) {
      setChainError(errorText(e));
    } finally {
      setChainLoading(false);
    }
  }, [busy]);

  const refreshHx = useCallback(async () => {
    try {
      const status = await api.hxStatus();
      setHx(status);
      setHxError(null);
      if (!status.connected) {
        autoLoadedChainRef.current = false;
      } else if (!autoLoadedChainRef.current) {
        autoLoadedChainRef.current = true;
        refreshChain();
      }
    } catch (e) {
      setHx(null);
      setHxError(errorText(e));
    }
  }, [refreshChain]);

  const disconnectHx = useCallback(async () => {
    try {
      setHx(await api.disconnectHx());
      setHxError(null);
    } catch (e) {
      pushError(errorText(e));
    }
  }, [pushError]);

  // 起動時
  useEffect(() => {
    (async () => {
      await refreshDevices();
      // 画面を読み込み直しても、Rust 側に残っている Undo 履歴を使えるようにする
      api.undoDepth().then(setUndoDepth).catch((e) => pushError(errorText(e)));
    })();
  }, [refreshDevices, pushError]);

  // HX Stomp の USB 接続状態を定期的に見る
  useEffect(() => {
    if (busy) return;
    refreshHx();
    const t = setInterval(refreshHx, 5000);
    return () => clearInterval(t);
  }, [busy, refreshHx]);

  useEffect(() => {
    if (deviceId) savePref("deviceId", deviceId);
  }, [deviceId]);
  useEffect(() => savePref("seconds", String(seconds)), [seconds]);
  useEffect(() => savePref("maxIterations", String(maxIterations)), [maxIterations]);
  useEffect(() => savePref("mode", mode), [mode]);

  useEffect(() => {
    const un = listen<Progress>("progress", (e) => {
      setProgress(e.payload.stage === "done" ? null : e.payload.message);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const un = listen<ChainView>("chain_updated", (e) => {
      setChain(e.payload);
      setChainError(null);
      setSelectedBlockId((cur) =>
        cur && e.payload.blocks.some((b) => b.id === cur) ? cur : null,
      );
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const showReport = useCallback(
    (r: CycleReport, kind: "adjust" | "undo", label?: string) => {
      const meta: string[] = [];
      if (label) meta.push(label);
      if (r.recording_peak_dbfs !== null) meta.push(`録音ピーク ${r.recording_peak_dbfs.toFixed(1)} dBFS`);
      if (r.goal_reached) meta.push("目標に届いた");
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
      if (kind === "undo") {
        setRecentChanges([]);
      } else if (r.changes.length > 0) {
        setRecentChanges(r.changes);
      }
      setUndoDepth(r.undo_depth);
    },
    [push],
  );

  // 自動ループの 1 回ごとの結果
  useEffect(() => {
    const un = listen<LoopIteration>("loop_iteration", (e) => {
      const { iteration, max, report } = e.payload;
      showReport(report, "adjust", `ループ ${iteration}/${max}`);
    });
    return () => {
      un.then((f) => f());
    };
  }, [showReport]);

  // 録音中の経過時間。上限に達したら止める
  const stopRecordingRef = useRef<() => void>(() => {});
  useEffect(() => {
    if (recordingSince === null) return;
    const t = setInterval(() => {
      const sec = (Date.now() - recordingSince) / 1000;
      setElapsed(sec);
      if (sec >= MAX_RECORD_SECONDS) {
        clearInterval(t);
        stopRecordingRef.current();
      }
    }, 200);
    return () => clearInterval(t);
  }, [recordingSince]);

  // 録音中か。ボタンの連打やタイマーで 2 回止めないよう、描画を待たずに見られる ref に持つ
  const recordingRef = useRef(false);
  const startRecording = useCallback(async () => {
    try {
      await api.startRecording(deviceId);
      recordingRef.current = true;
      setPending(null);
      setElapsed(0);
      setRecordingSince(Date.now());
    } catch (e) {
      pushError(errorText(e));
    }
  }, [deviceId, pushError]);

  const stopRecording = useCallback(async () => {
    if (!recordingRef.current) return;
    recordingRef.current = false;
    setRecordingSince(null);
    try {
      setPending(await api.stopRecording());
    } catch (e) {
      pushError(errorText(e));
    }
  }, [pushError]);
  stopRecordingRef.current = stopRecording;

  const toggleRecording = useCallback(() => {
    if (busy) return;
    if (recordingSince === null) startRecording();
    else stopRecording();
  }, [busy, recordingSince, startRecording, stopRecording]);

  const discardRecording = async () => {
    try {
      await api.discardRecording();
      setPending(null);
    } catch (e) {
      pushError(errorText(e));
    }
  };

  const send = useCallback(async () => {
    if (busy || recordingSince !== null) return;
    const text = instruction.trim();
    if (!text && !pending) return;
    setBusy(true);
    const attachment = pending ? `🎙 録音 ${pending.seconds.toFixed(1)} 秒` : null;
    push({ role: "user", text: [text, attachment].filter(Boolean).join("\n") });
    setInstruction("");
    try {
      const r = await api.sendMessage(text);
      showReport(r, "adjust");
      if (!r.recording_kept) setPending(null);
      // 何も変わらずに失敗したときは、送り直せるように指示を戻す
      if (r.error && r.changes.length === 0) setInstruction(text);
    } catch (e) {
      pushError(errorText(e));
      setInstruction(text);
    } finally {
      setProgress(null);
      setBusy(false);
      api.undoDepth().then(setUndoDepth).catch(() => {});
    }
  }, [busy, recordingSince, instruction, pending, push, pushError, showReport]);

  const startLoop = useCallback(async () => {
    if (busy) return;
    const text = instruction.trim();
    setBusy(true);
    setLooping(true);
    push({
      role: "user",
      text: `${text || "(指示なし)"}\n▶ 自動ループ (${seconds} 秒録音 · 最大 ${maxIterations} 回)`,
    });
    setInstruction("");
    try {
      const r = await api.startLoop(text, deviceId, seconds, maxIterations);
      push({ role: "system", text: `自動ループを終えた (${r.iterations} 回): ${r.reason}` });
      setUndoDepth(r.undo_depth);
    } catch (e) {
      pushError(errorText(e));
    } finally {
      setProgress(null);
      setBusy(false);
      setLooping(false);
      setStopping(false);
      api.undoDepth().then(setUndoDepth).catch(() => {});
    }
  }, [busy, instruction, deviceId, seconds, maxIterations, push, pushError]);

  const stopLoop = useCallback(async () => {
    setStopping(true);
    try {
      await api.stopLoop();
    } catch (e) {
      setStopping(false);
      pushError(errorText(e));
    }
  }, [pushError]);

  const addLoopInstruction = useCallback(async () => {
    const text = instruction.trim();
    if (!text) return;
    try {
      await api.addLoopInstruction(text);
      push({ role: "user", text: `${text}\n(次の回から加える)` });
      setInstruction("");
    } catch (e) {
      pushError(errorText(e));
    }
  }, [instruction, push, pushError]);

  /** ⌘↩ で呼ぶ、いまのモードの主な操作。 */
  const submit = useCallback(() => {
    if (mode === "manual") send();
    else if (looping) addLoopInstruction();
    else startLoop();
  }, [mode, looping, send, addLoopInstruction, startLoop]);

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

  const clearChat = async () => {
    try {
      await api.clearConversation();
      setMessages([]);
      setRecentChanges([]);
    } catch (e) {
      pushError(errorText(e));
    }
  };

  const blockNames = Object.fromEntries((chain?.blocks ?? []).map((b) => [b.id, b.name]));

  return (
    <div className="app">
      <header className="header">
        <div className="header-left">
          <div className="brand">HX AI</div>
          <div
            className={`target ${hx?.connected ? "ok" : "ng"}`}
            title={hxError ?? (hx?.connected ? `${hx.device_name ?? "HX Stomp"}` : "USB 未接続")}
          >
            {hx?.connected
              ? `${hx.device_name ?? "HX Stomp"} ${hx.session_open ? "接続中" : "待機中"}`
              : "HX Stomp 未接続"}
          </div>
          {hx?.session_open && (
            <button
              className="link"
              onClick={disconnectHx}
              disabled={busy}
              title="HX Edit を使うために USB 接続を解放する"
            >
              USB 解放
            </button>
          )}
        </div>
        <div className="header-actions">
          <select
            className="device"
            value={deviceId ?? ""}
            onChange={(e) => setDeviceId(e.target.value || null)}
            disabled={busy || recordingSince !== null}
            title="オーディオ入力"
          >
            {devices.length === 0 && <option value="">入力デバイスがありません</option>}
            {devices.map((d) => (
              <option key={d.id} value={d.id}>
                🎸 {d.name}
              </option>
            ))}
          </select>
          <button
            className="icon-btn"
            onClick={() => {
              refreshDevices();
              refreshHx();
              if (hx?.connected) refreshChain();
            }}
            disabled={busy}
            title="デバイスとチェーンを再読込"
          >
            ↻
          </button>
          <button
            className="icon-btn"
            onClick={() => api.openSettingsWindow().catch((e) => pushError(errorText(e)))}
            title="設定"
          >
            ⚙︎
          </button>
        </div>
      </header>

      <main className="workspace">
        <ChainOverview
          chain={chain}
          connected={hx?.connected ?? false}
          loading={chainLoading}
          error={chainError}
          onRefresh={refreshChain}
          selectedBlockId={selectedBlockId}
          onSelectBlock={setSelectedBlockId}
          recentChanges={recentChanges}
        />

        <aside className="assistant-pane">
          <section className="ref-bar">
            <ReferenceDrop disabled={busy} onError={pushError} />
          </section>

          <ChatLog messages={messages} progress={progress} blockNames={blockNames} />

          <footer className="composer">
            <div className="row controls-top">
              <div className="segmented" role="radiogroup" aria-label="モード">
                {(
                  [
                    ["manual", "手動"],
                    ["auto", "自動ループ"],
                  ] as const
                ).map(([m, label]) => (
                  <button
                    key={m}
                    role="radio"
                    aria-checked={mode === m}
                    className={mode === m ? "on" : ""}
                    onClick={() => setMode(m)}
                    disabled={busy || recordingSince !== null}
                  >
                    {label}
                  </button>
                ))}
              </div>
              {mode === "auto" && (
                <div className="loop-options small">
                  <span className="muted">録音</span>
                  <div className="segmented" role="radiogroup" aria-label="1 回の録音時間">
                    {LOOP_SECONDS.map((s) => (
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
                  <span className="muted">上限</span>
                  <div className="segmented" role="radiogroup" aria-label="ループの上限">
                    {LOOP_ITERATIONS.map((n) => (
                      <button
                        key={n}
                        role="radio"
                        aria-checked={maxIterations === n}
                        className={maxIterations === n ? "on" : ""}
                        onClick={() => setMaxIterations(n)}
                        disabled={busy}
                      >
                        {n} 回
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
            {mode === "manual" && pending && (
              <div className="attachment">
                <span>
                  🎙 録音 {pending.seconds.toFixed(1)} 秒 · ピーク {pending.peak_dbfs.toFixed(1)} dBFS
                </span>
                <button className="link" onClick={discardRecording} disabled={busy} aria-label="録音を消す">
                  消す
                </button>
              </div>
            )}
            <textarea
              value={instruction}
              onChange={(e) => setInstruction(e.target.value)}
              placeholder={mode === "manual" ? "指示" : looping ? "追加の指示" : "目標"}
              rows={3}
              disabled={busy && !looping}
              onKeyDown={(e) => {
                if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                  e.preventDefault();
                  submit();
                }
              }}
            />
            <div className="row">
              {mode === "manual" ? (
                <>
                  <button
                    className={`secondary ${recordingSince !== null ? "recording" : ""}`}
                    onClick={toggleRecording}
                    disabled={busy}
                  >
                    {recordingSince !== null ? `■ 停止 ${formatTime(elapsed)}` : "● 録音"}
                  </button>
                  <button
                    className="primary grow"
                    onClick={send}
                    disabled={busy || recordingSince !== null || (!instruction.trim() && !pending)}
                  >
                    {busy ? "処理中…" : "送信"}
                  </button>
                </>
              ) : looping ? (
                <>
                  <button className="primary grow" onClick={stopLoop} disabled={stopping}>
                    {stopping ? "止めています…" : "■ ループを止める"}
                  </button>
                  <button className="secondary" onClick={addLoopInstruction} disabled={!instruction.trim()}>
                    指示を追加
                  </button>
                </>
              ) : (
                <button className="primary grow" onClick={startLoop} disabled={busy}>
                  ▶ 自動ループを始める
                </button>
              )}
              <button className="secondary" onClick={undo} disabled={busy || undoDepth === 0} title="1 つ前の状態に戻す">
                ↶ 戻す{undoDepth > 0 ? ` (${undoDepth})` : ""}
              </button>
            </div>
            {messages.length > 0 && (
              <div className="hint">
                <button className="link" onClick={clearChat} disabled={busy}>
                  会話をリセット
                </button>
              </div>
            )}
          </footer>
        </aside>
      </main>
    </div>
  );
}
