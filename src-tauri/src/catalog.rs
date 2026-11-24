//! HX Stomp のエフェクト一覧。AI がツール (search_effects) で引く。
//!
//! 一覧は公式マニュアルから作った `catalog/hx_stomp.json` である。毎回のリクエストに
//! 全部を載せると大きすぎるので、カテゴリとキーワードで絞った分だけを返す。

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// 検索 1 回で返すモデルの数の上限。超えた分は件数だけ伝え、絞り込みを促す。
pub const MAX_RESULTS: usize = 15;

/// カテゴリ。
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
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub based_on: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Catalog {
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

pub fn catalog() -> &'static Catalog {
    static HX: OnceLock<Catalog> = OnceLock::new();
    HX.get_or_init(|| {
        serde_json::from_str(include_str!("../catalog/hx_stomp.json"))
            .expect("catalog/hx_stomp.json が読めない (テストで確かめている)")
    })
}

/// 照合用に小文字にし、「系」を除く (「TS系」でも「TS」でも引けるように)。
fn normalize(s: &str) -> String {
    s.to_lowercase().replace('系', "")
}

fn haystack(m: &Model) -> String {
    let mut parts = vec![m.name.as_str(), m.kind.as_str()];
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
pub fn search(category: Option<&str>, query: Option<&str>) -> Result<SearchResult, String> {
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
    let c = catalog();
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
    fn catalog_loads_and_is_well_formed() {
        let c = catalog();
        assert!(!c.source.is_empty());
        // 件数が変わったら、一覧を作り直したことに気づけるように固定する
        assert_eq!(c.models.len(), 428);
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

    #[test]
    fn every_category_but_other_is_used_by_some_model() {
        for c in CATEGORIES.iter().filter(|c| **c != "other") {
            assert!(search(Some(c), None).unwrap().total > 0, "{c}");
        }
    }

    #[test]
    fn search_by_lineage_tag_with_or_without_kei() {
        let r = search(None, Some("TS系")).unwrap();
        assert_eq!(names(&r), vec!["Scream 808", "Screamer"]);
        let r = search(Some("drive"), Some("ts")).unwrap();
        assert_eq!(names(&r), vec!["Scream 808", "Screamer"]);
    }

    #[test]
    fn bb_finds_bluesbreaker_style_drives() {
        let r = search(Some("drive"), Some("bb")).unwrap();
        assert_eq!(names(&r), vec!["Heir Apparent", "Tone Sovereign"]);
    }

    #[test]
    fn search_matches_based_on_case_insensitively_and_requires_all_terms() {
        let r = search(Some("amp"), Some("marshall BRIGHT")).unwrap();
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
        let r = search(Some("delay"), Some("ping pong")).unwrap();
        assert_eq!(r.total, 2, "新しいものと Legacy の 2 つ");
        assert!(r.models.iter().all(|m| m.category == "delay"));
        let none = search(Some("reverb"), Some("ping pong")).unwrap();
        assert_eq!(none.total, 0);
    }

    #[test]
    fn results_are_capped_but_total_is_reported() {
        let r = search(Some("amp"), None).unwrap();
        assert_eq!(r.total, 113);
        assert_eq!(r.models.len(), MAX_RESULTS);
        let small = search(Some("eq"), None).unwrap();
        assert_eq!((small.total, small.models.len()), (8, 8));
    }

    #[test]
    fn invalid_requests_are_errors() {
        assert!(
            search(Some("fuzz"), None)
                .unwrap_err()
                .contains("不明なカテゴリ")
        );
        assert!(search(None, Some("  ")).is_err());
        assert!(search(Some(" "), None).is_err());
    }

    #[test]
    fn empty_fields_are_left_out_of_results() {
        let r = search(None, Some("Stupor")).unwrap();
        let json = serde_json::to_value(&r.models[0]).unwrap();
        assert!(
            json.get("note").is_none(),
            "元にした機種があるので説明は無い"
        );
        assert_eq!(json["type"], "Mono, Stereo");
        let original = search(None, Some("Thrifter")).unwrap();
        let json = serde_json::to_value(&original.models[0]).unwrap();
        assert!(json.get("based_on").is_none());
        assert_eq!(json["note"], "Line 6 Original");
    }
}
