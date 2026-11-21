//! 機種ごとのエフェクト一覧。AI がツール (search_effects) で引く。
//!
//! 一覧は各機種の公式マニュアルから作った `catalog/*.json` である。毎回のリクエストに
//! 全部を載せると大きすぎるので、カテゴリとキーワードで絞った分だけを返す。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::device::Device;

/// 検索 1 回で返すモデルの数の上限。超えた分は件数だけ伝え、絞り込みを促す。
pub const MAX_RESULTS: usize = 15;

/// 両機種で共通のカテゴリ。
pub const CATEGORIES: [&str; 11] = [
    "dynamics",
    "drive",
    "eq",
    "modulation",
    "delay",
    "reverb",
    "pitch",
    "filter",
    "amp",
    "cab",
    "other",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub name: String,
    pub category: String,
    /// 機種の中での置き場所 (GP-5 のモジュール名など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Catalog {
    pub device: Device,
    pub source: String,
    pub models: Vec<Model>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchResult {
    /// 一覧の出典 (マニュアルの版)。
    pub source: &'static str,
    /// 条件に合ったモデルの数 (返した数より多いことがある)。
    pub total: usize,
    pub models: Vec<Model>,
}

fn parse(device: Device, json: &str) -> Catalog {
    let c: Catalog =
        serde_json::from_str(json).expect("catalog/*.json が読めない (テストで確かめている)");
    assert_eq!(c.device, device, "別の機種の一覧を読んだ");
    c
}

pub fn catalog(device: Device) -> &'static Catalog {
    static HX: OnceLock<Catalog> = OnceLock::new();
    static GP5: OnceLock<Catalog> = OnceLock::new();
    match device {
        Device::HxStomp => {
            HX.get_or_init(|| parse(device, include_str!("../catalog/hx_stomp.json")))
        }
        Device::Gp5 => GP5.get_or_init(|| parse(device, include_str!("../catalog/gp5.json"))),
    }
}

/// 照合用に小文字にし、「系」を除く (「TS系」でも「TS」でも引けるように)。
fn normalize(s: &str) -> String {
    s.to_lowercase().replace('系', "")
}

fn haystack(m: &Model) -> String {
    let mut parts = vec![m.name.as_str(), m.kind.as_str()];
    parts.extend(m.module.as_deref());
    parts.extend(m.based_on.as_deref());
    parts.extend(m.note.as_deref());
    parts.extend(m.tags.iter().map(String::as_str));
    normalize(&parts.join(" "))
}

