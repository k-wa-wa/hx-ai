import { useCallback, useEffect, useState } from "react";
import { api, errorText, PermissionStatus, SettingsView } from "./api";
import { PermissionPanel } from "./components/PermissionPanel";
import { SettingsPanel } from "./components/SettingsPanel";
import "./App.css";

/** 設定ウィンドウ (メイン画面とは別のウィンドウ)。 */
export default function SettingsWindow() {
  const [permissions, setPermissions] = useState<PermissionStatus | null>(null);
  const [settings, setSettings] = useState<SettingsView | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshPermissions = useCallback(async () => {
    try {
      setPermissions(await api.permissionStatus());
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  useEffect(() => {
    refreshPermissions();
    api
      .getSettings()
      .then(setSettings)
      .catch((e) => setError(errorText(e)));
  }, [refreshPermissions]);

  // システム設定で許可して戻ってきたら確かめ直す
  useEffect(() => {
    const onFocus = () => {
      refreshPermissions();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshPermissions]);

  return (
    <div className="settings-window">
      <PermissionPanel status={permissions} onRefresh={refreshPermissions} onError={setError} />
      <SettingsPanel
        view={settings}
        onSaved={(v) => {
          setSettings(v);
          setError(null);
        }}
        onError={setError}
      />
      {error && <div className="msg error">{error}</div>}
    </div>
  );
}
