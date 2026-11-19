import { useEffect, useState } from "react";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { api, errorText, ReferenceInfo } from "../api";

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

  if (info && url) {
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
        <audio controls src={url} />
      </div>
    );
  }
  return (
    <div className={`drop ${hover ? "hover" : ""}`}>
      目標の音 (WAV / MP3) をここにドロップ
      <div className="muted small">任意。無ければ指示だけで調整する</div>
    </div>
  );
}
