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
        <button className="icon-btn" onClick={onRefresh} title="再確認" aria-label="再確認">
          ↻
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
              <button className="small-btn" onClick={() => run(() => api.grantPermission(k))}>
                許可する
              </button>
            )}
          </div>
        );
      })}
      <p className="muted small">
        「許可する」で出るダイアログか、開いたシステム設定で許可する。画面収録とアクセシビリティは、許可した後にアプリの再起動が必要なことがある。
      </p>
    </section>
  );
}
