//! 権限が足りているか、「許可する」を押したときに何をするかの判断。macOS を呼ぶのは `macos` で行う。

use serde::Serialize;

/// "granted" | "denied" | "not_determined"
#[derive(Debug, Clone, Serialize)]
pub struct PermissionStatus {
    pub microphone: String,
    pub screen_recording: String,
    pub accessibility: String,
}

/// 処理を始める前に足りない権限 (種類と表示名)。マイクが未確認 (not_determined) のときは
/// 録音を始めると macOS が許可を求めるので、足りないとはみなさない。
pub fn missing(
    status: &PermissionStatus,
    need_microphone: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if need_microphone && status.microphone == "denied" {
        out.push(("microphone", "マイク"));
    }
    if status.screen_recording != "granted" {
        out.push(("screen_recording", "画面収録"));
    }
    if status.accessibility != "granted" {
        out.push(("accessibility", "アクセシビリティ"));
    }
    out
}

/// 「許可する」を押したときにすること。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantPlan {
    /// macOS に許可を求める (初めてならダイアログが出る)。
    pub ask: bool,
    /// システム設定の該当ページを開く。
    pub open_settings: bool,
}

/// 権限の種類 ("microphone" / "screen_recording" / "accessibility") と今の状態
/// ("granted" / "denied" / "not_determined") から、すべきことを決める。
///
/// macOS は許可のダイアログを最初の 1 回しか出さない。一度拒否したり閉じたりした後は
/// システム設定でしか変えられないので、そのときは設定を開く。
pub fn grant_plan(kind: &str, state: &str) -> Result<GrantPlan, String> {
    settings_url(kind)?;
    let plan = |ask, open_settings| GrantPlan { ask, open_settings };
    Ok(match (kind, state) {
        (_, "granted") => plan(false, false),
        ("microphone", "not_determined") => plan(true, false),
        ("microphone", _) => plan(false, true),
        // 画面収録とアクセシビリティは「まだ聞いていない」と「拒否」を区別できない。
        // 求めるとシステム設定の一覧にこのアプリが載るので、求めてから設定を開く
        _ => plan(true, true),
    })
}

/// システム設定の該当ページの URL。
pub fn settings_url(kind: &str) -> Result<&'static str, String> {
    match kind {
        "microphone" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
        }
        "screen_recording" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        }
        "accessibility" => {
            Ok("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        }
        other => Err(format!("不明な権限です: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(mic: &str, screen: &str, ax: &str) -> PermissionStatus {
        PermissionStatus {
            microphone: mic.into(),
            screen_recording: screen.into(),
            accessibility: ax.into(),
        }
    }

    fn kinds(v: Vec<(&str, &str)>) -> Vec<String> {
        v.into_iter().map(|(k, _)| k.to_string()).collect()
    }

    #[test]
    fn nothing_missing_when_all_granted() {
        assert!(missing(&status("granted", "granted", "granted"), true).is_empty());
    }

    #[test]
    fn microphone_is_missing_only_when_denied_and_needed() {
        let s = status("denied", "granted", "granted");
        assert_eq!(kinds(missing(&s, true)), vec!["microphone"]);
        assert!(missing(&s, false).is_empty(), "録音しないなら要らない");
        // 未確認なら録音を始めたときに macOS が聞くので通す
        assert!(missing(&status("not_determined", "granted", "granted"), true).is_empty());
    }

    #[test]
    fn screen_and_accessibility_are_always_needed() {
        let s = status("granted", "denied", "denied");
        assert_eq!(
            kinds(missing(&s, false)),
            vec!["screen_recording", "accessibility"]
        );
        assert_eq!(missing(&s, false)[0].1, "画面収録");
    }

    const NOTHING: GrantPlan = GrantPlan {
        ask: false,
        open_settings: false,
    };
    const ASK: GrantPlan = GrantPlan {
        ask: true,
        open_settings: false,
    };
    const SETTINGS: GrantPlan = GrantPlan {
        ask: false,
        open_settings: true,
    };
    const BOTH: GrantPlan = GrantPlan {
        ask: true,
        open_settings: true,
    };

    #[test]
    fn granted_needs_nothing() {
        for kind in ["microphone", "screen_recording", "accessibility"] {
            assert_eq!(grant_plan(kind, "granted").unwrap(), NOTHING, "{kind}");
        }
    }

    #[test]
    fn microphone_asks_only_the_first_time() {
        assert_eq!(grant_plan("microphone", "not_determined").unwrap(), ASK);
        assert_eq!(
            grant_plan("microphone", "denied").unwrap(),
            SETTINGS,
            "拒否した後はダイアログが出ないので設定を開く"
        );
    }

    #[test]
    fn screen_and_accessibility_ask_and_open_settings() {
        assert_eq!(grant_plan("screen_recording", "denied").unwrap(), BOTH);
        assert_eq!(grant_plan("accessibility", "denied").unwrap(), BOTH);
    }

    #[test]
    fn unknown_kind_is_an_error() {
        assert!(grant_plan("camera", "denied").is_err());
        assert!(settings_url("camera").is_err());
    }

    #[test]
    fn settings_urls_point_to_each_privacy_page() {
        assert!(
            settings_url("microphone")
                .unwrap()
                .ends_with("Privacy_Microphone")
        );
        assert!(
            settings_url("screen_recording")
                .unwrap()
                .ends_with("Privacy_ScreenCapture")
        );
        assert!(
            settings_url("accessibility")
                .unwrap()
                .ends_with("Privacy_Accessibility")
        );
    }
}
