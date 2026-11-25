import type { CSSProperties } from "react";
import type { BlockUiView, ChainView, DoneAction, ParamUiView } from "../api";

interface Props {
  chain: ChainView | null;
  connected: boolean;
  loading: boolean;
  error: string | null;
  onRefresh: () => void;
  selectedBlockId: string | null;
  onSelectBlock: (id: string | null) => void;
  recentChanges?: DoneAction[];
}

/** 同じ単位の数値パラメータだけ 0..100 のゲージ幅を返す (単位が変わる Hz/kHz などは出さない)。 */
function gaugePercent(p: ParamUiView): number | null {
  if (p.kind !== "number" || p.min === null || p.max === null) return null;
  const parse = (raw: string) => {
    const m = raw.trim().match(/^(-?\d+(?:\.\d+)?)\s*(.*)$/);
    if (!m) return null;
    return { num: Number(m[1]), unit: m[2].trim() };
  };
  const val = parse(p.value);
  const lo = parse(p.min);
  const hi = parse(p.max);
  if (!val || !lo || !hi) return null;
  if (val.unit !== lo.unit || val.unit !== hi.unit) return null;
  if (!(hi.num > lo.num)) return null;
  const pct = ((val.num - lo.num) / (hi.num - lo.num)) * 100;
  return Math.max(0, Math.min(100, pct));
}

function normKey(s: string): string {
  return s.trim().toLowerCase();
}

function BlockTile({
  block,
  selected,
  changed,
  onClick,
}: {
  block: BlockUiView;
  selected: boolean;
  changed: boolean;
  onClick: () => void;
}) {
  const style = { "--block-color": block.colorHex } as CSSProperties;
  return (
    <button
      type="button"
      className={`chain-tile ${block.enabled ? "on" : "bypassed"} ${selected ? "selected" : ""} ${changed ? "changed" : ""}`}
      style={style}
      onClick={onClick}
      aria-pressed={selected}
      title={`${block.id} · ${block.name}${block.locked && block.lockReason ? ` (${block.lockReason})` : ""}`}
    >
      <span className="tile-bar" />
      <div className="tile-top">
        <span className="tile-cat">{block.category}</span>
        {!block.enabled ? (
          <span className="tile-bypass">BYPASS</span>
        ) : changed ? (
          <span className="tile-changed">変更</span>
        ) : null}
      </div>
      <div className="tile-name">{block.name}</div>
      <div className="tile-foot">
        <span className="tile-id">{block.id}</span>
        {block.locked && (
          <span className="tile-lock" title={block.lockReason ?? "固定済み"}>
            固定
          </span>
        )}
      </div>
    </button>
  );
}

function BlockCard({
  block,
  focused,
  changedParams,
}: {
  block: BlockUiView;
  focused: boolean;
  changedParams?: Map<string, DoneAction>;
}) {
  const style = { "--block-color": block.colorHex } as CSSProperties;
  return (
    <article
      className={`block-card ${block.enabled ? "on" : "bypassed"} ${focused ? "focused" : ""}`}
      style={style}
    >
      <header className="block-card-head">
        <div className="block-title-wrap">
          <span className="block-cat-pill">{block.category}</span>
          <h3 className="block-name">{block.name}</h3>
          <span className="block-id">{block.id}</span>
          {block.locked && (
            <span className="block-lock" title={block.lockReason ?? "割り当て済み"}>
              {block.lockReason ?? "固定"}
            </span>
          )}
        </div>
        {!block.enabled && <span className="block-bypass off">BYPASS</span>}
      </header>

      {block.params.length > 0 ? (
        <div className="param-grid">
          {block.params.map((p) => {
            const pct = gaugePercent(p);
            const change = changedParams?.get(normKey(p.name));
            return (
              <div
                key={p.name}
                className={`param-cell ${p.locked ? "locked" : ""} ${change ? "changed" : ""}`}
                title={change ? `${change.previous_value} → ${change.value}` : undefined}
              >
                <div className="param-row">
                  <span className="param-name" title={p.name}>
                    {change && <span className="param-dot" aria-hidden="true" />}
                    {p.name}
                    {p.locked ? " (固定)" : ""}
                  </span>
                  <span className="param-value">{p.value}</span>
                </div>
                {pct !== null && (
                  <div className="param-meter" aria-hidden="true">
                    <div className="param-meter-fill" style={{ width: `${pct}%` }} />
                  </div>
                )}
              </div>
            );
          })}
        </div>
      ) : (
        <div className="muted small">調整可能なパラメータはありません</div>
      )}

      {block.readonlyNotes.length > 0 && (
        <div className="block-notes">
          {block.readonlyNotes.map((note, i) => (
            <div key={i} className="block-note">
              {note}
            </div>
          ))}
        </div>
      )}
    </article>
  );
}