/// 英数字のキーワードは単語の頭に一致したときだけ当たりにする (「ts」が「slots」に当たらないように)。
/// 日本語を含むキーワードは、単語の区切りが無いので部分一致にする。
fn matches_term(haystack: &str, term: &str) -> bool {
    if !term.is_ascii() {
        return haystack.contains(term);
    }
    haystack.match_indices(term).any(|(i, _)| {
        haystack[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

/// カテゴリとキーワードで探す。キーワードは空白で区切り、すべてを含むものを返す。
pub fn search(
    device: Device,
    category: Option<&str>,
    query: Option<&str>,
) -> Result<SearchResult, String> {
    let category = category.map(str::trim).filter(|c| !c.is_empty());
    if let Some(c) = category
        && !CATEGORIES.contains(&c)
    {
        return Err(format!(
            "不明なカテゴリです: {c} (使えるのは {})",
            CATEGORIES.join(", ")
        ));
    }
    let terms: Vec<String> = query
        .unwrap_or("")
        .split_whitespace()
        .map(normalize)
        .filter(|t| !t.is_empty())
        .collect();
    if category.is_none() && terms.is_empty() {
        return Err("category か query のどちらかを指定してください".to_string());
    }
    let c = catalog(device);
    let hits: Vec<&Model> = c
        .models
        .iter()
        .filter(|m| category.is_none_or(|c| m.category == c))
        .filter(|m| {
            let h = haystack(m);
            terms.iter().all(|t| matches_term(&h, t))
        })
        .collect();
    Ok(SearchResult {
        source: &c.source,
        total: hits.len(),
        models: hits.into_iter().take(MAX_RESULTS).cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(r: &SearchResult) -> Vec<&str> {
        r.models.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn catalogs_load_and_are_well_formed() {
        for (device, count) in [(Device::HxStomp, 428), (Device::Gp5, 111)] {
            let c = catalog(device);
            assert_eq!(c.device, device);
            assert!(!c.source.is_empty());
            // 件数が変わったら、一覧を作り直したことに気づけるように固定する
            assert_eq!(c.models.len(), count, "{device:?}");
            let mut seen = std::collections::HashSet::new();
            for m in &c.models {
                assert!(CATEGORIES.contains(&m.category.as_str()), "{}", m.name);
                assert!(
                    seen.insert((m.name.clone(), m.kind.clone())),
                    "重複: {}",
                    m.name
                );
                assert!(
                    m.based_on.is_some() || m.note.is_some(),
                    "元にした機種か説明が要る: {}",
                    m.name
                );
            }
        }
    }

    #[test]
    fn every_category_is_used_by_some_model() {
        for c in CATEGORIES.iter().filter(|c| **c != "other") {
            assert!(
                search(Device::HxStomp, Some(c), None).unwrap().total > 0,
                "{c}"
            );
            assert!(search(Device::Gp5, Some(c), None).unwrap().total > 0, "{c}");
        }
    }

    #[test]
    fn search_by_lineage_tag_with_or_without_kei() {
        let gp5 = search(Device::Gp5, None, Some("TS系")).unwrap();
        assert_eq!(names(&gp5), vec!["Green OD"]);
        let hx = search(Device::HxStomp, Some("drive"), Some("ts")).unwrap();
        assert_eq!(names(&hx), vec!["Scream 808", "Screamer"]);
    }

    #[test]
    fn bb_finds_both_meanings() {
        // 「BB系」は Xotic BB Preamp と Bluesbreaker 系の両方を指しうる
        let gp5 = search(Device::Gp5, Some("drive"), Some("BB系")).unwrap();
        assert_eq!(names(&gp5), vec!["B-Boost"]);
        let hx = search(Device::HxStomp, Some("drive"), Some("bb")).unwrap();
        assert_eq!(names(&hx), vec!["Heir Apparent", "Tone Sovereign"]);
    }

    #[test]
    fn search_matches_based_on_case_insensitively_and_requires_all_terms() {
        let r = search(Device::HxStomp, Some("amp"), Some("marshall BRIGHT")).unwrap();
        assert_eq!(
            names(&r),
            vec!["Brit J45 Brt", "Brit Trem Brt", "Brit Plexi Brt"]
        );
    }

    #[test]
    fn ascii_terms_match_only_at_word_starts() {
        assert!(matches_term("ts tube screamer", "ts"));
        assert!(matches_term("(ts) x", "ts"));
        assert!(!matches_term("user ir slots", "ts"));
        assert!(matches_term("marshall bluesbreaker", "blues"));
        assert!(
            matches_term("ディストーション", "ストー"),
            "日本語は部分一致"
        );
    }

    #[test]
    fn category_filters_results() {
        let r = search(Device::HxStomp, Some("delay"), Some("ping pong")).unwrap();
        assert_eq!(r.total, 2, "新しいものと Legacy の 2 つ");
        assert!(r.models.iter().all(|m| m.category == "delay"));
        let none = search(Device::HxStomp, Some("reverb"), Some("ping pong")).unwrap();
        assert_eq!(none.total, 0);
    }

    #[test]
    fn results_are_capped_but_total_is_reported() {
        let r = search(Device::HxStomp, Some("amp"), None).unwrap();
        assert_eq!(r.total, 113);
        assert_eq!(r.models.len(), MAX_RESULTS);
        let small = search(Device::Gp5, Some("reverb"), None).unwrap();
        assert_eq!((small.total, small.models.len()), (10, 10));
    }

    #[test]
    fn gp5_models_carry_their_parameters() {
        let r = search(Device::Gp5, None, Some("Darktale")).unwrap();
        assert_eq!(r.models[0].params, vec!["Gain", "Filter", "VOL"]);
        assert_eq!(r.models[0].module.as_deref(), Some("DST"));
    }

    #[test]
    fn invalid_requests_are_errors() {
        assert!(
            search(Device::Gp5, Some("fuzz"), None)
                .unwrap_err()
                .contains("不明なカテゴリ")
        );
        assert!(search(Device::Gp5, None, Some("  ")).is_err());
        assert!(search(Device::Gp5, Some(" "), None).is_err());
    }

    #[test]
    fn empty_fields_are_left_out_of_results() {
        let r = search(Device::HxStomp, None, Some("Stupor")).unwrap();
        let json = serde_json::to_value(&r.models[0]).unwrap();
        assert!(
            json.get("params").is_none(),
            "HX の一覧にはパラメータが無い"
        );
        assert!(json.get("module").is_none());
        assert_eq!(json["type"], "Mono, Stereo");
    }
}
