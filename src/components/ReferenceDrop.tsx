import { useEffect, useRef, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, errorText, ReferenceInfo } from "../api";

/** 秒を「分:秒」で書く。 */
export function formatTime(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** リファレンス音源のドロップ先。ウィンドウのどこに落としても受け付ける。 */
export function ReferenceDrop({
  disabled,
  onError,
}: {
  disabled: boolean;
  onError: (msg: string) => void;
}) {
  const [info, setInfo] = useState<ReferenceInfo | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [hover, setHover] = useState(false);
  const [loading, setLoading] = useState(false);
  const audio = useRef<HTMLAudioElement>(null);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWebview()
      .onDragDropEvent(async (event) => {
        const p = event.payload;
        if (p.type === "enter" || p.type === "over") {
          setHover(true);
        } else if (p.type === "leave") {
          setHover(false);
        } else if (p.type === "drop") {
          setHover(false);
          if (p.paths.length === 0) return;
          setLoading(true);
          try {
            const next = await api.setReference(p.paths[0]);
            const bytes = await api.getReferenceAudio();
            setInfo(next);
            setUrl((old) => {
              if (old) URL.revokeObjectURL(old);
              return URL.createObjectURL(new Blob([bytes], { type: next.mime }));
            });
          } catch (e) {
            onError(errorText(e));
          } finally {
            setLoading(false);
          }
        }
      })
      .then((f) => (unlisten = f));
    return () => unlisten?.();
  }, [onError]);

  const clear = async () => {
    try {
      await api.clearReference();
      setInfo(null);
      setUrl((old) => {
        if (old) URL.revokeObjectURL(old);
        return null;
      });
    } catch (e) {
      onError(errorText(e));
    }
  };

  const useCurrentPosition = async () => {
    try {
      setInfo(await api.setReferenceStart(audio.current?.currentTime ?? 0));
    } catch (e) {
      onError(errorText(e));
    }
  };

  if (info && url) {
    const whole = info.clipSeconds >= info.duration - 0.01;
    return (
      <div className={`drop filled ${hover ? "hover" : ""}`}>
        <div className="drop-head">
          <span className="ref-name" title={info.name}>
            🎯 {info.name}
          </span>
          <button className="link" onClick={clear} disabled={disabled}>
            解除
          </button>
        </div>
        <audio controls src={url} ref={audio} />
        <div className="drop-head small">
          <span className="muted">
            AI に送る区間 {formatTime(info.clipStart)}〜{formatTime(info.clipStart + info.clipSeconds)}
          </span>
          {!whole && (
            <button className="link" onClick={useCurrentPosition} disabled={disabled}>
              再生位置から使う
            </button>
          )}
        </div>
      </div>
    );
  }
  return (
    <div className={`drop ${hover ? "hover" : ""}`}>
      {loading ? "音源を読み込み中…" : "目標の音 (WAV / MP3) をここにドロップ"}
      <div className="muted small">任意。無ければ指示だけで調整する</div>
    </div>
  );
}
