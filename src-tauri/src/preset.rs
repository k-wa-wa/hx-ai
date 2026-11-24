//! HX Edit のプリセット (.hlx) を読み、AI に見せる形にし、AI の変更を当てる。OS には触らない。
//!
//! .hlx は JSON で、`data.tone.dsp0.block0` などにブロックのモデル (`@model`) とパラメータの
//! 保存の値が入っている。保存の値と画面の表示の関係 (Gain の 0.22 は 2.2、Time の 0.6 は 600 ms) は
//! HX Edit の中の定義から引く。
//! - `*.models`: モデルごとのパラメータ (名前・値の型・範囲・表示の種類)
//! - `HelixControls.json`: 表示の種類ごとの倍率・書式・選択肢の表示名

// AI への問い合わせとサイクルに組み込むまで (このあとの変更で使う)、使われない警告を許す
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Deserialize)]
struct ModelDef {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    params: Vec<ParamDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct ParamDef {
    #[serde(rename = "symbolicID")]
    id: String,
    name: String,
    #[serde(rename = "valueType")]
    value_type: u8,
    #[serde(rename = "displayType", default)]
    display_type: Option<String>,
    #[serde(rename = "displayType_stereo", default)]
    display_type_stereo: Option<String>,
    min: Value,
    max: Value,
    #[serde(default)]
    max_stereo: Option<Value>,
}

/// HX Edit のモデルと表示の定義。
pub struct Defs {
    models: HashMap<String, ModelDef>,
    controls: Map<String, Value>,
}

/// HX Edit の中の定義があるフォルダ。
pub const HX_EDIT_RESOURCES: &str = "/Applications/Line6/HX Edit.app/Contents/Resources";

impl Defs {
    /// `*.models` の中身 (それぞれモデルの配列) と `HelixControls.json` の中身から作る。
    pub fn from_json(models: &[&str], controls: &str) -> Result<Self, String> {
        let mut map = HashMap::new();
        for (i, text) in models.iter().enumerate() {
            let list: Vec<Value> = serde_json::from_str(text)
                .map_err(|e| format!("モデルの定義 ({} 個目) を読めません: {e}", i + 1))?;
            for m in list {
                let Some(id) = m.get("symbolicID").and_then(Value::as_str) else {
                    continue;
                };
                let id = id.to_string();
                let def: ModelDef = serde_json::from_value(m)
                    .map_err(|e| format!("モデル {id} の定義を読めません: {e}"))?;
                map.insert(id, def);
            }
        }
        let controls: Map<String, Value> = serde_json::from_str(controls)
            .map_err(|e| format!("表示の定義 (HelixControls.json) を読めません: {e}"))?;
        if map.is_empty() {
            return Err("モデルの定義が 1 つもありません".into());
        }
        Ok(Self {
            models: map,
            controls,
        })
    }

    /// HX Edit の Resources フォルダから読む。
    pub fn load(dir: &Path) -> Result<Self, String> {
        let entries = std::fs::read_dir(dir).map_err(|e| {
            format!(
                "HX Edit の定義のフォルダ {} を読めません ({e})。HX Edit が入っているかを確かめてください",
                dir.display()
            )
        })?;
        let mut paths: Vec<_> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "models"))
            .collect();
        paths.sort();
        let mut texts = Vec::new();
        for p in &paths {
            texts.push(
                std::fs::read_to_string(p)
                    .map_err(|e| format!("{} を読めません: {e}", p.display()))?,
            );
        }
        let controls_path = dir.join("HelixControls.json");
        let controls = std::fs::read_to_string(&controls_path)
            .map_err(|e| format!("{} を読めません: {e}", controls_path.display()))?;
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        Self::from_json(&refs, &controls)
    }

    /// 表示の種類の定義。別名 (`alias`) をたどる。
    fn control(&self, name: &str) -> Option<&Map<String, Value>> {
        let mut name = name;
        for _ in 0..8 {
            let c = self.controls.get(name)?.as_object()?;
            match c.get("alias").and_then(Value::as_str) {
                Some(next) => name = next,
                None => return Some(c),
            }
        }
        None
    }
}

/// 数値の表示の書式のうち、ある範囲の値に当てるもの。
#[derive(Debug, Clone, PartialEq)]
pub struct Range {
    pub lo: f64,
    pub hi: f64,
    /// 小数の桁数。None なら桁を決めない (余分な 0 を落とす)。
    pub decimals: Option<usize>,
    /// 数値の代わりに出る文字 ("Off" など)。
    pub label: Option<String>,
}

/// パラメータの値の種類と、表示との換算。
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// 表示の値 = 保存の値 × scale + offset。min と max は表示の値。
    Number {
        scale: f64,
        offset: f64,
        unit: String,
        integer: bool,
        min: f64,
        max: f64,
        ranges: Vec<Range>,
    },
    /// 選択肢。保存の値は base + 選んだ番号 (boolean なら false / true)。
    Choice {
        options: Vec<String>,
        base: i64,
        boolean: bool,
    },
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// printf の数値の指定 (`%.1f` `%+d` `%03d` `%f`) を 1 つ探す。(前, 小数の桁, 後)。
fn spec(fmt: &str) -> Option<(String, Option<usize>, String)> {
    let mut i = 0;
    let b = fmt.as_bytes();
    while i < b.len() {
        if b[i] == b'%' {
            if b.get(i + 1) == Some(&b'%') {
                i += 2;
                continue;
            }
            let rest = &fmt[i + 1..];
            let end = rest.find(['f', 'd'])?;
            let flags = &rest[..end];
            if !flags
                .chars()
                .all(|c| c.is_ascii_digit() || c == '+' || c == '.')
            {
                return None;
            }
            let decimals = if rest.as_bytes()[end] == b'd' {
                Some(0)
            } else {
                match flags.split_once('.') {
                    Some((_, n)) => Some(n.parse().ok()?),
                    None => None,
                }
            };
            let before = fmt[..i].replace("%%", "%");
            let after = rest[end + 1..].replace("%%", "%");
            return Some((before, decimals, after));
        }
        i += 1;
    }
    None
}

