import { useEffect, useState } from "react";
import { api, Device, DEVICES, errorText, SettingsView } from "../api";

export function SettingsPanel({
  view,
  onSaved,
  onError,
}: {
  view: SettingsView | null;
  onSaved: (v: SettingsView) => void;
  onError: (msg: string) => void;
}) {
  const [device, setDevice] = useState<Device>("hx_stomp");
  const [baseUrl, setBaseUrl] = useState("");
  const [model, setModel] = useState("");
  const [targetApp, setTargetApp] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [clearKey, setClearKey] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    if (!view) return;
    setDevice(view.device);
    setBaseUrl(view.stored.api_base_url);
    setModel(view.stored.model);
    setTargetApp(view.stored.target_app);
  }, [view]);

  const save = async () => {
    setSaving(true);
    try {
      const next = await api.saveSettings({
        device,
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
      <label className="field">
        <span>機種</span>
        <select value={device} onChange={(e) => setDevice(e.target.value as Device)}>
          {DEVICES.map((d) => (
            <option key={d.id} value={d.id}>
              {d.label}
            </option>
          ))}
        </select>
      </label>
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