export function ChainOverview({
  chain,
  connected,
  loading,
  error,
  onRefresh,
  selectedBlockId,
  onSelectBlock,
  recentChanges = [],
}: Props) {
  const visibleBlocks = chain
    ? selectedBlockId
      ? chain.blocks.filter((b) => b.id === selectedBlockId)
      : chain.blocks
    : [];

  const changedByBlock = new Map<string, Map<string, DoneAction>>();
  for (const c of recentChanges) {
    const bKey = normKey(c.block);
    let m = changedByBlock.get(bKey);
    if (!m) {
      m = new Map<string, DoneAction>();
      changedByBlock.set(bKey, m);
    }
    m.set(normKey(c.parameter), c);
  }

  const staleNotice = chain
    ? !connected
      ? "未接続 · 最終取得時の設定"
      : error
        ? "再読込失敗 · 最終取得時の設定"
        : null
    : null;

  return (
    <section className="chain-pane" aria-label="現在のエフェクト設定">
      <div className="preset-bar">
        <div className="preset-meta">
          {chain ? (
            <>
              <span className="preset-slot">{chain.slotLabel || "--"}</span>
              <h2 className="preset-title">{chain.presetName || "無題"}</h2>
              {chain.tempo !== null && (
                <span className="preset-pill">{chain.tempo.toFixed(1)} BPM</span>
              )}
              <span className="preset-pill muted">空き {chain.freeBlockSlots} 枠</span>
              {staleNotice && <span className="preset-pill stale">{staleNotice}</span>}
            </>
          ) : (
            <span className="muted">
              {connected ? "プリセット未読込" : "HX Stomp を USB で接続すると設定を表示する"}
            </span>
          )}
        </div>
        <div className="preset-actions">
          {selectedBlockId && (
            <button type="button" className="small-btn" onClick={() => onSelectBlock(null)}>
              すべて表示
            </button>
          )}
          {(!chain || error) && (
            <button
              type="button"
              className="small-btn"
              onClick={onRefresh}
              disabled={!connected || loading}
              title="HX Stomp から現在のチェーンを読み直す"
            >
              {loading ? "読込中…" : "チェーン読込"}
            </button>
          )}
        </div>
      </div>

      {error && <div className="chain-banner error">{error}</div>}

      {chain ? (
        <>
          <div className="signal-flow" role="list" aria-label="シグナルチェーン">
            <div className="flow-endpoint">IN</div>
            {chain.blocks.length === 0 ? (
              <div className="flow-empty muted small">ブロックがありません</div>
            ) : (
              chain.blocks.map((block) => (
                <div key={block.id} className="flow-step" role="listitem">
                  <span className="flow-wire" aria-hidden="true" />
                  <BlockTile
                    block={block}
                    selected={selectedBlockId === block.id}
                    changed={changedByBlock.has(normKey(block.id))}
                    onClick={() => onSelectBlock(selectedBlockId === block.id ? null : block.id)}
                  />
                </div>
              ))
            )}
            <span className="flow-wire" aria-hidden="true" />
            <div className="flow-endpoint">OUT</div>
          </div>

          <div className="blocks-scroll">
            {visibleBlocks.length === 0 ? (
              <div className="chain-empty muted">表示できるブロックがありません</div>
            ) : (
              <div className={`blocks-grid ${selectedBlockId ? "single" : ""}`}>
                {visibleBlocks.map((block) => (
                  <BlockCard
                    key={block.id}
                    block={block}
                    focused={selectedBlockId === block.id}
                    changedParams={changedByBlock.get(normKey(block.id))}
                  />
                ))}
              </div>
            )}
          </div>
        </>
      ) : (
        <div className="chain-empty-state">
          <div className="empty-flow-preview" aria-hidden="true">
            <span className="flow-endpoint">IN</span>
            <span className="flow-wire" />
            <span className="ghost-block">Dist</span>
            <span className="flow-wire" />
            <span className="ghost-block">Amp+Cab</span>
            <span className="flow-wire" />
            <span className="ghost-block">Delay</span>
            <span className="flow-wire" />
            <span className="ghost-block">Verb</span>
            <span className="flow-wire" />
            <span className="flow-endpoint">OUT</span>
          </div>
          <p className="muted">
            {connected
              ? "「チェーン読込」または送信で現在のブロック構成とパラメータを一目で表示する。"
              : "HX Stomp / HX Stomp XL を USB 接続すると、ブロック構成と全パラメータをここに一覧表示する。"}
          </p>
        </div>
      )}
    </section>
  );
}