/// 選択肢の表示名の並び。
fn labels(c: &Map<String, Value>) -> Option<Vec<String>> {
    let list = c.get("format")?.as_array()?;
    let names: Option<Vec<String>> = list.iter().map(|v| v.as_str().map(String::from)).collect();
    names.filter(|n| !n.is_empty())
}

/// 数値の書式 (1 つの文字列か、範囲ごとの並び) を読む。単位は範囲のうち数値の指定があり、
/// 単位の言い換え (kHz・s) が無いものから取る。
fn number_format(c: &Map<String, Value>) -> Result<(String, Vec<Range>), String> {
    let all = Range {
        lo: f64::NEG_INFINITY,
        hi: f64::INFINITY,
        decimals: None,
        label: None,
    };
    match c.get("format") {
        None => Ok((String::new(), vec![all])),
        Some(Value::String(f)) => {
            let units = c.get("formatUnits").and_then(Value::as_str).unwrap_or(f);
            let (_, decimals, _) = spec(f).ok_or_else(|| format!("書式 {f} を読めません"))?;
            let (before, _, after) =
                spec(units).ok_or_else(|| format!("書式 {units} を読めません"))?;
            if !before.trim().is_empty() {
                return Err(format!("数値の前に文字がある書式 ({units})"));
            }
            Ok((after.trim().to_string(), vec![Range { decimals, ..all }]))
        }
        Some(Value::Array(list)) => {
            let mut unit = None;
            let mut ranges = Vec::new();
            for r in list {
                let r = r.as_object().ok_or("書式の並びを読めません")?;
                let lo = r
                    .get("lowerBound")
                    .and_then(as_f64)
                    .ok_or("書式の範囲が無い")?;
                let hi = r
                    .get("upperBound")
                    .and_then(as_f64)
                    .ok_or("書式の範囲が無い")?;
                let f = r.get("format").and_then(Value::as_str).unwrap_or("");
                let units = r.get("formatUnits").and_then(Value::as_str).unwrap_or(f);
                let multiplier = r.get("unitsMultiplier").and_then(as_f64);
                if multiplier.is_some_and(|m| m < 0.0) {
                    return Err(format!("向きで表示が変わる書式 ({units})"));
                }
                match spec(units) {
                    None => ranges.push(Range {
                        lo,
                        hi,
                        decimals: None,
                        label: Some(units.to_string()),
                    }),
                    Some((before, decimals, after)) => {
                        if !before.trim().is_empty() {
                            return Err(format!("数値の前に文字がある書式 ({units})"));
                        }
                        if multiplier.is_none() {
                            unit.get_or_insert_with(|| after.trim().to_string());
                            ranges.push(Range {
                                lo,
                                hi,
                                decimals,
                                label: None,
                            });
                        } else {
                            // kHz・s への言い換えは使わず、基本の単位のまま桁を決めない
                            ranges.push(Range {
                                lo,
                                hi,
                                decimals: None,
                                label: None,
                            });
                        }
                    }
                }
            }
            let unit = unit.ok_or("数値の書式が 1 つも無い")?;
            Ok((unit, ranges))
        }
        Some(_) => Err("書式を読めません".into()),
    }
}

impl Defs {
    /// パラメータの値の種類を決める。変えられない (表し方が分からない) ときは理由を返す。
    fn kind(&self, p: &ParamDef, stereo: bool) -> Result<Kind, String> {
        let display = if stereo {
            p.display_type_stereo.as_ref().or(p.display_type.as_ref())
        } else {
            p.display_type.as_ref()
        };
        let display = display.ok_or("表示の定義が無い")?;
        let c = self
            .control(display)
            .ok_or_else(|| format!("表示の種類 {display} の定義が無い"))?;
        if c.contains_key("minimumValue") {
            return Err(format!(
                "表示の範囲が保存の値と別に決まっている ({display})"
            ));
        }
        let max = if stereo {
            p.max_stereo.as_ref().unwrap_or(&p.max)
        } else {
            &p.max
        };
        let (min, max) = match (as_f64(&p.min), as_f64(max)) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err("範囲を読めません".into()),
        };
        match p.value_type {
            2 => {
                let options = labels(c).unwrap_or_else(|| vec!["Off".into(), "On".into()]);
                if options.len() != 2 {
                    return Err(format!("Off / On の表示名が 2 つでない ({display})"));
                }
                Ok(Kind::Choice {
                    options,
                    base: 0,
                    boolean: true,
                })
            }
            0 => {
                if let Some(options) = labels(c) {
                    if options.len() as f64 != max - min + 1.0 {
                        return Err(format!(
                            "選択肢の数 ({}) が範囲 ({min}〜{max}) と合わない",
                            options.len()
                        ));
                    }
                    return Ok(Kind::Choice {
                        options,
                        base: min as i64,
                        boolean: false,
                    });
                }
                let offset = c
                    .get("dspToDisplayIntegerOffset")
                    .and_then(as_f64)
                    .unwrap_or(0.0);
                let (unit, ranges) = number_format(c)?;
                Ok(Kind::Number {
                    scale: 1.0,
                    offset,
                    unit,
                    integer: true,
                    min: min + offset,
                    max: max + offset,
                    ranges,
                })
            }
            1 => {
                if labels(c).is_some() {
                    return Err(format!("小数の値に選択肢の表示 ({display})"));
                }
                let scale = c.get("dspToDisplayScale").and_then(as_f64).unwrap_or(1.0);
                let (unit, ranges) = number_format(c)?;
                Ok(Kind::Number {
                    scale,
                    offset: 0.0,
                    unit,
                    integer: false,
                    min: min * scale,
                    max: max * scale,
                    ranges,
                })
            }
            t => Err(format!("値の型 {t} は扱わない")),
        }
    }
}

