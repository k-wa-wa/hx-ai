//! AI 接続先や操作対象アプリの設定。
//!
//! 設定画面で保存した値 (アプリの設定ディレクトリの settings.json) を優先し、
//! 空の項目だけ環境変数 (.env を含む) で補う。API キーはどちらにも無ければエラーにする。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/openai/";
pub const DEFAULT_MODEL: &str = "gemini-3.8-flash";
pub const DEFAULT_TARGET_APP: &str = "HX Edit";

/// settings.json に保存する内容。空文字は「未設定」として環境変数・既定値に任せる。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct StoredSettings {
    pub api_base_url: String,
    pub api_key: String,
    pub model: String,
    pub target_app: String,
}

/// 実際に使う値。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSettings {
    pub api_base_url: String,
    pub api_key: String,
    pub model: String,
    pub target_app: String,
}

/// 画面に返す設定の状態。API キーそのものは返さない。
#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub stored: StoredSettingsView,
    pub effective_base_url: String,
    pub effective_model: String,
    pub effective_target_app: String,
    /// "settings" | "env" | "missing"
    pub api_key_source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StoredSettingsView {
    pub api_base_url: String,
    pub model: String,
    pub target_app: String,
    pub has_api_key: bool,
}

/// 環境変数を読む関数。テストで差し替えられるようにしてある。
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn pick(stored: &str, env: EnvLookup, env_key: &str) -> Option<String> {
    non_empty(stored).or_else(|| env(env_key).and_then(|v| non_empty(&v)))
}

impl StoredSettings {
    pub fn resolve(&self, env: EnvLookup) -> Result<ResolvedSettings, String> {
        let api_key = pick(&self.api_key, env, "OPENAI_API_KEY").ok_or_else(|| {
            "API キーがありません。設定画面で入力するか、.env に OPENAI_API_KEY を書いてください"
                .to_string()
        })?;
        Ok(ResolvedSettings {
            api_base_url: pick(&self.api_base_url, env, "OPENAI_BASE_URL")
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            api_key,
            model: pick(&self.model, env, "OPENAI_MODEL")
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            target_app: pick(&self.target_app, env, "HX_TARGET_APP")
                .unwrap_or_else(|| DEFAULT_TARGET_APP.to_string()),
        })
    }

    pub fn view(&self, env: EnvLookup) -> SettingsView {
        let api_key_source = if non_empty(&self.api_key).is_some() {
            "settings"
        } else if env("OPENAI_API_KEY").and_then(|v| non_empty(&v)).is_some() {
            "env"
        } else {
            "missing"
        };
        SettingsView {
            stored: StoredSettingsView {
                api_base_url: self.api_base_url.clone(),
                model: self.model.clone(),
                target_app: self.target_app.clone(),
                has_api_key: non_empty(&self.api_key).is_some(),
            },
            effective_base_url: pick(&self.api_base_url, env, "OPENAI_BASE_URL")
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            effective_model: pick(&self.model, env, "OPENAI_MODEL")
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            effective_target_app: pick(&self.target_app, env, "HX_TARGET_APP")
                .unwrap_or_else(|| DEFAULT_TARGET_APP.to_string()),
            api_key_source: api_key_source.to_string(),
        }
    }
}

pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("settings.json")
}

/// ファイルが無ければ既定値 (すべて未設定) を返す。壊れていればエラーにする。
pub fn load(path: &Path) -> Result<StoredSettings, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| format!("設定ファイル {} を読めません: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoredSettings::default()),
        Err(e) => Err(format!("設定ファイル {} を開けません: {e}", path.display())),
    }
}

pub fn save(path: &Path, settings: &StoredSettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("設定ディレクトリ {} を作れません: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    std::fs::write(path, text)
        .map_err(|e| format!("設定ファイル {} に書けません: {e}", path.display()))?;
    // API キーを含むので本人以外に読ませない
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("設定ファイルの権限を変えられません: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_with(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn stored_values_take_precedence_over_env() {
        let env = env_with(&[("OPENAI_API_KEY", "env-key"), ("OPENAI_MODEL", "env-model")]);
        let s = StoredSettings {
            api_key: "ui-key".into(),
            model: "ui-model".into(),
            ..Default::default()
        };
        let r = s.resolve(&env).unwrap();
        assert_eq!(r.api_key, "ui-key");
        assert_eq!(r.model, "ui-model");
        assert_eq!(r.api_base_url, DEFAULT_BASE_URL);
        assert_eq!(r.target_app, DEFAULT_TARGET_APP);
    }

    #[test]
    fn env_fills_blank_fields() {
        let env = env_with(&[
            ("OPENAI_API_KEY", "env-key"),
            ("OPENAI_BASE_URL", "http://localhost:9999/v1/"),
        ]);
        let s = StoredSettings {
            model: "  ".into(),
            ..Default::default()
        };
        let r = s.resolve(&env).unwrap();
        assert_eq!(r.api_key, "env-key");
        assert_eq!(r.api_base_url, "http://localhost:9999/v1/");
        assert_eq!(r.model, DEFAULT_MODEL);
        assert_eq!(s.view(&env).api_key_source, "env");
    }

    #[test]
    fn missing_api_key_is_an_error() {
        let env = env_with(&[]);
        let err = StoredSettings::default().resolve(&env).unwrap_err();
        assert!(err.contains("OPENAI_API_KEY"));
        assert_eq!(
            StoredSettings::default().view(&env).api_key_source,
            "missing"
        );
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hx-ai-test-{}", std::process::id()));
        let path = settings_path(&dir);
        let s = StoredSettings {
            api_key: "k".into(),
            model: "m".into(),
            ..Default::default()
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap(), s);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_settings_file_means_defaults() {
        let path = std::env::temp_dir().join("hx-ai-test-no-such-dir/settings.json");
        assert_eq!(load(&path).unwrap(), StoredSettings::default());
    }

    #[test]
    fn unreadable_settings_path_is_an_error() {
        // ファイルの代わりにディレクトリがある (読めない) 場合は既定値で続けない
        let dir = std::env::temp_dir().join(format!("hx-ai-test-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("settings.json")).unwrap();
        assert!(load(&settings_path(&dir)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_settings_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("hx-ai-test-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = settings_path(&dir);
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
