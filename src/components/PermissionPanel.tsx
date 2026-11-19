import { api, errorText, PermissionKind, PermissionStatus } from "../api";

const LABELS: Record<PermissionKind, { name: string; why: string }> = {
  microphone: { name: "マイク", why: "HX Stomp の音を録音する" },
  screen_recording: { name: "画面収録", why: "HX Edit の画面を撮る" },
  accessibility: { name: "アクセシビリティ", why: "HX Edit の値を自動で入力する" },
};

const KINDS: PermissionKind[] = ["microphone", "screen_recording", "accessibility"];

export function allGranted(s: PermissionStatus | null): boolean {
  return !!s && KINDS.every((k) => s[k] === "granted");
}

/** ヘッダーに並べる小さな状態表示。押すと詳細を開く。 */
export function PermissionDots({
  status,
  onClick,
}: {
  status: PermissionStatus | null;
  onClick: () => void;
}) {
  return (
    <button className="perm-dots" onClick={onClick} title="macOS の権限">
      {KINDS.map((k) => (
        <span
          key={k}
          className={`dot ${status?.[k] ?? "unknown"}`}
          title={`${LABELS[k].name}: ${status?.[k] ?? "確認中"}`}
        />
      ))}
    </button>
  );
}

export function PermissionPanel({
  status,
  onRefresh,
  onError,
}: {
  status: PermissionStatus | null;
  onRefresh: () => void;
  onError: (msg: string) => void;
}) {
  const run = async (f: () => Promise<void>) => {
    try {
      await f();
    } catch (e) {
      onError(errorText(e));
    }
    setTimeout(onRefresh, 800);
  };

  return (
    <section className="panel">
      <div className="panel-title">
        macOS の権限
        <button className="link" onClick={onRefresh}>
          再確認
        </button>
      </div>
      {KINDS.map((k) => {
        const s = status?.[k];
        return (
          <div className="perm-row" key={k}>
            <span className={`dot ${s ?? "unknown"}`} />
            <div className="perm-text">
              <div>{LABELS[k].name}</div>
              <div className="muted small">{LABELS[k].why}</div>
            </div>
            {s === "granted" ? (
              <span className="muted small">許可済み</span>
            ) : (
              <div className="perm-actions">
                {/* 一度拒否するとダイアログは出なくなるので、設定画面を開く導線も置く */}
                <button className="small-btn" onClick={() => run(() => api.requestPermission(k))}>
                  許可を求める
                </button>
                <button
                  className="small-btn"
                  onClick={() => run(() => api.openPermissionSettings(k))}
                >
                  設定を開く
                </button>
              </div>
            )}
          </div>
        );
      })}
      <p className="muted small">
        画面収録とアクセシビリティは、システム設定で許可した後にアプリの再起動が必要なことがある。
      </p>
    </section>
  );
}
