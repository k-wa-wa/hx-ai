import { api, errorText, PermissionKind, PermissionStatus } from "../api";

const LABELS: Record<PermissionKind, string> = {
  microphone: "マイク",
};

const KINDS: PermissionKind[] = ["microphone"];

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
            <div className="perm-text">{LABELS[k]}</div>
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
    </section>
  );
}