/// 余分な 0 を落とした数値 (小数 3 桁まで)。
fn trim_number(v: f64) -> String {
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

fn fixed(v: f64, decimals: usize) -> String {
    let s = format!("{v:.decimals$}");
    // -0.0 を 0.0 と書く
    if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
        s[1..].to_string()
    } else {
        s
    }
}

impl Kind {
    /// 表示の値に当てる書式の範囲。
    fn range(ranges: &[Range], v: f64) -> Option<&Range> {
        ranges
            .iter()
            .find(|r| r.lo <= v && v < r.hi)
            .or_else(|| ranges.iter().rev().find(|r| r.lo <= v))
            .or(ranges.first())
    }

    /// 数値の代わりに画面に出る文字 (High Cut の最大の "Off" など)。
    fn label(&self, stored: &Value) -> Option<String> {
        match self {
            Kind::Number {
                scale,
                offset,
                ranges,
                ..
            } => {
                let v = as_f64(stored)? * scale + offset;
                Self::range(ranges, v)?.label.clone()
            }
            Kind::Choice { .. } => None,
        }
    }

    /// 保存の値を表示の文字にする (単位なし)。
    fn show(&self, stored: &Value) -> Result<String, String> {
        match self {
            Kind::Number {
                scale,
                offset,
                ranges,
                ..
            } => {
                let v = as_f64(stored).ok_or("数値でない")? * scale + offset;
                Ok(match Self::range(ranges, v).and_then(|r| r.decimals) {
                    Some(d) => fixed(v, d),
                    None => trim_number(v),
                })
            }
            Kind::Choice {
                options,
                base,
                boolean,
            } => {
                let i = if *boolean {
                    stored.as_bool().ok_or("Off / On でない")? as i64
                } else {
                    as_f64(stored).ok_or("数値でない")? as i64 - base
                };
                options
                    .get(usize::try_from(i).map_err(|_| "選択肢の外")?)
                    .cloned()
                    .ok_or_else(|| "選択肢の外".into())
            }
        }
    }

    /// 表示の文字 (AI の答え) を保存の値にする。範囲の外や読めない値はエラー。
    fn parse(&self, text: &str) -> Result<Value, String> {
        let t = text.trim();
        match self {
            Kind::Number {
                scale,
                offset,
                unit,
                integer,
                min,
                max,
                ..
            } => {
                let number = if !unit.is_empty()
                    && t.len() >= unit.len()
                    && t.is_char_boundary(t.len() - unit.len())
                    && t[t.len() - unit.len()..].eq_ignore_ascii_case(unit)
                {
                    t[..t.len() - unit.len()].trim()
                } else {
                    t
                };
                let v: f64 = number
                    .strip_prefix('+')
                    .unwrap_or(number)
                    .parse()
                    .map_err(|_| format!("{text} は数値として読めません"))?;
                if !v.is_finite() {
                    return Err(format!("{text} は数値として読めません"));
                }
                let range = format!("{}〜{}", trim_number(*min), trim_number(*max));
                let eps = 1e-9 * (max - min).abs().max(1.0);
                if v < min - eps || v > max + eps {
                    return Err(format!("{text} は範囲 ({range}) の外です"));
                }
                if *integer {
                    if v.fract() != 0.0 {
                        return Err(format!("{text} は整数ではありません"));
                    }
                    Ok(Value::from((v - offset) as i64))
                } else {
                    Ok(Value::from((v - offset) / scale))
                }
            }
            Kind::Choice {
                options,
                base,
                boolean,
            } => {
                let i = options
                    .iter()
                    .position(|o| o.eq_ignore_ascii_case(t))
                    .ok_or_else(|| {
                        format!("{text} は選択肢 ({}) にありません", options.join(" / "))
                    })?;
                Ok(if *boolean {
                    Value::Bool(i == 1)
                } else {
                    Value::from(base + i as i64)
                })
            }
        }
    }

    /// AI に見せる範囲か選択肢。
    fn describe_range(&self) -> String {
        match self {
            Kind::Number { min, max, .. } => {
                format!("({}〜{})", trim_number(*min), trim_number(*max))
            }
            Kind::Choice { options, .. } => format!("[{}]", options.join(" / ")),
        }
    }

    fn unit(&self) -> &str {
        match self {
            Kind::Number { unit, .. } => unit,
            Kind::Choice { .. } => "",
        }
    }
}

/// AI に見せるパラメータ。
#[derive(Debug, Clone, PartialEq)]
pub struct ParamView {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    /// 表示の値 (単位なし)。
    pub value: String,
    /// 数値の代わりに画面に出る文字。
    pub label: Option<String>,
    /// スナップショットやコントローラーに割り当てられている (変えない)。
    pub locked: bool,
}

/// AI に見せるブロック。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockView {
    /// `dsp0.block0` のような、.hlx の中の場所。
    pub id: String,
    pub model: String,
    pub enabled: Option<bool>,
    pub params: Vec<ParamView>,
}

fn tone(preset: &Value) -> Result<&Map<String, Value>, String> {
    preset
        .pointer("/data/tone")
        .and_then(Value::as_object)
        .ok_or_else(|| ".hlx の形が想定と違います (data.tone が無い)".to_string())
}

