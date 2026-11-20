import { useEffect, useState } from "react";
import { api, errorText, SettingsView } from "../api";

const KEY_SOURCE: Record<SettingsView["api_key_source"], string> = {
  settings: "この画面で保存したキーを使用中",
  env: ".env / 環境変数の OPENAI_API_KEY を使用中",
  missing: "API キーがありません",
};

export function SettingsPanel({
  view,
  onSaved,
  onError,
}: {
  view: SettingsView | null;
  onSaved: (v: SettingsView) => void;
  onError: (msg: string) => void;
}) {
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [targetApp, setTargetApp] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (!view) return;
    setBaseUrl(view.stored.api_base_url);
    setModel(view.stored.model);
    setTargetApp(view.stored.target_app);
  }, [view]);

  const save = async () => {
    setSaving(true);
    try {
      const next = await api.saveSettings({
        apiBaseUrl: baseUrl,
        apiKey: clearKey ? "" : apiKey.trim() ? apiKey : null,
        model,
        targetApp,
      });
      setApiKey("");
      setClearKey(false);
      setSaved(true);
      onSaved(next);
    } catch (e) {
      onError(errorText(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="panel">
      <div className="panel-title">AI と操作対象の設定</div>
      <p className="muted small">空欄の項目は .env / 環境変数、それも無ければ既定値を使う。</p>
      <label className="field">
        <span>API の base URL (OpenAI 互換)</span>
        <input
          value={baseUrl}
          placeholder={view?.effective_base_url}
          onChange={(e) => setBaseUrl(e.target.value)}
          spellCheck={false}
        />
      </label>
      <label className="field">
        <span>モデル</span>
        <input
          value={model}
          placeholder={view?.effective_model}
          onChange={(e) => setModel(e.target.value)}
          spellCheck={false}
        />
      </label>
      <label className="field">
        <span>API キー</span>
        <input
          type="password"
          value={apiKey}
          placeholder={view?.stored.has_api_key ? "保存済み (変えるときだけ入力)" : "未保存"}
          onChange={(e) => setApiKey(e.target.value)}
          disabled={clearKey}
        />
      </label>
      {view && (
        <div className={`small ${view.api_key_source === "missing" ? "warn" : "muted"}`}>
          {KEY_SOURCE[view.api_key_source]}
        </div>
      )}
      {view?.stored.has_api_key && (
        <label className="check small">
          <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
          保存したキーを消す
        </label>
      )}
      <label className="field">
        <span>操作するアプリ名</span>
        <input
          value={targetApp}
          placeholder={view?.effective_target_app}
          onChange={(e) => setTargetApp(e.target.value)}
          spellCheck={false}
        />
      </label>
      <div className="row">
        <button className="primary" onClick={save} disabled={saving}>
          保存
        </button>
        {saved && <span className="muted small">保存した</span>}
      </div>
    </section>
  );
}
