//! 操作する機種。機種ごとに、操作するアプリ・AI への説明・エフェクト一覧が変わる。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Device {
    /// Line 6 HX Stomp を HX Edit で操作する。
    HxStomp,
    /// Valeton GP-5 を Valeton Suite (iPad 版を Mac で動かしたもの) で操作する。
    Gp5,
}

impl Device {
    /// 設定ファイルの値から読む。空なら HX Stomp (機種を選べるようになる前の設定と同じ)。
    pub fn parse(id: &str) -> Result<Self, String> {
        match id.trim() {
            "" | "hx_stomp" => Ok(Self::HxStomp),
            "gp5" => Ok(Self::Gp5),
            other => Err(format!("不明な機種です: {other}")),
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::HxStomp => "hx_stomp",
            Self::Gp5 => "gp5",
        }
    }

    /// 操作するアプリの名前 (ウィンドウの持ち主) の既定値。
    pub fn default_target_app(self) -> &'static str {
        match self {
            Self::HxStomp => "HX Edit",
            Self::Gp5 => "Valeton Suite",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Device; 2] = [Device::HxStomp, Device::Gp5];

    #[test]
    fn parse_ids_and_default() {
        assert_eq!(Device::parse("").unwrap(), Device::HxStomp);
        assert_eq!(Device::parse("hx_stomp").unwrap(), Device::HxStomp);
        assert_eq!(Device::parse(" gp5 ").unwrap(), Device::Gp5);
        assert!(Device::parse("gp-5").is_err(), "知らない値は既定値にしない");
    }

    #[test]
    fn ids_roundtrip_and_match_serde() {
        for d in ALL {
            assert_eq!(Device::parse(d.id()).unwrap(), d);
            assert_eq!(serde_json::to_value(d).unwrap(), d.id());
        }
    }

    #[test]
    fn each_device_has_its_editor() {
        assert_eq!(Device::HxStomp.default_target_app(), "HX Edit");
        assert_eq!(Device::Gp5.default_target_app(), "Valeton Suite");
    }
}
