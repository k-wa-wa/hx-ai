import { useEffect, useState } from "react";
import { api, errorText, SettingsView } from "../api";

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
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (!view) return;
    setBaseUrl(view.stored.api_base_url);
    setModel(view.stored.model);
  }, [view]);

  const save = async () => {
    setSaving(true);
    try {
      const next = await api.saveSettings({
        apiBaseUrl: baseUrl,
        apiKey: clearKey ? "" : apiKey.trim() ? apiKey : null,
        model,
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
      <div className="panel-title">AI</div>
      <label className="field">
        <span>base URL</span>
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
          placeholder={
            view?.stored.has_api_key ? "保存済み" : view?.api_key_source === "env" ? ".env (開発用)" : ""
          }
          onChange={(e) => setApiKey(e.target.value)}
          disabled={clearKey}
        />
      </label>
      {view?.api_key_source === "missing" && <div className="small warn">API キーがありません</div>}
      {view?.stored.has_api_key && (
        <label className="check small">
          <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
          保存したキーを消す
        </label>
      )}
      <div className="row">
        <button className="primary" onClick={save} disabled={saving}>
          保存
        </button>
        {saved && <span className="muted small">保存した</span>}
      </div>
    </section>
  );
}
