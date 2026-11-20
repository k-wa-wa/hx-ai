//! 「許可する」を押したときに何をするかの判断。macOS を呼ぶのは `macos` で行う。

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