/// `dsp0` などの中のブロック (`block0`・`cab0`) を、信号の順 (経路・位置) に並べる。
fn blocks(tone: &Map<String, Value>) -> Vec<(String, String, &Map<String, Value>)> {
    let mut out = Vec::new();
    for (dsp, v) in tone {
        if !dsp.starts_with("dsp") {
            continue;
        }
        let Some(v) = v.as_object() else { continue };
        for (key, b) in v {
            if !(key.starts_with("block") || key.starts_with("cab")) {
                continue;
            }
            if let Some(b) = b.as_object().filter(|b| b.contains_key("@model")) {
                out.push((dsp.clone(), key.clone(), b));
            }
        }
    }
    let order = |b: &Map<String, Value>, k: &str| b.get(k).and_then(as_f64).unwrap_or(0.0);
    out.sort_by(|a, b| {
        (&a.0, order(a.2, "@path"), order(a.2, "@position"), &a.1)
            .partial_cmp(&(&b.0, order(b.2, "@path"), order(b.2, "@position"), &b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// スナップショットやコントローラーに割り当てられたパラメータ (`dsp0.block0` → 名前の集まり)。
fn controlled(tone: &Map<String, Value>) -> HashMap<String, HashSet<String>> {
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    let Some(c) = tone.get("controller").and_then(Value::as_object) else {
        return out;
    };
    for (dsp, blocks) in c {
        let Some(blocks) = blocks.as_object() else {
            continue;
        };
        for (block, params) in blocks {
            if let Some(params) = params.as_object() {
                out.entry(format!("{dsp}.{block}"))
                    .or_default()
                    .extend(params.keys().cloned());
            }
        }
    }
    out
}

/// ブロックの On / Off と、ステレオかどうかは AI に変えさせない。
const STRUCTURAL: [&str; 2] = ["@enabled", "@stereo"];

/// プリセットの中身を、AI に見せる形にする。表し方の分からないパラメータは載せない。
pub fn describe(preset: &Value, defs: &Defs) -> Result<Vec<BlockView>, String> {
    let tone = tone(preset)?;
    let locked = controlled(tone);
    let mut out = Vec::new();
    for (dsp, key, b) in blocks(tone) {
        let id = format!("{dsp}.{key}");
        let model_id = b.get("@model").and_then(Value::as_str).unwrap_or_default();
        let enabled = b.get("@enabled").and_then(Value::as_bool);
        let Some(model) = defs.models.get(model_id) else {
            out.push(BlockView {
                id,
                model: model_id.to_string(),
                enabled,
                params: Vec::new(),
            });
            continue;
        };
        let stereo = b.get("@stereo").and_then(Value::as_bool).unwrap_or(false);
        let mut params = Vec::new();
        for p in &model.params {
            if STRUCTURAL.contains(&p.id.as_str()) {
                continue;
            }
            let Some(stored) = b.get(&p.id) else { continue };
            let Ok(kind) = defs.kind(p, stereo) else {
                continue;
            };
            let Ok(value) = kind.show(stored) else {
                continue;
            };
            params.push(ParamView {
                id: p.id.clone(),
                name: p.name.clone(),
                value,
                label: kind.label(stored),
                locked: locked.get(&id).is_some_and(|s| s.contains(&p.id)),
                kind,
            });
        }
        out.push(BlockView {
            id,
            model: model.name.clone().unwrap_or_else(|| model_id.to_string()),
            enabled,
            params,
        });
    }
    Ok(out)
}

/// AI に渡す文字。1 行 1 パラメータ。
pub fn render(blocks: &[BlockView]) -> String {
    let mut s = String::new();
    for b in blocks {
        let state = match b.enabled {
            Some(true) => " (On)",
            Some(false) => " (Off)",
            None => "",
        };
        s.push_str(&format!("{} {}{state}\n", b.id, b.model));
        if b.params.is_empty() {
            s.push_str("- (変えられるパラメータは無い)\n");
        }
        for p in &b.params {
            let mut value = with_unit(&p.value, p.kind.unit());
            if let Some(label) = &p.label {
                value.push_str(&format!(" = {label}"));
            }
            if p.locked {
                s.push_str(&format!(
                    "- {}: {value} (固定: スナップショットかコントローラーで変わる)\n",
                    p.name
                ));
            } else {
                s.push_str(&format!(
                    "- {}: {value} {}\n",
                    p.name,
                    p.kind.describe_range()
                ));
            }
        }
    }
    s
}

/// AI が求める変更 1 つ。値は表示の値 (単位は付けても付けなくてもよい) か選択肢の表示名。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub block: String,
    pub parameter: String,
    pub value: String,
}

/// 当てた変更 (履歴と画面に出す)。値は単位付きの表示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub block: String,
    pub model: String,
    pub parameter: String,
    pub before: String,
    pub after: String,
}

fn with_unit(value: &str, unit: &str) -> String {
    if unit.is_empty() {
        value.to_string()
    } else {
        format!("{value} {unit}")
    }
}

/// 変更を当てたプリセットを返す。1 つでも当てられなければ、何も当てずにエラーにする。
pub fn apply(
    preset: &Value,
    defs: &Defs,
    changes: &[Change],
) -> Result<(Value, Vec<Applied>), String> {
    let views = describe(preset, defs)?;
    let mut out = preset.clone();
    let mut applied = Vec::new();
    let mut seen = HashSet::new();
    for c in changes {
        let block = views
            .iter()
            .find(|b| b.id == c.block.trim())
            .ok_or_else(|| format!("ブロック {} はプリセットにありません", c.block))?;
        let p = block
            .params
            .iter()
            .find(|p| p.name == c.parameter.trim() || p.id == c.parameter.trim())
            .ok_or_else(|| {
                format!(
                    "{} ({}) に変えられるパラメータ {} はありません",
                    block.id, block.model, c.parameter
                )
            })?;
        let what = format!("{} ({}) の {}", block.id, block.model, p.name);
        if p.locked {
            return Err(format!(
                "{what} はスナップショットかコントローラーで変わるので変えません"
            ));
        }
        if !seen.insert((block.id.clone(), p.id.clone())) {
            return Err(format!("{what} を 2 回変えようとしました"));
        }
        let stored = p.kind.parse(&c.value).map_err(|e| format!("{what}: {e}"))?;
        let after = p.kind.show(&stored)?;
        let (dsp, key) = block.id.split_once('.').expect("id は dsp.block の形");
        out["data"]["tone"][dsp][key][&p.id] = stored;
        applied.push(Applied {
            block: block.id.clone(),
            model: block.model.clone(),
            parameter: p.name.clone(),
            before: with_unit(&p.value, p.kind.unit()),
            after: with_unit(&after, p.kind.unit()),
        });
    }
    Ok((out, applied))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// HX Edit 3.82 の定義から、テストに使うモデルだけを写したもの。
    const MODELS: &str = r#"[
      { "symbolicID": "HD2_DistToneSovereign", "name": "Tone Sovereign", "params": [
        { "symbolicID": "Gain 1", "name": "Gain 1", "valueType": 1, "displayType": "generic_knob", "min": 0.0, "max": 1.0, "default": 0.5 },
        { "symbolicID": "Clipping 1", "name": "Clipping 1", "valueType": 0, "displayType": "clipping_dist", "min": 0, "max": 2, "default": 1 },
        { "symbolicID": "Level 1", "name": "Level 1", "valueType": 1, "displayType": "generic_knob", "min": 0.0, "max": 1.0, "default": 0.5 },
        { "symbolicID": "Voltage", "name": "Voltage", "valueType": 2, "displayType": "voltage", "min": false, "max": true, "default": false },
        { "symbolicID": "@enabled", "name": "Enabled", "valueType": 2, "min": false, "max": true, "default": true },
        { "symbolicID": "@stereo", "name": "Stereo", "valueType": 2, "min": false, "max": true, "default": false }
      ]},
      { "symbolicID": "HD2_AmpBritJ45Brt", "name": "Brit J45 Brt", "params": [
        { "symbolicID": "Drive", "name": "Drive", "valueType": 1, "displayType": "generic_knob", "min": 0.0, "max": 1.0, "default": 0.6 },
        { "symbolicID": "Treble", "name": "Treble", "valueType": 1, "displayType": "generic_knob", "min": 0.0, "max": 1.0, "default": 0.8 },
        { "symbolicID": "ChVol", "name": "Ch Vol", "valueType": 1, "displayType": "generic_knob", "min": 0.0, "max": 1.0, "default": 0.5 },
        { "symbolicID": "@enabled", "name": "Enabled", "valueType": 2, "min": false, "max": true, "default": true }
      ]}
    ]"#;

    const MORE_MODELS: &str = r#"[
      { "symbolicID": "HD2_DelaySimpleDelay", "name": "Simple Delay", "params": [
        { "symbolicID": "Time", "name": "Time", "valueType": 1, "displayType": "time_ms_0_8000", "displayType_stereo": "time_ms_0_4000", "min": 0.0, "max": 8.0, "max_stereo": 4.0, "default": 0.6 },
        { "symbolicID": "Mix", "name": "Mix", "valueType": 1, "displayType": "percent", "min": 0.0, "max": 1.0, "default": 0.4 },
        { "symbolicID": "Level", "name": "Level", "valueType": 1, "displayType": "volume", "min": -60.0, "max": 6.0, "default": 0.0 },
        { "symbolicID": "SyncSelect1", "displayType": "sync_note", "name": "Note Sync", "valueType": 0, "min": 1, "max": 3, "default": 2 },
        { "symbolicID": "TempoSync1", "displayType": "off_on", "name": "Tempo Sync", "valueType": 2, "min": false, "max": true, "default": false },
        { "symbolicID": "@trails", "name": "Trails", "valueType": 2, "displayType": "off_on", "min": false, "max": true, "default": false },
        { "symbolicID": "Pan", "name": "Pan", "valueType": 1, "displayType": "pan", "min": 0.0, "max": 1.0, "default": 0.5 },
        { "symbolicID": "HighCut", "name": "High Cut", "valueType": 1, "displayType": "mod_high_cut", "min": 500.0, "max": 20100.0, "default": 20100.0 },
        { "symbolicID": "Mode", "name": "Mode", "valueType": 0, "displayType": "integer_slider_1based", "min": 0, "max": 2, "default": 0 },
        { "symbolicID": "Shape", "name": "Shape", "valueType": 0, "displayType": "wave_shape", "min": 0, "max": 4, "default": 0 }
      ]}
    ]"#;

    /// HX Edit 3.82 の HelixControls.json から写したもの (step などは省く)。
    const CONTROLS: &str = r#"{
      "generic_knob": { "dspToDisplayScale": 10, "format": "%.1f" },
      "percent": { "dspToDisplayScale": 100, "format": "%.0f", "formatUnits": "%.0f %%" },
      "volume": { "format": "%+.1f", "formatUnits": "%+.1f dB" },
      "time_ms": { "dspToDisplayScale": 1000.0, "format": [
        { "lowerBound": 0.0, "upperBound": 9.999, "format": "%.1f", "formatUnits": "%.1f ms" },
        { "lowerBound": 9.999, "upperBound": 1000.0, "format": "%.0f", "formatUnits": "%.0f ms" },
        { "lowerBound": 1000, "upperBound": 99999.0, "unitsMultiplier": 0.001, "format": "%.0f", "formatUnits": "%.3f s" } ] },
      "time_ms_0_8000": { "alias": "time_ms" },
      "time_ms_0_4000": { "alias": "time_ms" },
      "clipping_dist": { "isDiscrete": true, "controlType": "segmented", "format": ["Overdrive", "Boost", "Distortion"] },
      "voltage": { "isDiscrete": true, "controlType": "segmented", "format": ["9V", "18V"] },
      "off_on": { "isDiscrete": true, "controlType": "segmented", "format": ["Off", "On"] },
      "sync_note": { "isDiscrete": true, "format": ["1/1", "1/2 Dotted", "1/2"] },
      "pan": { "minimumValue": -100, "maximumValue": 100, "zeroValue": 0.0, "format": [
        { "lowerBound": -99999, "upperBound": -0.5, "format": "%.0f", "formatUnits": "Left %.0f", "unitsMultiplier": -1 },
        { "lowerBound": -0.5, "upperBound": 0.5, "format": "%.0f", "formatUnits": "Center" },
        { "lowerBound": 0.5, "upperBound": 999999, "format": "%.0f", "formatUnits": "Right %.0f" } ] },
      "eq_high_cut": { "format": [
        { "lowerBound": 0.0, "upperBound": 1000.0, "format": "%.0f", "formatUnits": "%.0f Hz" },
        { "lowerBound": 1000.0, "upperBound": 20000.1, "unitsMultiplier": 0.001, "format": "%.0f", "formatUnits": "%.1f kHz" },
        { "lowerBound": 20000.1, "upperBound": 99999.0, "format": "Off" } ] },
      "mod_high_cut": { "alias": "eq_high_cut" },
      "integer_slider_1based": { "isDiscrete": true, "dspToDisplayIntegerOffset": 1 },
      "wave_shape": { "isDiscrete": true, "format": ["Sine", "Square", "Saw", "Ramp", "Triangle", "Noise", "Pulse"] }
    }"#;

    fn defs() -> Defs {
        Defs::from_json(&[MODELS, MORE_MODELS], CONTROLS).unwrap()
    }

    /// 実機の HX Stomp XL から書き出したプリセット (Tone Sovereign → Brit J45 Brt)。
    fn real_preset() -> Value {
        let path = format!(
            "{}/tests/fixtures/real_preset_tone_sovereign_brit.hlx",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Simple Delay 1 つのプリセット。
    fn delay_preset(block: Value) -> Value {
        serde_json::json!({ "data": { "tone": { "dsp0": { "block0": block } } } })
    }

    fn simple_delay() -> Value {
        serde_json::json!({
            "@model": "HD2_DelaySimpleDelay", "@enabled": true, "@stereo": false, "@position": 0,
            "Time": 0.6, "Mix": 0.4, "Level": -1.5, "SyncSelect1": 2, "TempoSync1": false,
            "@trails": true, "Pan": 0.5, "HighCut": 20100.0, "Mode": 1, "Shape": 0
        })
    }

    fn param<'a>(blocks: &'a [BlockView], block: &str, name: &str) -> &'a ParamView {
        blocks
            .iter()
            .find(|b| b.id == block)
            .unwrap()
            .params
            .iter()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("{name} が無い"))
    }

    fn change(block: &str, parameter: &str, value: &str) -> Change {
        Change {
            block: block.into(),
            parameter: parameter.into(),
            value: value.into(),
        }
    }

    // ---- 読む ----

    #[test]
    fn the_real_preset_is_shown_as_on_the_screen() {
        let blocks = describe(&real_preset(), &defs()).unwrap();
        assert_eq!(
            render(&blocks),
            "dsp0.block0 Tone Sovereign (On)\n\
             - Gain 1: 2.2 (0〜10)\n\
             - Clipping 1: Boost [Overdrive / Boost / Distortion]\n\
             - Level 1: 5.0 (0〜10)\n\
             - Voltage: 9V [9V / 18V]\n\
             dsp0.block1 Brit J45 Brt (On)\n\
             - Drive: 4.6 (0〜10)\n\
             - Treble: 8.0 (0〜10)\n\
             - Ch Vol: 5.0 (0〜10)\n"
        );
    }

    #[test]
    fn units_choices_and_ranges_follow_the_display_definitions() {
        let blocks = describe(&delay_preset(simple_delay()), &defs()).unwrap();
        assert_eq!(
            render(&blocks),
            "dsp0.block0 Simple Delay (On)\n\
             - Time: 600 ms (0〜8000)\n\
             - Mix: 40 % (0〜100)\n\
             - Level: -1.5 dB (-60〜6)\n\
             - Note Sync: 1/2 Dotted [1/1 / 1/2 Dotted / 1/2]\n\
             - Tempo Sync: Off [Off / On]\n\
             - Trails: On [Off / On]\n\
             - High Cut: 20100 Hz = Off (500〜20100)\n\
             - Mode: 2 (1〜3)\n",
            "Pan (向きで表示が変わる) と Shape (選択肢の数が合わない) は載せない"
        );
    }

    #[test]
    fn short_times_get_a_decimal_and_long_times_stay_in_ms() {
        let mut b = simple_delay();
        b["Time"] = serde_json::json!(0.0052);
        let v = describe(&delay_preset(b.clone()), &defs()).unwrap();
        assert_eq!(param(&v, "dsp0.block0", "Time").value, "5.2");
        b["Time"] = serde_json::json!(1.25);
        let v = describe(&delay_preset(b), &defs()).unwrap();
        assert_eq!(
            param(&v, "dsp0.block0", "Time").value,
            "1250",
            "s への言い換えはしない"
        );
    }

    #[test]
    fn stereo_blocks_use_the_stereo_range() {
        let mut b = simple_delay();
        b["@stereo"] = serde_json::json!(true);
        let v = describe(&delay_preset(b), &defs()).unwrap();
        assert_eq!(
            param(&v, "dsp0.block0", "Time").kind.describe_range(),
            "(0〜4000)"
        );
    }

    #[test]
    fn blocks_are_listed_in_signal_order() {
        let preset = serde_json::json!({ "data": { "tone": { "dsp0": {
            "block0": { "@model": "HD2_AmpBritJ45Brt", "@path": 0, "@position": 5 },
            "block1": { "@model": "HD2_DistToneSovereign", "@path": 0, "@position": 1 },
            "block2": { "@model": "HD2_DelaySimpleDelay", "@path": 1, "@position": 0 },
            "inputA": { "@model": "HelixStomp_AppDSPFlowInput" },
            "split": { "@model": "HD2_AppDSPFlowSplitY" }
        } } } });
        let ids: Vec<_> = describe(&preset, &defs())
            .unwrap()
            .into_iter()
            .map(|b| b.id)
            .collect();
        assert_eq!(ids, ["dsp0.block1", "dsp0.block0", "dsp0.block2"]);
    }

    #[test]
    fn unknown_models_are_listed_without_parameters() {
        let preset = delay_preset(serde_json::json!({ "@model": "HD2_Future", "@enabled": false }));
        assert_eq!(
            render(&describe(&preset, &defs()).unwrap()),
            "dsp0.block0 HD2_Future (Off)\n- (変えられるパラメータは無い)\n"
        );
    }

    #[test]
    fn parameters_assigned_to_snapshots_or_controllers_are_locked() {
        let mut preset = real_preset();
        preset["data"]["tone"]["controller"] = serde_json::json!({ "dsp0": { "block1": {
            "Drive": { "@min": 0.0, "@max": 1.0, "@controller": 19 }
        } } });
        let v = describe(&preset, &defs()).unwrap();
        assert!(param(&v, "dsp0.block1", "Drive").locked);
        assert!(!param(&v, "dsp0.block1", "Treble").locked);
        assert!(!param(&v, "dsp0.block0", "Gain 1").locked, "別のブロック");
        assert!(
            render(&v).contains("- Drive: 4.6 (固定: スナップショットかコントローラーで変わる)\n")
        );
        let e = apply(&preset, &defs(), &[change("dsp0.block1", "Drive", "5")]).unwrap_err();
        assert!(e.contains("変えません"), "{e}");
    }

    #[test]
    fn a_file_without_a_tone_is_an_error() {
        let e = describe(&serde_json::json!({ "data": {} }), &defs()).unwrap_err();
        assert!(e.contains("data.tone"), "{e}");
    }

    // ---- 変える ----

    #[test]
    fn changes_are_written_as_stored_values() {
        let (out, applied) = apply(
            &delay_preset(simple_delay()),
            &defs(),
            &[
                change("dsp0.block0", "Time", "450 ms"),
                change("dsp0.block0", "Mix", "25%"),
                change("dsp0.block0", "Level", "+2"),
                change("dsp0.block0", "Note Sync", "1/2"),
                change("dsp0.block0", "Trails", "off"),
                change("dsp0.block0", "Mode", "3"),
            ],
        )
        .unwrap();
        let b = &out["data"]["tone"]["dsp0"]["block0"];
        assert_eq!(b["Time"], serde_json::json!(0.45));
        assert_eq!(b["Mix"], serde_json::json!(0.25));
        assert_eq!(b["Level"], serde_json::json!(2.0));
        assert_eq!(b["SyncSelect1"], serde_json::json!(3));
        assert_eq!(b["@trails"], serde_json::json!(false));
        assert_eq!(b["Mode"], serde_json::json!(2));
        assert_eq!(b["Pan"], serde_json::json!(0.5), "他は変えない");
        assert_eq!(
            applied[0],
            Applied {
                block: "dsp0.block0".into(),
                model: "Simple Delay".into(),
                parameter: "Time".into(),
                before: "600 ms".into(),
                after: "450 ms".into(),
            }
        );
        assert_eq!(applied[1].after, "25 %");
        assert_eq!(applied[4].before, "On");
    }

    #[test]
    fn the_real_preset_keeps_everything_else() {
        let before = real_preset();
        let (out, applied) = apply(
            &before,
            &defs(),
            &[
                change("dsp0.block0", "Gain 1", "3.5"),
                change("dsp0.block0", "Clipping 1", "Distortion"),
                change("dsp0.block0", "Voltage", "18V"),
                change("dsp0.block1", "Ch Vol", "6"),
            ],
        )
        .unwrap();
        let mut expected = before.clone();
        let b0 = &mut expected["data"]["tone"]["dsp0"]["block0"];
        b0["Gain 1"] = serde_json::json!(0.35);
        b0["Clipping 1"] = serde_json::json!(2);
        b0["Voltage"] = serde_json::json!(true);
        expected["data"]["tone"]["dsp0"]["block1"]["ChVol"] = serde_json::json!(0.6);
        assert_eq!(out, expected);
        assert_eq!(applied[0].before, "2.2");
        assert_eq!(applied[0].after, "3.5");
        assert_eq!(applied[1].before, "Boost");
        assert_eq!(applied[3].parameter, "Ch Vol");
    }

    #[test]
    fn the_stored_name_also_finds_the_parameter() {
        let (out, _) = apply(
            &real_preset(),
            &defs(),
            &[change("dsp0.block1", "ChVol", "6")],
        )
        .unwrap();
        assert_eq!(
            out["data"]["tone"]["dsp0"]["block1"]["ChVol"],
            serde_json::json!(0.6)
        );
    }

    #[test]
    fn values_outside_the_range_are_refused_not_clamped() {
        for (p, v) in [("Gain 1", "10.5"), ("Gain 1", "-0.1")] {
            let e = apply(&real_preset(), &defs(), &[change("dsp0.block0", p, v)]).unwrap_err();
            assert!(e.contains("範囲 (0〜10) の外"), "{e}");
        }
        assert!(
            apply(
                &real_preset(),
                &defs(),
                &[change("dsp0.block0", "Gain 1", "10")]
            )
            .is_ok()
        );
        assert!(
            apply(
                &real_preset(),
                &defs(),
                &[change("dsp0.block0", "Gain 1", "0")]
            )
            .is_ok()
        );
        let e = apply(
            &delay_preset(simple_delay()),
            &defs(),
            &[change("dsp0.block0", "Mode", "4")],
        )
        .unwrap_err();
        assert!(e.contains("範囲 (1〜3) の外"), "{e}");
    }

    #[test]
    fn unreadable_values_are_refused() {
        let cases = [
            ("Time", "fast", "数値として読めません"),
            ("Time", "450 Hz", "数値として読めません"),
            ("Time", "NaN", "数値として読めません"),
            ("Mode", "1.5", "整数ではありません"),
            (
                "Note Sync",
                "1/16",
                "選択肢 (1/1 / 1/2 Dotted / 1/2) にありません",
            ),
            ("Trails", "maybe", "選択肢 (Off / On) にありません"),
        ];
        for (p, v, msg) in cases {
            let e = apply(
                &delay_preset(simple_delay()),
                &defs(),
                &[change("dsp0.block0", p, v)],
            )
            .unwrap_err();
            assert!(e.contains(msg) && e.contains(p), "{p} {v}: {e}");
        }
    }

    #[test]
    fn unknown_blocks_and_parameters_are_refused() {
        let e = apply(
            &real_preset(),
            &defs(),
            &[change("dsp0.block9", "Gain 1", "3")],
        )
        .unwrap_err();
        assert!(e.contains("block9"), "{e}");
        let e = apply(
            &real_preset(),
            &defs(),
            &[change("dsp0.block0", "Bass", "3")],
        )
        .unwrap_err();
        assert!(e.contains("Bass"), "{e}");
        for p in ["Enabled", "@enabled", "Pan"] {
            let preset = delay_preset(simple_delay());
            assert!(
                apply(&preset, &defs(), &[change("dsp0.block0", p, "On")]).is_err(),
                "{p} は変えない"
            );
        }
    }

    #[test]
    fn nothing_is_applied_when_one_change_fails() {
        let e = apply(
            &real_preset(),
            &defs(),
            &[
                change("dsp0.block0", "Gain 1", "3"),
                change("dsp0.block0", "Gain 1", "4"),
            ],
        )
        .unwrap_err();
        assert!(e.contains("2 回"), "{e}");
    }

    // ---- 書式 ----

    #[test]
    fn printf_specs() {
        assert_eq!(
            spec("%.1f ms"),
            Some((String::new(), Some(1), " ms".into()))
        );
        assert_eq!(
            spec("%+.0f %%"),
            Some((String::new(), Some(0), " %".into()))
        );
        assert_eq!(spec("%+d"), Some((String::new(), Some(0), String::new())));
        assert_eq!(spec("%03d"), Some((String::new(), Some(0), String::new())));
        assert_eq!(spec("%f"), Some((String::new(), None, String::new())));
        assert_eq!(
            spec("Left %.0f"),
            Some(("Left ".into(), Some(0), String::new()))
        );
        assert_eq!(spec("Center"), None);
        assert_eq!(spec("100%%"), None);
        assert_eq!(spec("%s"), None);
    }

    #[test]
    fn numbers_without_a_fixed_width_are_trimmed() {
        assert_eq!(trim_number(2.5), "2.5");
        assert_eq!(trim_number(10.0), "10");
        assert_eq!(trim_number(0.12345), "0.123");
        assert_eq!(trim_number(-0.0001), "0");
        assert_eq!(fixed(-0.01, 1), "0.0");
        assert_eq!(fixed(-1.25, 1), "-1.2");
    }

    /// HX Edit の本物の定義を読む。HX Edit が入っていない環境もあるので、手で走らせる。
    #[test]
    #[ignore]
    fn real_hx_edit_definitions() {
        let defs = Defs::load(Path::new(HX_EDIT_RESOURCES)).unwrap();
        let blocks = describe(&real_preset(), &defs).unwrap();
        let text = render(&blocks);
        println!("{text}");
        assert!(text.contains("- Gain 1: 2.2 (0〜10)"));
        assert!(text.contains("- Clipping 1: Boost [Overdrive / Boost / Distortion]"));
        let mut ok = 0;
        let mut skipped: HashMap<String, usize> = HashMap::new();
        for m in defs.models.values() {
            for p in &m.params {
                if STRUCTURAL.contains(&p.id.as_str()) {
                    continue;
                }
                match defs.kind(p, false) {
                    Ok(_) => ok += 1,
                    Err(e) => *skipped.entry(e).or_default() += 1,
                }
            }
        }
        let mut skipped: Vec<_> = skipped.into_iter().collect();
        skipped.sort_by_key(|s| std::cmp::Reverse(s.1));
        println!("扱えるパラメータ {ok}、扱えない {:?}", skipped);
    }
}
