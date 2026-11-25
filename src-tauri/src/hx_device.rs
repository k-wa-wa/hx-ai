//! HX Stomp / HX Stomp XL と USB で直接やり取りし、今のプリセットを読む・パラメータを変える。
//!
//! AI や外部からの誤操作を防ぐため、このモジュールが公開する操作は
//! 「現在の編集バッファの読み取り」「許可されたパラメータの変更」「Undo による復元」のみに絞る。
//! プリセットの保存 (`save`)・切り替え (`select`)・モデル変更・本体設定変更は行わない。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::JoinHandle;
use std::time::Duration;

use hx_catalog::{Catalog, Kind as CatalogKind};
use hx_proto::{Preset, Value as WireValue, preset::Kind as SlotKind, rpc};
use serde::Serialize;

/// 実機のアイドル切断を防ぐため、tonepush-gui と同じ 700ms 周期で keepalive を送る。
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_millis(700);

/// UI に返す HX Stomp の USB 接続状態。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HxStatus {
    pub connected: bool,
    pub device_name: Option<String>,
    pub session_open: bool,
}

/// USB 上に HX Stomp 等が見えているかを調べる (セッションは張らないので HX Edit とも競合しない)。
pub fn probe_status(session_open: bool) -> Result<HxStatus, String> {
    let devices = hx_usb::list().map_err(|e| format!("USB デバイスを探せません: {e}"))?;
    Ok(match devices.into_iter().next() {
        Some(found) => HxStatus {
            connected: true,
            device_name: Some(found.profile.name.to_string()),
            session_open,
        },
        None => HxStatus {
            connected: false,
            device_name: None,
            session_open: false,
        },
    })
}

/// 1 回の応答で受け付ける操作の上限。
pub const MAX_OPERATIONS: usize = 12;

/// AI やワークフローから渡す 1 件のパラメータ変更指示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// ブロック識別子 (例: `"block2"`)。
    pub block: String,
    /// パラメータの表示名 (例: `"Gain 1"`)。
    pub parameter: String,
    /// 画面表示相当の新しい値 (例: `"2.5"`, `"Boost"`)。
    pub value: String,
}

/// ブロック追加時の配置位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockPosition {
    /// メインラインの末尾 (Output の直前)。
    End,
    /// 指定ブロックの直前 (例: `"block5"`)。
    Before(String),
    /// 指定ブロックの直後 (例: `"block2"`)。
    After(String),
}

/// AI やワークフローから渡す 1 件のブロック操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockOp {
    SetEnabled {
        block: String,
        enabled: bool,
    },
    Remove {
        block: String,
    },
    Add {
        model: String,
        position: BlockPosition,
        stereo: Option<bool>,
        amp_cab: bool,
    },
    Replace {
        block: String,
        model: String,
        stereo: Option<bool>,
        amp_cab: bool,
    },
}

/// パラメータの見た目と制約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamKindView {
    Number { min: String, max: String },
    Choice { options: Vec<String> },
    ReadOnly,
}

/// 1 つのパラメータの現在状態。
#[derive(Debug, Clone, PartialEq)]
pub struct ParamView {
    pub index: usize,
    pub name: String,
    pub value: String,
    pub native_value: f32,
    pub wire_kind: CatalogKind,
    pub kind: ParamKindView,
    pub locked: bool,
    pub lock_reason: Option<String>,
}

/// チェーン上の 1 ブロック。
#[derive(Debug, Clone, PartialEq)]
pub struct BlockView {
    pub id: String,
    pub slot: usize,
    pub model_number: u32,
    pub name: String,
    pub category: String,
    pub color_hex: String,
    pub enabled: bool,
    /// フットスイッチ・コントローラー・スナップショット切り替え対象などのため、削除・モデル差し替え不可か。
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub params: Vec<ParamView>,
    /// Amp+Cab ブロックの Cab 側など、読み取り参考用の注記。
    pub readonly_notes: Vec<String>,
}

/// 現在のプリセットの編集バッファのスナップショット。
#[derive(Debug, Clone, PartialEq)]
pub struct ChainSnapshot {
    pub slot_label: String,
    pub preset_name: String,
    pub tempo: Option<f32>,
    pub snapshots: Vec<String>,
    pub free_block_slots: usize,
    /// 構造変更を含む Undo 用に、読み込み時の編集バッファ生バイト列を控える。
    pub raw_preset: Option<Vec<u8>>,
    pub blocks: Vec<BlockView>,
}

/// 画面表示向けのパラメータ 1 件。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ParamUiView {
    pub name: String,
    pub value: String,
    pub kind: &'static str,
    pub min: Option<String>,
    pub max: Option<String>,
    pub locked: bool,
}

/// 画面表示向けのブロック 1 つ。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BlockUiView {
    pub id: String,
    pub slot: usize,
    pub name: String,
    pub category: String,
    pub color_hex: String,
    pub enabled: bool,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub readonly_notes: Vec<String>,
    pub params: Vec<ParamUiView>,
}

/// 画面表示向けのプリセットとエフェクトチェーン全体。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChainView {
    pub slot_label: String,
    pub preset_name: String,
    pub tempo: Option<f32>,
    pub snapshots: Vec<String>,
    pub free_block_slots: usize,
    pub blocks: Vec<BlockUiView>,
}

/// 内部スナップショットをフロントエンド向けの JSON 表現に変換する。
pub fn to_chain_view(snap: &ChainSnapshot) -> ChainView {
    ChainView {
        slot_label: snap.slot_label.clone(),
        preset_name: snap.preset_name.clone(),
        tempo: snap.tempo,
        snapshots: snap.snapshots.clone(),
        free_block_slots: snap.free_block_slots,
        blocks: snap
            .blocks
            .iter()
            .map(|b| BlockUiView {
                id: b.id.clone(),
                slot: b.slot,
                name: b.name.clone(),
                category: b.category.clone(),
                color_hex: b.color_hex.clone(),
                enabled: b.enabled,
                locked: b.locked,
                lock_reason: b.lock_reason.clone(),
                readonly_notes: b.readonly_notes.clone(),
                params: b
                    .params
                    .iter()
                    .map(|p| {
                        let (kind, min, max) = match &p.kind {
                            ParamKindView::Number { min, max } => {
                                ("number", Some(min.clone()), Some(max.clone()))
                            }
                            ParamKindView::Choice { .. } => ("choice", None, None),
                            ParamKindView::ReadOnly => ("readonly", None, None),
                        };
                        ParamUiView {
                            name: p.name.clone(),
                            value: p.value.clone(),
                            kind,
                            min,
                            max,
                            locked: p.locked,
                        }
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// 検証済みの 1 件の書き込み計画。
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedWrite {
    pub slot: usize,
    pub block_id: String,
    pub block_name: String,
    pub param_index: usize,
    pub param_name: String,
    pub previous_display: String,
    pub previous_native: f32,
    pub new_display: String,
    pub new_native: f32,
    pub wire_kind: CatalogKind,
}

/// 実際に機器へ送った変更 (Undo 用に元の生値を保持する)。
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedChange {
    pub slot: usize,
    pub block_id: String,
    pub block_name: String,
    pub param_index: usize,
    pub param_name: String,
    pub previous_display: String,
    pub previous_native: f32,
    pub new_display: String,
    pub new_native: f32,
    pub wire_kind: CatalogKind,
}

impl From<PlannedWrite> for AppliedChange {
    fn from(w: PlannedWrite) -> Self {
        Self {
            slot: w.slot,
            block_id: w.block_id,
            block_name: w.block_name,
            param_index: w.param_index,
            param_name: w.param_name,
            previous_display: w.previous_display,
            previous_native: w.previous_native,
            new_display: w.new_display,
            new_native: w.new_native,
            wire_kind: w.wire_kind,
        }
    }
}

/// Undo で戻せなかった項目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFailure {
    pub block: String,
    pub parameter: String,
    pub value: String,
    pub reason: String,
}

/// 実機との最小限のやり取り (テストでは偽物に差し替える)。
pub trait HxPort: Send {
    fn read_snapshot(&mut self) -> Result<ChainSnapshot, String>;
    fn write_param(
        &mut self,
        slot: usize,
        param_index: usize,
        value: WireValue,
    ) -> Result<(), String>;
    fn set_enabled(&mut self, slot: usize, enabled: bool) -> Result<(), String>;
    fn clear_block(&mut self, slot: usize) -> Result<(), String>;
    fn set_model(&mut self, slot: usize, model: u32, paired_cab: Option<u32>)
    -> Result<(), String>;
    fn write_preset_bytes(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn catalog(&self) -> &Catalog;
}

/// ワイヤー上のプリセット (`hx_proto::Preset`) を、AI と検証ロジック向けの表現に変換する。
pub fn snapshot_from_preset(
    preset: &Preset,
    catalog: &Catalog,
    layout: rpc::ControlLayout,
    slot_label: String,
    preset_name: String,
) -> ChainSnapshot {
    let mut locked_params: HashMap<(i64, i64), String> = HashMap::new();
    let mut locked_blocks: HashMap<usize, String> = HashMap::new();
    for a in preset.assignments(layout) {
        if let Ok(slot) = usize::try_from(a.block) {
            locked_blocks
                .entry(slot)
                .or_insert_with(|| format!("{} に割り当て済み", a.source.label()));
        }
        if let hx_proto::preset::Target::Param(idx) = a.target {
            locked_params
                .entry((a.block, idx))
                .or_insert_with(|| format!("コントローラー ({:?}) に割り当て済み", a.source));
        }
    }

    // 有効なスナップショット間で ON/OFF が異なるブロックも、構成破壊を防ぐため保護する
    let valid_snapshots: Vec<_> = preset
        .snapshot_details()
        .into_iter()
        .filter(|s| s.valid)
        .collect();
    for (slot, _) in preset.blocks() {
        let states: Vec<bool> = valid_snapshots
            .iter()
            .filter_map(|s| s.enabled.get(slot).and_then(|v| *v))
            .collect();
        if states.iter().any(|&b| b) && states.iter().any(|&b| !b) {
            locked_blocks
                .entry(slot)
                .or_insert_with(|| "スナップショット間で ON/OFF が異なるため固定".into());
        }
    }

    let free_block_slots = preset
        .slots
        .iter()
        .filter(|s| matches!(s.kind, SlotKind::Block | SlotKind::Empty) && s.model.is_none())
        .count();

    let blocks = preset
        .blocks()
        .map(|(slot, block)| {
            let model_number = block.model.unwrap_or_default();
            let model_name = catalog
                .model_number(model_number)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| format!("model {model_number}"));

            let params = block
                .values
                .iter()
                .enumerate()
                .map(|(index, &native)| {
                    build_param_view(
                        catalog,
                        model_number,
                        index,
                        native,
                        locked_params.get(&(slot as i64, index as i64)).cloned(),
                    )
                })
                .collect();

            let mut readonly_notes = Vec::new();
            if let Some(cab_model) = block.paired {
                let cab_name = catalog
                    .model_number(cab_model)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| format!("model {cab_model}"));
                let cab_params: Vec<String> = block
                    .paired_values
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, &val)| {
                        catalog
                            .param(cab_model, idx)
                            .map(|p| format!("{}={}", p.name, catalog.format(p, val)))
                    })
                    .collect();
                if cab_params.is_empty() {
                    readonly_notes.push(format!("Cab ({cab_name})"));
                } else {
                    readonly_notes.push(format!("Cab ({cab_name}): {}", cab_params.join(", ")));
                }
            }

            let lock_reason = locked_blocks.get(&slot).cloned();
            let locked = lock_reason.is_some();
            let (category, color_hex) = match catalog
                .model_number(model_number)
                .and_then(|m| catalog.category_of(&m.id))
                .and_then(|id| catalog.category(id))
            {
                Some(cat) => {
                    let label = if block.paired.is_some() && cat.name == "Amp" {
                        "Amp+Cab".to_string()
                    } else if !cat.short_name.is_empty() {
                        cat.short_name.clone()
                    } else {
                        cat.name.clone()
                    };
                    (label, format!("#{:06x}", cat.colour & 0x00ff_ffff))
                }
                None => ("FX".to_string(), "#646874".to_string()),
            };

            BlockView {
                id: format!("block{slot}"),
                slot,
                model_number,
                name: model_name,
                category,
                color_hex,
                enabled: block.enabled,
                locked,
                lock_reason,
                params,
                readonly_notes,
            }
        })
        .collect();

    ChainSnapshot {
        slot_label,
        preset_name,
        tempo: preset.tempo(),
        snapshots: preset.snapshots(),
        free_block_slots,
        raw_preset: Some(preset.encode()),
        blocks,
    }
}

fn build_param_view(
    catalog: &Catalog,
    model_number: u32,
    index: usize,
    native: f32,
    assignment_lock: Option<String>,
) -> ParamView {
    let Some(param) = catalog.param(model_number, index) else {
        return ParamView {
            index,
            name: format!("param_{index}"),
            value: format!("{native}"),
            native_value: native,
            wire_kind: CatalogKind::Continuous,
            kind: ParamKindView::ReadOnly,
            locked: true,
            lock_reason: Some("カタログに定義が無いパラメータ".into()),
        };
    };

    let value = catalog.format(param, native);
    let (kind, unsupported_reason) = match param.kind {
        CatalogKind::Text => (
            ParamKindView::ReadOnly,
            Some("文字入力パラメータは変更不可".to_string()),
        ),
        CatalogKind::Switch => {
            let options = catalog
                .choices(param)
                .map(|c| c.to_vec())
                .unwrap_or_else(|| vec!["Off".into(), "On".into()]);
            (ParamKindView::Choice { options }, None)
        }
        CatalogKind::Enum => {
            if let Some(options) = catalog.choices(param) {
                (
                    ParamKindView::Choice {
                        options: options.to_vec(),
                    },
                    None,
                )
            } else {
                (
                    ParamKindView::Number {
                        min: catalog.format(param, param.min),
                        max: catalog.format(param, param.max),
                    },
                    None,
                )
            }
        }
        CatalogKind::Continuous => (
            ParamKindView::Number {
                min: catalog.format(param, param.min),
                max: catalog.format(param, param.max),
            },
            None,
        ),
    };

    let lock_reason = assignment_lock.or(unsupported_reason);
    let locked = lock_reason.is_some();

    ParamView {
        index,
        name: param.name.clone(),
        value,
        native_value: native,
        wire_kind: param.kind,
        kind,
        locked,
        lock_reason,
    }
}

/// 現在のチェーン状態を AI に見せるテキストへ整形する。
pub fn render_chain(snap: &ChainSnapshot) -> String {
    let mut out = String::new();
    let header = if snap.slot_label.is_empty() {
        snap.preset_name.clone()
    } else {
        format!("{} {}", snap.slot_label, snap.preset_name)
    };
    out.push_str(&format!("プリセット: {header}"));
    if let Some(tempo) = snap.tempo {
        out.push_str(&format!(" (Tempo: {tempo:.1} BPM)"));
    }
    out.push('\n');
    out.push_str(&format!(
        "空きブロックスロット: {} 枠\n",
        snap.free_block_slots
    ));

    if snap.blocks.is_empty() {
        out.push_str("(ブロックがありません)\n");
        return out;
    }

    for b in &snap.blocks {
        let state = if b.enabled { "ON" } else { "BYPASSED (OFF)" };
        let lock = if b.locked {
            " [割り当て済み: 削除・差し替え不可]"
        } else {
            ""
        };
        out.push_str(&format!("\n[{}] {} [{}]{lock}\n", b.id, b.name, state));
        for p in &b.params {
            let range = match &p.kind {
                ParamKindView::Number { min, max } => format!(" ({min}〜{max})"),
                ParamKindView::Choice { options } => format!(" [{}]", options.join(" / ")),
                ParamKindView::ReadOnly => " (変更不可)".into(),
            };
            let lock = if p.locked && !matches!(p.kind, ParamKindView::ReadOnly) {
                " [固定: 変更不可]"
            } else {
                ""
            };
            out.push_str(&format!("  - {}: {}{range}{lock}\n", p.name, p.value));
        }
        for note in &b.readonly_notes {
            out.push_str(&format!("  - (参照のみ) {note}\n"));
        }
    }
    out
}

/// 変更のリストを検証し、すべて有効な場合だけ書き込み計画を返す。
/// 1 つでも不正 (範囲外・存在しない・固定・重複) があればエラーを返し、1 つも適用しない。
pub fn plan_changes(
    snap: &ChainSnapshot,
    changes: &[Change],
    catalog: &Catalog,
) -> Result<Vec<PlannedWrite>, String> {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut planned = Vec::with_capacity(changes.len());

    for c in changes {
        let block = snap
            .blocks
            .iter()
            .find(|b| b.id == c.block)
            .ok_or_else(|| format!("ブロック '{}' が現在のチェーンに見つかりません", c.block))?;

        let param = block
            .params
            .iter()
            .find(|p| p.name == c.parameter)
            .or_else(|| {
                block
                    .params
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&c.parameter))
            })
            .ok_or_else(|| {
                format!(
                    "ブロック '{}' ({}) にパラメータ '{}' がありません",
                    block.id, block.name, c.parameter
                )
            })?;

        let key = (block.id.clone(), param.name.clone());
        if !seen.insert(key) {
            return Err(format!(
                "{} / {} の変更が 1 回の中に重複しています",
                block.id, param.name
            ));
        }

        if param.locked {
            let reason = param
                .lock_reason
                .as_deref()
                .unwrap_or("このパラメータは固定されています");
            return Err(format!(
                "{} / {} は変更できません ({reason})",
                block.id, param.name
            ));
        }

        let cat_param = catalog
            .param(block.model_number, param.index)
            .ok_or_else(|| {
                format!(
                    "{} / {} の定義がカタログに見つかりません",
                    block.id, param.name
                )
            })?;

        let native = catalog.parse(cat_param, &c.value).ok_or_else(|| {
            let hint = match &param.kind {
                ParamKindView::Number { min, max } => format!("範囲: {min}〜{max}"),
                ParamKindView::Choice { options } => {
                    format!("選択肢: {}", options.join(" / "))
                }
                ParamKindView::ReadOnly => "変更不可".into(),
            };
            format!(
                "{} / {} の値 '{}' を解釈できないか範囲外です ({hint})",
                block.id, param.name, c.value
            )
        })?;

        if !cat_param.accepts(native) {
            return Err(format!(
                "{} / {} の値 '{}' が範囲外です",
                block.id, param.name, c.value
            ));
        }

        let new_display = catalog.format(cat_param, native);
        planned.push(PlannedWrite {
            slot: block.slot,
            block_id: block.id.clone(),
            block_name: block.name.clone(),
            param_index: param.index,
            param_name: param.name.clone(),
            previous_display: param.value.clone(),
            previous_native: param.native_value,
            new_display,
            new_native: native,
            wire_kind: param.wire_kind,
        });
    }

    Ok(planned)
}

/// 名前・ステレオ指定・Amp+Cab 指定から解決したカタログ上のモデル情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub model_number: u32,
    pub paired_cab: Option<u32>,
    pub display_name: String,
    pub is_stereo: bool,
}

fn find_symbol_for_model<'a>(
    catalog: &'a Catalog,
    model_id: &str,
    stereo: Option<bool>,
    model_name: &str,
) -> Result<&'a hx_catalog::Symbol, String> {
    let mut candidates: Vec<_> = catalog
        .symbols()
        .iter()
        .filter(|s| s.model.as_deref() == Some(model_id))
        .collect();
    candidates.sort_by_key(|s| s.number);
    if candidates.is_empty() {
        return Err(format!(
            "モデル '{model_name}' のデバイス番号が見つかりません"
        ));
    }
    match stereo {
        Some(true) => candidates
            .into_iter()
            .find(|s| s.symbol.ends_with("Stereo"))
            .ok_or_else(|| format!("モデル '{model_name}' には Stereo 版がありません")),
        Some(false) => candidates
            .into_iter()
            .find(|s| !s.symbol.ends_with("Stereo"))
            .ok_or_else(|| format!("モデル '{model_name}' には Mono 版がありません")),
        None => Ok(candidates
            .iter()
            .copied()
            .find(|s| !s.symbol.ends_with("Stereo"))
            .unwrap_or(candidates[0])),
    }
}

/// モデル名・ステレオ指定・Amp+Cab 指定から実機のモデル番号を解決する。
pub fn resolve_model(
    catalog: &Catalog,
    name: &str,
    stereo: Option<bool>,
    amp_cab: bool,
) -> Result<ResolvedModel, String> {
    let trimmed = name.trim();
    let (base_name, inferred_stereo) = if let Some(stripped) = trimmed
        .strip_suffix("(Stereo)")
        .or_else(|| trimmed.strip_suffix("(stereo)"))
    {
        (stripped.trim(), Some(true))
    } else if let Some(stripped) = trimmed
        .strip_suffix("(Mono)")
        .or_else(|| trimmed.strip_suffix("(mono)"))
    {
        (stripped.trim(), Some(false))
    } else {
        (trimmed, None)
    };
    let effective_stereo = stereo.or(inferred_stereo);
    if base_name.is_empty() {
        return Err("モデル名が空です".into());
    }

    let mut matches: Vec<_> = catalog
        .models()
        .filter(|m| m.name.eq_ignore_ascii_case(base_name) || m.id.eq_ignore_ascii_case(base_name))
        .collect();
    matches.sort_by(|a, b| a.id.cmp(&b.id));
    if matches.is_empty() {
        return Err(format!(
            "モデル '{base_name}' が HX Edit のカタログに見つかりません (search_effects で正確な名前を確認してください)"
        ));
    }
    let model = if amp_cab {
        matches
            .iter()
            .copied()
            .find(|m| m.cab_link.is_some())
            .unwrap_or(matches[0])
    } else {
        matches[0]
    };

    let sym = find_symbol_for_model(catalog, &model.id, effective_stereo, &model.name)?;
    let paired_cab = if amp_cab {
        let cab = catalog.paired_cab(model).ok_or_else(|| {
            format!(
                "モデル '{}' にはペアの Cab 定義がありません (amp_cab を false にしてください)",
                model.name
            )
        })?;
        let cab_sym = find_symbol_for_model(catalog, &cab.id, None, &cab.name)?;
        Some(cab_sym.number)
    } else {
        None
    };

    let display_name = if paired_cab.is_some() {
        format!("{} (Amp+Cab)", model.name)
    } else {
        model.name.clone()
    };

    Ok(ResolvedModel {
        model_number: sym.number,
        paired_cab,
        display_name,
        is_stereo: sym.symbol.ends_with("Stereo"),
    })
}

fn apply_planned_writes(
    port: &mut dyn HxPort,
    planned: Vec<PlannedWrite>,
) -> Result<Vec<AppliedChange>, (Vec<AppliedChange>, String)> {
    let mut applied = Vec::with_capacity(planned.len());
    for w in planned {
        let wire = w.wire_kind.wire(w.new_native);
        if let Err(e) = port.write_param(w.slot, w.param_index, wire) {
            return Err((applied, e));
        }
        applied.push(AppliedChange::from(w));
    }
    Ok(applied)
}

/// 軽量な編集変更 (パラメータ変更・ON/OFF) の Undo 要素。
#[derive(Debug, Clone, PartialEq)]
pub enum AppliedEdit {
    Param(AppliedChange),
    Enabled {
        slot: usize,
        block_id: String,
        block_name: String,
        previous_enabled: bool,
        new_enabled: bool,
    },
}

/// 画面表示やログ向けの 1 件の実行記録。
#[derive(Debug, Clone, PartialEq)]
pub enum AppliedStep {
    Param(AppliedChange),
    Enabled {
        block_id: String,
        previous_enabled: bool,
        new_enabled: bool,
    },
    Removed {
        block_id: String,
        previous_name: String,
    },
    Replaced {
        block_id: String,
        previous_name: String,
        new_name: String,
    },
    Added {
        block_id: String,
        model_name: String,
    },
}

/// 1 サイクル分の Undo データ。
#[derive(Debug, Clone, PartialEq)]
pub enum UndoEntry {
    /// パラメータ変更・ON/OFF のみ: 個別に逆順で戻す。
    Edits(Vec<AppliedEdit>),
    /// ブロック追加・削除・モデル差し替えを含む: 変更前の編集バッファ丸ごと戻す。
    PresetSnapshot {
        before_bytes: Vec<u8>,
        summary: String,
    },
}

/// `apply_operations` の結果。
#[derive(Debug, Clone, PartialEq)]
pub struct OperationsOutcome {
    pub steps: Vec<AppliedStep>,
    pub undo: Option<UndoEntry>,
    pub structure_changed: bool,
    pub added_or_replaced: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
enum PlannedOp {
    SetEnabled {
        slot: usize,
        block_id: String,
        block_name: String,
        previous_enabled: bool,
        enabled: bool,
    },
    Replace {
        slot: usize,
        block_id: String,
        previous_name: String,
        model: ResolvedModel,
    },
    Remove {
        slot: usize,
        block_id: String,
        previous_name: String,
    },
    Add {
        position: BlockPosition,
        model: ResolvedModel,
    },
}

fn ensure_block_unlocked(b: &BlockView, action: &str) -> Result<(), String> {
    if b.locked {
        let reason = b
            .lock_reason
            .as_deref()
            .unwrap_or("割り当てがあるため操作できません");
        return Err(format!(
            "{} ({}) は{action}できません ({reason})",
            b.id, b.name
        ));
    }
    Ok(())
}

fn find_block<'a>(snap: &'a ChainSnapshot, id: &str) -> Result<&'a BlockView, String> {
    snap.blocks
        .iter()
        .find(|b| b.id == id)
        .ok_or_else(|| format!("ブロック '{id}' が現在のチェーンに見つかりません"))
}

fn plan_operations(
    snap: &ChainSnapshot,
    changes: &[Change],
    ops: &[BlockOp],
    catalog: &Catalog,
) -> Result<(Vec<PlannedWrite>, Vec<PlannedOp>), String> {
    if changes.len() + ops.len() > MAX_OPERATIONS {
        return Err(format!(
            "1 回の操作が多すぎます ({} 件; 上限 {MAX_OPERATIONS} 件)",
            changes.len() + ops.len()
        ));
    }

    let planned_params = plan_changes(snap, changes, catalog)?;
    let mut removed_blocks: HashSet<String> = HashSet::new();
    let mut replaced_blocks: HashSet<String> = HashSet::new();
    let mut toggled_blocks: HashSet<String> = HashSet::new();

    for op in ops {
        match op {
            BlockOp::Remove { block } => {
                if !removed_blocks.insert(block.clone()) {
                    return Err(format!("{block} の削除が 1 回の中に重複しています"));
                }
            }
            BlockOp::Replace { block, .. } => {
                if !replaced_blocks.insert(block.clone()) {
                    return Err(format!("{block} のモデル変更が 1 回の中に重複しています"));
                }
            }
            BlockOp::SetEnabled { block, .. } => {
                if !toggled_blocks.insert(block.clone()) {
                    return Err(format!(
                        "{block} の ON/OFF 切り替えが 1 回の中に重複しています"
                    ));
                }
            }
            BlockOp::Add { .. } => {}
        }
    }

    for c in changes {
        if removed_blocks.contains(&c.block) {
            return Err(format!(
                "{} は削除予定のためパラメータ変更を同時に行えません",
                c.block
            ));
        }
        if replaced_blocks.contains(&c.block) {
            return Err(format!(
                "{} はモデル変更後のパラメータ再取得が必要なため、差し替え後に変更してください",
                c.block
            ));
        }
    }

    let mut add_count = 0usize;
    let mut planned_ops = Vec::with_capacity(ops.len());
    for op in ops {
        match op {
            BlockOp::SetEnabled { block, enabled } => {
                let b = find_block(snap, block)?;
                if removed_blocks.contains(&b.id) {
                    return Err(format!("{} は削除予定のため ON/OFF できません", b.id));
                }
                planned_ops.push(PlannedOp::SetEnabled {
                    slot: b.slot,
                    block_id: b.id.clone(),
                    block_name: b.name.clone(),
                    previous_enabled: b.enabled,
                    enabled: *enabled,
                });
            }
            BlockOp::Remove { block } => {
                let b = find_block(snap, block)?;
                if replaced_blocks.contains(&b.id) {
                    return Err(format!("{} に削除とモデル変更を同時に行えません", b.id));
                }
                ensure_block_unlocked(b, "削除")?;
                planned_ops.push(PlannedOp::Remove {
                    slot: b.slot,
                    block_id: b.id.clone(),
                    previous_name: b.name.clone(),
                });
            }
            BlockOp::Replace {
                block,
                model,
                stereo,
                amp_cab,
            } => {
                let b = find_block(snap, block)?;
                ensure_block_unlocked(b, "モデル変更")?;
                let resolved = resolve_model(catalog, model, *stereo, *amp_cab)?;
                planned_ops.push(PlannedOp::Replace {
                    slot: b.slot,
                    block_id: b.id.clone(),
                    previous_name: b.name.clone(),
                    model: resolved,
                });
            }
            BlockOp::Add {
                model,
                position,
                stereo,
                amp_cab,
            } => {
                add_count += 1;
                let resolved = resolve_model(catalog, model, *stereo, *amp_cab)?;
                match position {
                    BlockPosition::Before(target) | BlockPosition::After(target) => {
                        let b = find_block(snap, target)?;
                        if removed_blocks.contains(&b.id) {
                            return Err(format!(
                                "削除予定のブロック {} を基準に追加位置を指定できません",
                                b.id
                            ));
                        }
                    }
                    BlockPosition::End => {}
                }
                planned_ops.push(PlannedOp::Add {
                    position: position.clone(),
                    model: resolved,
                });
            }
        }
    }

    if add_count > 0 {
        if snap.raw_preset.is_none() {
            return Err("ブロック追加に必要なプリセット構造情報がありません".into());
        }
        if snap.free_block_slots + removed_blocks.len() < add_count {
            return Err(format!(
                "空きブロックスロットが足りません (追加 {add_count} 個、空き {} 枠)",
                snap.free_block_slots + removed_blocks.len()
            ));
        }
    }

    Ok((planned_params, planned_ops))
}

fn resolve_insert_anchor(
    preset: &Preset,
    position: &BlockPosition,
    slot_map: &HashMap<usize, usize>,
) -> Result<usize, String> {
    let layout = preset.layout();
    match position {
        BlockPosition::End => layout
            .paths
            .first()
            .and_then(|p| p.output)
            .ok_or_else(|| "プリセットの Output 位置が見つかりません".to_string()),
        BlockPosition::Before(id) => {
            let orig = parse_block_slot(id)?;
            slot_map
                .get(&orig)
                .copied()
                .ok_or_else(|| format!("配置基準ブロック {id} の位置を追跡できません"))
        }
        BlockPosition::After(id) => {
            let orig = parse_block_slot(id)?;
            let cur = slot_map
                .get(&orig)
                .copied()
                .ok_or_else(|| format!("配置基準ブロック {id} の位置を追跡できません"))?;
            Ok(cur + 1)
        }
    }
}

fn parse_block_slot(id: &str) -> Result<usize, String> {
    id.strip_prefix("block")
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(|| format!("ブロック名 '{id}' の形式が不正です"))
}

fn occupied_block_slots(preset: &Preset) -> Vec<usize> {
    preset.blocks().map(|(i, _)| i).collect()
}

fn finish_outcome(
    steps: Vec<AppliedStep>,
    edits: Vec<AppliedEdit>,
    before_bytes: Option<Vec<u8>>,
    structure_applied: bool,
    added_or_replaced: bool,
    error: Option<String>,
) -> OperationsOutcome {
    let undo = if steps.is_empty() {
        None
    } else if structure_applied {
        before_bytes.map(|bytes| UndoEntry::PresetSnapshot {
            before_bytes: bytes,
            summary: steps
                .iter()
                .map(|s| match s {
                    AppliedStep::Added {
                        block_id,
                        model_name,
                    } => {
                        format!("{block_id}: {model_name} 追加")
                    }
                    AppliedStep::Removed {
                        block_id,
                        previous_name,
                    } => format!("{block_id}: {previous_name} 削除"),
                    AppliedStep::Replaced {
                        block_id,
                        previous_name,
                        new_name,
                    } => format!("{block_id}: {previous_name} → {new_name}"),
                    AppliedStep::Enabled {
                        block_id,
                        new_enabled,
                        ..
                    } => format!("{block_id}: {}", if *new_enabled { "ON" } else { "OFF" }),
                    AppliedStep::Param(c) => {
                        format!("{} / {} = {}", c.block_id, c.param_name, c.new_display)
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        })
    } else {
        Some(UndoEntry::Edits(edits))
    };

    let err_msg = error.map(|e| {
        if steps.is_empty() {
            e
        } else {
            format!("{} 件実行したところで失敗しました: {e}", steps.len())
        }
    });

    OperationsOutcome {
        steps,
        undo,
        structure_changed: structure_applied,
        added_or_replaced,
        error: err_msg,
    }
}

/// パラメータ変更とブロック操作 (ON/OFF・追加・削除・差し替え) を検証して実行する。
/// 検証エラー時は 1 バイトも書き込まず `Err` を返す。
pub fn apply_operations(
    port: &mut dyn HxPort,
    snap: &ChainSnapshot,
    changes: &[Change],
    ops: &[BlockOp],
) -> Result<OperationsOutcome, String> {
    let (planned_params, planned_ops) = plan_operations(snap, changes, ops, port.catalog())?;
    let before_bytes = snap.raw_preset.clone();

    let mut steps = Vec::new();
    let mut edits = Vec::new();
    let mut structure_applied = false;
    let mut added_or_replaced = false;
    let mut touched_before_add = false;

    match apply_planned_writes(port, planned_params) {
        Ok(applied) => {
            for a in applied {
                touched_before_add = true;
                steps.push(AppliedStep::Param(a.clone()));
                edits.push(AppliedEdit::Param(a));
            }
        }
        Err((partial, e)) => {
            for a in partial {
                steps.push(AppliedStep::Param(a.clone()));
                edits.push(AppliedEdit::Param(a));
            }
            return Ok(finish_outcome(
                steps,
                edits,
                before_bytes,
                false,
                false,
                Some(e),
            ));
        }
    }

    let mut enables = Vec::new();
    let mut replaces = Vec::new();
    let mut removes = Vec::new();
    let mut adds = Vec::new();
    for op in planned_ops {
        match op {
            PlannedOp::SetEnabled {
                slot,
                block_id,
                block_name,
                previous_enabled,
                enabled,
            } => enables.push((slot, block_id, block_name, previous_enabled, enabled)),
            PlannedOp::Replace {
                slot,
                block_id,
                previous_name,
                model,
            } => replaces.push((slot, block_id, previous_name, model)),
            PlannedOp::Remove {
                slot,
                block_id,
                previous_name,
            } => removes.push((slot, block_id, previous_name)),
            PlannedOp::Add { position, model } => adds.push((position, model)),
        }
    }

    for (slot, block_id, block_name, previous_enabled, enabled) in enables {
        if let Err(e) = port.set_enabled(slot, enabled) {
            return Ok(finish_outcome(
                steps,
                edits,
                before_bytes,
                structure_applied,
                added_or_replaced,
                Some(e),
            ));
        }
        touched_before_add = true;
        steps.push(AppliedStep::Enabled {
            block_id: block_id.clone(),
            previous_enabled,
            new_enabled: enabled,
        });
        edits.push(AppliedEdit::Enabled {
            slot,
            block_id,
            block_name,
            previous_enabled,
            new_enabled: enabled,
        });
    }

    for (slot, block_id, previous_name, model) in replaces {
        if let Err(e) = port.set_model(slot, model.model_number, model.paired_cab) {
            return Ok(finish_outcome(
                steps,
                edits,
                before_bytes,
                structure_applied,
                added_or_replaced,
                Some(e),
            ));
        }
        structure_applied = true;
        added_or_replaced = true;
        touched_before_add = true;
        steps.push(AppliedStep::Replaced {
            block_id,
            previous_name,
            new_name: model.display_name,
        });
    }

    for (slot, block_id, previous_name) in removes {
        if let Err(e) = port.clear_block(slot) {
            return Ok(finish_outcome(
                steps,
                edits,
                before_bytes,
                structure_applied,
                added_or_replaced,
                Some(e),
            ));
        }
        structure_applied = true;
        touched_before_add = true;
        steps.push(AppliedStep::Removed {
            block_id,
            previous_name,
        });
    }

    if !adds.is_empty() {
        // 先に実行した変更を古いドキュメントで上書きしないよう、変更済みなら最新状態を読み直す
        let mut preset = if touched_before_add {
            match port.read_snapshot() {
                Ok(fresh) => {
                    let bytes = fresh.raw_preset.ok_or_else(|| {
                        "ブロック追加後の最新プリセット構造を取得できませんでした".to_string()
                    });
                    match bytes.and_then(|b| {
                        Preset::parse(&b)
                            .ok_or_else(|| "読み直したプリセットを解析できませんでした".to_string())
                    }) {
                        Ok(p) => p,
                        Err(e) => {
                            return Ok(finish_outcome(
                                steps,
                                edits,
                                before_bytes,
                                structure_applied,
                                added_or_replaced,
                                Some(e),
                            ));
                        }
                    }
                }
                Err(e) => {
                    return Ok(finish_outcome(
                        steps,
                        edits,
                        before_bytes,
                        structure_applied,
                        added_or_replaced,
                        Some(e),
                    ));
                }
            }
        } else {
            let bytes = before_bytes
                .as_deref()
                .ok_or_else(|| "プリセット構造情報がありません".to_string())?;
            Preset::parse(bytes).ok_or_else(|| "プリセットを解析できませんでした".to_string())?
        };

        let mut slot_map: HashMap<usize, usize> =
            snap.blocks.iter().map(|b| (b.slot, b.slot)).collect();

        for (add_index, (position, model)) in adds.into_iter().enumerate() {
            if add_index > 0 {
                // 2 つ目以降の追加では前の set_model を反映した最新ドキュメントを読む
                match port.read_snapshot() {
                    Ok(fresh) => {
                        let Some(bytes) = fresh.raw_preset else {
                            return Ok(finish_outcome(
                                steps,
                                edits,
                                before_bytes,
                                structure_applied,
                                added_or_replaced,
                                Some("追加後のプリセット構造を取得できませんでした".into()),
                            ));
                        };
                        let Some(p) = Preset::parse(&bytes) else {
                            return Ok(finish_outcome(
                                steps,
                                edits,
                                before_bytes,
                                structure_applied,
                                added_or_replaced,
                                Some("読み直したプリセットを解析できませんでした".into()),
                            ));
                        };
                        preset = p;
                    }
                    Err(e) => {
                        return Ok(finish_outcome(
                            steps,
                            edits,
                            before_bytes,
                            structure_applied,
                            added_or_replaced,
                            Some(e),
                        ));
                    }
                }
            }

            let anchor = match resolve_insert_anchor(&preset, &position, &slot_map) {
                Ok(a) => a,
                Err(e) => {
                    return Ok(finish_outcome(
                        steps,
                        edits,
                        before_bytes,
                        structure_applied,
                        added_or_replaced,
                        Some(e),
                    ));
                }
            };
            let before_add_bytes = preset.encode();
            let before_occupied = occupied_block_slots(&preset);
            let Some(target) = preset.room_before(anchor) else {
                return Ok(finish_outcome(
                    steps,
                    edits,
                    before_bytes,
                    structure_applied,
                    added_or_replaced,
                    Some("このレーンに空きスロットがありません".into()),
                ));
            };
            let after_occupied = occupied_block_slots(&preset);
            let moved = preset.encode() != before_add_bytes;

            // スライド前後で占有スロット一覧の対応を取り、元のブロック識別子を追跡する
            let shift: HashMap<usize, usize> =
                before_occupied.into_iter().zip(after_occupied).collect();
            for cur in slot_map.values_mut() {
                if let Some(&next) = shift.get(cur) {
                    *cur = next;
                }
            }

            if moved {
                let moved_bytes = preset.encode();
                if let Err(e) = port.write_preset_bytes(&moved_bytes) {
                    return Ok(finish_outcome(
                        steps,
                        edits,
                        before_bytes,
                        structure_applied,
                        added_or_replaced,
                        Some(e),
                    ));
                }
                structure_applied = true;
            }

            if let Err(e) = port.set_model(target, model.model_number, model.paired_cab) {
                if moved {
                    // スライドだけ送った状態でモデル配置が拒否 (DSP 不足等) されたら、配置前の状態に戻す
                    let _ = port.write_preset_bytes(&before_add_bytes);
                }
                return Ok(finish_outcome(
                    steps,
                    edits,
                    before_bytes,
                    structure_applied,
                    added_or_replaced,
                    Some(e),
                ));
            }
            structure_applied = true;
            added_or_replaced = true;
            steps.push(AppliedStep::Added {
                block_id: format!("block{target}"),
                model_name: model.display_name,
            });
        }
    }

    Ok(finish_outcome(
        steps,
        edits,
        before_bytes,
        structure_applied,
        added_or_replaced,
        None,
    ))
}

fn restore_edits(port: &mut dyn HxPort, edits: &[AppliedEdit]) -> Vec<RestoreFailure> {
    let mut failures = Vec::new();
    for edit in edits.iter().rev() {
        match edit {
            AppliedEdit::Param(a) => {
                let wire = a.wire_kind.wire(a.previous_native);
                if let Err(reason) = port.write_param(a.slot, a.param_index, wire) {
                    failures.push(RestoreFailure {
                        block: a.block_id.clone(),
                        parameter: a.param_name.clone(),
                        value: a.previous_display.clone(),
                        reason,
                    });
                }
            }
            AppliedEdit::Enabled {
                slot,
                block_id,
                previous_enabled,
                ..
            } => {
                if let Err(reason) = port.set_enabled(*slot, *previous_enabled) {
                    failures.push(RestoreFailure {
                        block: block_id.clone(),
                        parameter: "状態 (ON/OFF)".into(),
                        value: if *previous_enabled { "ON" } else { "OFF" }.into(),
                        reason,
                    });
                }
            }
        }
    }
    failures
}

/// `UndoEntry` に記録された変更を実機へ戻す。戻せなかった項目だけ返す。
pub fn restore_undo(port: &mut dyn HxPort, entry: &UndoEntry) -> Vec<RestoreFailure> {
    match entry {
        UndoEntry::Edits(edits) => restore_edits(port, edits),
        UndoEntry::PresetSnapshot {
            before_bytes,
            summary,
        } => match port.write_preset_bytes(before_bytes) {
            Ok(()) => Vec::new(),
            Err(reason) => vec![RestoreFailure {
                block: "プリセット".into(),
                parameter: "編集バッファ復元".into(),
                value: summary.clone(),
                reason,
            }],
        },
    }
}

/// 実機の HX Stomp / HX Stomp XL と USB で通信するポート。
/// 1 度開いたセッションを保持し、切断時だけ再接続する。
pub struct UsbHxPort {
    catalog: Catalog,
    session: Option<hx_usb::Session>,
}

impl UsbHxPort {
    /// HX Edit のリソースからカタログを読み込んでポートを作る。USB 接続は最初の操作時に開く。
    pub fn new() -> Result<Self, String> {
        let catalog = Catalog::load().map_err(|e| {
            format!(
                "HX Edit のモデル定義を読めません ({e})。/Applications/Line6/HX Edit.app が入っているか確認してください"
            )
        })?;
        Ok(Self {
            catalog,
            session: None,
        })
    }

    /// USB 接続を明示的に解放する (HX Edit を使いたいとき用)。
    pub fn disconnect(&mut self) {
        self.session = None;
    }

    pub fn is_session_open(&self) -> bool {
        self.session.is_some()
    }

    /// 機器からの未読イベントを ACK しつつ keepalive を送る。失敗したらセッションを手放す。
    pub fn keepalive(&mut self) -> Result<(), String> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        let res = session
            .poll_notifications()
            .map(|_| ())
            .and_then(|_| session.keepalive());
        if let Err(err) = res {
            // poll_notifications / keepalive がエラーのときはトランスポート状態が信用できないため閉じる
            self.session = None;
            return Err(format_usb_error(err));
        }
        Ok(())
    }

    fn ensure_session(&mut self) -> Result<&mut hx_usb::Session, String> {
        if self.session.is_none() {
            let devices = hx_usb::list().map_err(|e| format!("USB デバイスを探せません: {e}"))?;
            let found = devices.into_iter().next().ok_or_else(|| {
                "HX Stomp / HX Stomp XL が USB で見つかりません (電源とケーブルを確認してください)"
                    .to_string()
            })?;
            let session = found.open().map_err(format_usb_error)?;
            self.session = Some(session);
        }
        Ok(self.session.as_mut().expect("session initialized"))
    }

    fn call_session<T>(
        &mut self,
        f: impl FnOnce(&mut hx_usb::Session) -> hx_usb::Result<T>,
    ) -> Result<T, String> {
        let _ = self.ensure_session()?;
        let res = f(self.session.as_mut().unwrap());
        match res {
            Ok(v) => Ok(v),
            Err(e) => {
                if e.loses_session() {
                    self.session = None;
                }
                Err(format_usb_error(e))
            }
        }
    }

    fn read_preset_settled(&mut self) -> Result<Preset, String> {
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(300));
            }
            let _ = self.ensure_session()?;
            match self.session.as_mut().unwrap().read_preset() {
                Ok(preset) => return Ok(preset),
                Err(e) if !e.loses_session() && attempt < 2 => continue,
                Err(e) => {
                    if e.loses_session() {
                        self.session = None;
                    }
                    return Err(format_usb_error(e));
                }
            }
        }
        unreachable!("loop returns by attempt 2")
    }
}

fn format_usb_error(err: hx_usb::Error) -> String {
    match err {
        hx_usb::Error::NotFound => "HX Stomp / HX Stomp XL が USB で見つかりません".into(),
        hx_usb::Error::Claim(detail) => format!(
            "HX Stomp の USB 制御をつかめません (HX Edit が開いていれば閉じてください): {detail}"
        ),
        hx_usb::Error::Device(-306) => {
            "HX Stomp の DSP 容量が足りず、このブロックを追加/変更できません".into()
        }
        other => format!("HX Stomp との USB 通信に失敗しました: {other}"),
    }
}

impl HxPort for UsbHxPort {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn read_snapshot(&mut self) -> Result<ChainSnapshot, String> {
        let preset = self.read_preset_settled()?;
        let (profile, (slot_label, preset_name)) = self.call_session(|s| {
            let profile = s.profile;
            let (_, index, name) = s.preset_info()?;
            Ok((profile, (profile.slot_label(index), name)))
        })?;

        Ok(snapshot_from_preset(
            &preset,
            &self.catalog,
            profile.controls,
            slot_label,
            preset_name,
        ))
    }

    fn write_param(
        &mut self,
        slot: usize,
        param_index: usize,
        value: WireValue,
    ) -> Result<(), String> {
        self.call_session(|s| s.set_param(slot as i64, param_index as i64, value))
    }

    fn set_enabled(&mut self, slot: usize, enabled: bool) -> Result<(), String> {
        self.call_session(|s| s.set_enabled(slot as i64, enabled))
    }

    fn clear_block(&mut self, slot: usize) -> Result<(), String> {
        self.call_session(|s| s.clear_block(slot as i64))
    }

    fn set_model(
        &mut self,
        slot: usize,
        model: u32,
        paired_cab: Option<u32>,
    ) -> Result<(), String> {
        self.call_session(|s| match paired_cab {
            Some(cab) => s.set_model_pair(slot as i64, model, cab),
            None => s.set_model(slot as i64, model),
        })
    }

    fn write_preset_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        let preset = Preset::parse(bytes)
            .ok_or_else(|| "復元・書き込み用のプリセットデータが不正です".to_string())?;
        self.call_session(|s| s.write_preset(&preset))
    }
}

/// バックグラウンド keepalive が叩く対象 (テストでは偽物に差し替える)。
pub trait KeepalivePort {
    fn keepalive(&mut self) -> Result<(), String>;
}

impl KeepalivePort for UsbHxPort {
    fn keepalive(&mut self) -> Result<(), String> {
        UsbHxPort::keepalive(self)
    }
}

/// 他の USB 読み書きでロックされていないときだけ 1 回 keepalive を進める。
pub fn keepalive_step<P: KeepalivePort>(port: &Mutex<Option<P>>) {
    match port.try_lock() {
        Ok(mut guard) => {
            if let Some(p) = guard.as_mut() {
                let _ = p.keepalive();
            }
        }
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Poisoned(e)) => {
            if let Some(p) = e.into_inner().as_mut() {
                let _ = p.keepalive();
            }
        }
    }
}

/// keepalive スレッドの寿命管理。Drop 時に即座に起こして終了を待つ。
pub struct KeepaliveGuard {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for KeepaliveGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

/// USB セッションがアイドルで切断されないよう、`interval` ごとに keepalive を送るスレッドを起動する。
pub fn spawn_keepalive<P: KeepalivePort + Send + 'static>(
    port: Arc<Mutex<Option<P>>>,
    interval: Duration,
) -> KeepaliveGuard {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            std::thread::park_timeout(interval);
            if flag.load(Ordering::SeqCst) {
                break;
            }
            keepalive_step(&port);
        }
    });
    KeepaliveGuard {
        stop,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(1);

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// テスト用の最小カタログ (HX Edit の Resources を模擬) を作る。
    fn test_catalog() -> (Catalog, TempDir) {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("hx-ai-cat-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let controls = r#"{
            "generic_knob": {
                "dspToDisplayScale": 10.0,
                "format": "%.1f"
            },
            "clipping_menu": {
                "format": ["Overdrive", "Boost", "Distortion"]
            },
            "bypass_switch": {
                "format": ["Off", "On"]
            }
        }"#;
        std::fs::write(dir.join("HelixControls.json"), controls).unwrap();

        let models = r#"[
            {
                "symbolicID": "Test_ToneSovereign",
                "name": "Tone Sovereign",
                "category": 1,
                "params": [
                    {
                        "symbolicID": "Gain1",
                        "name": "Gain 1",
                        "valueType": 1,
                        "displayType": "generic_knob",
                        "min": 0.0,
                        "max": 1.0,
                        "default": 0.5
                    },
                    {
                        "symbolicID": "Clipping1",
                        "name": "Clipping 1",
                        "valueType": 0,
                        "displayType": "clipping_menu",
                        "min": 0.0,
                        "max": 2.0,
                        "default": 0.0
                    },
                    {
                        "symbolicID": "Bright",
                        "name": "Bright",
                        "valueType": 2,
                        "displayType": "bypass_switch",
                        "min": 0.0,
                        "max": 1.0,
                        "default": 0.0
                    }
                ]
            },
            {
                "symbolicID": "Test_DynamicHall",
                "name": "Dynamic Hall",
                "category": 2,
                "params": [
                    {
                        "symbolicID": "Mix",
                        "name": "Mix",
                        "valueType": 1,
                        "displayType": "generic_knob",
                        "min": 0.0,
                        "max": 1.0,
                        "default": 0.35
                    }
                ]
            },
            {
                "symbolicID": "Test_BritJ45Brt",
                "name": "Brit J45 Brt",
                "category": 3,
                "cablink": "Test_CabDeluxe",
                "params": []
            },
            {
                "symbolicID": "Test_CabDeluxe",
                "name": "1x12 US Deluxe",
                "category": 4,
                "params": []
            },
            {
                "symbolicID": "Test_StereoOnly",
                "name": "Stereo Only",
                "category": 2,
                "params": []
            }
        ]"#;
        std::fs::write(dir.join("distortion.models"), models).unwrap();

        let browse = r#"{
            "categories": [
                {
                    "id": 1,
                    "name": "Distortion",
                    "shortName": "Dist",
                    "color": "0xf5901e",
                    "models": [
                        { "id": "Test_ToneSovereign" }
                    ],
                    "subcategories": []
                },
                {
                    "id": 2,
                    "name": "Reverb",
                    "shortName": "Verb",
                    "color": "0xff7722",
                    "models": [
                        { "id": "Test_DynamicHall" },
                        { "id": "Test_StereoOnly" }
                    ],
                    "subcategories": []
                },
                {
                    "id": 3,
                    "name": "Amp",
                    "shortName": "Amp",
                    "color": "0xdd1111",
                    "models": [
                        { "id": "Test_BritJ45Brt" }
                    ],
                    "subcategories": []
                },
                {
                    "id": 4,
                    "name": "Cab",
                    "shortName": "Cab",
                    "color": "0x888888",
                    "models": [
                        { "id": "Test_CabDeluxe" }
                    ],
                    "subcategories": []
                }
            ]
        }"#;
        std::fs::write(dir.join("HX_ModelCatalog.json"), browse).unwrap();

        let sym = r#"[
            {
                "symbol": "Test_ToneSovereign",
                "parameters": ["Gain1", "Clipping1", "Bright"]
            },
            {
                "symbol": "Test_DynamicHallMono",
                "model": "Test_DynamicHall",
                "parameters": ["Mix"]
            },
            {
                "symbol": "Test_DynamicHallStereo",
                "model": "Test_DynamicHall",
                "parameters": ["Mix"]
            },
            {
                "symbol": "Test_BritJ45Brt",
                "parameters": []
            },
            {
                "symbol": "Test_CabDeluxe",
                "parameters": []
            },
            {
                "symbol": "Test_StereoOnlyStereo",
                "model": "Test_StereoOnly",
                "parameters": []
            }
        ]"#;
        std::fs::write(dir.join("Helix.sym"), sym).unwrap();

        let catalog = Catalog::load_from(&dir).unwrap();
        (catalog, TempDir(dir))
    }

    #[derive(Debug, Clone, PartialEq)]
    enum FakeEvent {
        Param(usize, usize, WireValue),
        Enabled(usize, bool),
        Clear(usize),
        Model(usize, u32, Option<u32>),
        WritePreset,
    }

    struct FakePort {
        catalog: Catalog,
        snapshot: ChainSnapshot,
        writes: Vec<(usize, usize, WireValue)>,
        events: Vec<FakeEvent>,
        read_calls: usize,
        write_calls: usize,
        fail_on_write: Option<usize>,
        fail_on_set_model: Option<String>,
        fail_on_write_preset: Option<String>,
    }

    impl FakePort {
        fn new(catalog: Catalog, snapshot: ChainSnapshot) -> Self {
            Self {
                catalog,
                snapshot,
                writes: vec![],
                events: vec![],
                read_calls: 0,
                write_calls: 0,
                fail_on_write: None,
                fail_on_set_model: None,
                fail_on_write_preset: None,
            }
        }
    }

    impl HxPort for FakePort {
        fn read_snapshot(&mut self) -> Result<ChainSnapshot, String> {
            self.read_calls += 1;
            Ok(self.snapshot.clone())
        }
        fn write_param(
            &mut self,
            slot: usize,
            param_index: usize,
            value: WireValue,
        ) -> Result<(), String> {
            let call = self.write_calls;
            self.write_calls += 1;
            if self.fail_on_write == Some(call) {
                return Err("USB write error".into());
            }
            self.writes.push((slot, param_index, value.clone()));
            self.events.push(FakeEvent::Param(slot, param_index, value));
            Ok(())
        }
        fn set_enabled(&mut self, slot: usize, enabled: bool) -> Result<(), String> {
            self.events.push(FakeEvent::Enabled(slot, enabled));
            Ok(())
        }
        fn clear_block(&mut self, slot: usize) -> Result<(), String> {
            self.events.push(FakeEvent::Clear(slot));
            if let Some(bytes) = self.snapshot.raw_preset.as_deref()
                && let Some(mut p) = Preset::parse(bytes)
                && let Some(hx_proto::msgpack::Value::Array(slots)) = p.tone.at_mut(&[0, 22])
                && let Some(s) = slots.get_mut(slot)
            {
                *s = hx_proto::msgmap! { 19 => hx_proto::msgpack::Value::Int(8), 20 => hx_proto::msgpack::Value::Nil };
                let refreshed = Preset::parse(&p.encode()).expect("valid cleared preset");
                self.snapshot = snapshot_from_preset(
                    &refreshed,
                    &self.catalog,
                    rpc::ControlLayout::Stomp,
                    self.snapshot.slot_label.clone(),
                    self.snapshot.preset_name.clone(),
                );
            }
            Ok(())
        }
        fn set_model(
            &mut self,
            slot: usize,
            model: u32,
            paired_cab: Option<u32>,
        ) -> Result<(), String> {
            if let Some(err) = &self.fail_on_set_model {
                return Err(err.clone());
            }
            self.events.push(FakeEvent::Model(slot, model, paired_cab));
            if let Some(bytes) = self.snapshot.raw_preset.as_deref()
                && let Some(mut p) = Preset::parse(bytes)
                && let Some(hx_proto::msgpack::Value::Array(slots)) = p.tone.at_mut(&[0, 22])
                && let Some(s) = slots.get_mut(slot)
            {
                let ref_map = hx_proto::msgmap! {
                    23 => hx_proto::msgpack::Value::Bool(paired_cab.is_some()),
                    25 => hx_proto::msgpack::Value::Int(model as i64),
                    26 => hx_proto::msgpack::Value::Int(paired_cab.map_or(-1, |c| c as i64)),
                };
                let empty_vals = || {
                    hx_proto::msgmap! {
                        2 => hx_proto::msgpack::Value::Int(0),
                        3 => hx_proto::msgpack::Value::Int(0),
                        4 => hx_proto::msgpack::Value::Array(vec![]),
                    }
                };
                let body = if paired_cab.is_some() {
                    hx_proto::msgmap! {
                        9 => hx_proto::msgpack::Value::Int(0),
                        10 => hx_proto::msgpack::Value::Bool(true),
                        24 => ref_map,
                        11 => empty_vals(),
                        12 => empty_vals(),
                    }
                } else {
                    hx_proto::msgmap! {
                        9 => hx_proto::msgpack::Value::Int(0),
                        10 => hx_proto::msgpack::Value::Bool(true),
                        24 => ref_map,
                        11 => empty_vals(),
                    }
                };
                *s = hx_proto::msgmap! {
                    19 => hx_proto::msgpack::Value::Int(6),
                    20 => body,
                };
                let refreshed = Preset::parse(&p.encode()).expect("valid model preset");
                self.snapshot = snapshot_from_preset(
                    &refreshed,
                    &self.catalog,
                    rpc::ControlLayout::Stomp,
                    self.snapshot.slot_label.clone(),
                    self.snapshot.preset_name.clone(),
                );
            }
            Ok(())
        }
        fn write_preset_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
            if let Some(err) = &self.fail_on_write_preset {
                return Err(err.clone());
            }
            self.events.push(FakeEvent::WritePreset);
            if let Some(p) = Preset::parse(bytes) {
                self.snapshot = snapshot_from_preset(
                    &p,
                    &self.catalog,
                    rpc::ControlLayout::Stomp,
                    self.snapshot.slot_label.clone(),
                    self.snapshot.preset_name.clone(),
                );
            }
            Ok(())
        }
        fn catalog(&self) -> &Catalog {
            &self.catalog
        }
    }

    fn sample_snapshot(catalog: &Catalog, lock_gain: bool) -> ChainSnapshot {
        let params = vec![
            build_param_view(
                catalog,
                0,
                0,
                0.22,
                lock_gain.then(|| "コントローラーに割り当て済み".into()),
            ),
            build_param_view(catalog, 0, 1, 1.0, None),
            build_param_view(catalog, 0, 2, 0.0, None),
        ];
        ChainSnapshot {
            slot_label: "28B".into(),
            preset_name: "Test Preset".into(),
            tempo: Some(120.0),
            snapshots: vec!["SNAPSHOT 1".into()],
            free_block_slots: 3,
            raw_preset: None,
            blocks: vec![BlockView {
                id: "block2".into(),
                slot: 2,
                model_number: 0,
                name: "Tone Sovereign".into(),
                category: "Dist".into(),
                color_hex: "#f5901e".into(),
                enabled: true,
                locked: false,
                lock_reason: None,
                params,
                readonly_notes: vec![],
            }],
        }
    }

    #[test]
    fn renders_chain_with_ranges_choices_and_locks() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, true);
        let text = render_chain(&snap);
        assert!(text.contains("プリセット: 28B Test Preset (Tempo: 120.0 BPM)"));
        assert!(text.contains("[block2] Tone Sovereign [ON]"));
        assert!(text.contains("Gain 1: 2.2 (0.0〜10.0) [固定: 変更不可]"));
        assert!(text.contains("Clipping 1: Boost [Overdrive / Boost / Distortion]"));
        assert!(text.contains("Bright: Off [Off / On]"));
    }

    #[test]
    fn plans_valid_changes_for_number_enum_and_switch() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let changes = vec![
            Change {
                block: "block2".into(),
                parameter: "Gain 1".into(),
                value: "7.5".into(),
            },
            Change {
                block: "block2".into(),
                parameter: "Clipping 1".into(),
                value: "Distortion".into(),
            },
            Change {
                block: "block2".into(),
                parameter: "Bright".into(),
                value: "On".into(),
            },
        ];
        let plan = plan_changes(&snap, &changes, &catalog).unwrap();
        assert_eq!(plan.len(), 3);
        assert!((plan[0].new_native - 0.75).abs() < 1e-5);
        assert_eq!(plan[0].new_display, "7.5");
        assert_eq!(plan[1].new_native, 2.0);
        assert_eq!(plan[1].new_display, "Distortion");
        assert_eq!(plan[2].new_native, 1.0);
        assert_eq!(plan[2].new_display, "On");
    }

    #[test]
    fn rejects_out_of_range_without_clamping() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let err = plan_changes(
            &snap,
            &[Change {
                block: "block2".into(),
                parameter: "Gain 1".into(),
                value: "11.0".into(),
            }],
            &catalog,
        )
        .unwrap_err();
        assert!(err.contains("範囲外") || err.contains("解釈"), "{err}");
    }

    #[test]
    fn rejects_locked_parameter() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, true);
        let err = plan_changes(
            &snap,
            &[Change {
                block: "block2".into(),
                parameter: "Gain 1".into(),
                value: "5.0".into(),
            }],
            &catalog,
        )
        .unwrap_err();
        assert!(
            err.contains("変更できません") && err.contains("コントローラー"),
            "{err}"
        );
    }

    #[test]
    fn rejects_duplicate_parameter_in_one_batch() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let err = plan_changes(
            &snap,
            &[
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "3.0".into(),
                },
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "4.0".into(),
                },
            ],
            &catalog,
        )
        .unwrap_err();
        assert!(err.contains("重複"), "{err}");
    }

    #[test]
    fn apply_writes_nothing_if_second_change_is_invalid() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort::new(catalog, snap.clone());
        let err = apply_operations(
            &mut port,
            &snap,
            &[
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "5.0".into(),
                },
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "99.0".into(),
                },
            ],
            &[],
        )
        .unwrap_err();
        assert!(!err.is_empty());
        assert!(port.writes.is_empty(), "1 つでも不正なら 1 件も書かない");
    }

    #[test]
    fn apply_and_restore_writes_exact_wire_values_in_reverse_order() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort::new(catalog, snap.clone());
        let outcome = apply_operations(
            &mut port,
            &snap,
            &[
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "5.0".into(),
                },
                Change {
                    block: "block2".into(),
                    parameter: "Clipping 1".into(),
                    value: "Distortion".into(),
                },
            ],
            &[],
        )
        .unwrap();
        assert_eq!(outcome.error, None);
        assert_eq!(
            port.writes,
            vec![(2, 0, WireValue::F32(0.5)), (2, 1, WireValue::Int(2)),]
        );
        port.writes.clear();
        let failures = restore_undo(&mut port, outcome.undo.as_ref().unwrap());
        assert!(failures.is_empty());
        assert_eq!(
            port.writes,
            vec![(2, 1, WireValue::Int(1)), (2, 0, WireValue::F32(0.22)),]
        );
    }

    #[test]
    fn apply_partial_failure_keeps_already_written_changes() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort::new(catalog, snap.clone());
        port.fail_on_write = Some(1);
        let outcome = apply_operations(
            &mut port,
            &snap,
            &[
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "5.0".into(),
                },
                Change {
                    block: "block2".into(),
                    parameter: "Clipping 1".into(),
                    value: "Distortion".into(),
                },
            ],
            &[],
        )
        .unwrap();
        assert_eq!(outcome.steps.len(), 1);
        assert!(matches!(
            &outcome.steps[0],
            AppliedStep::Param(a) if a.param_name == "Gain 1"
        ));
        assert!(outcome.error.unwrap().contains("1 件実行"));
    }

    #[test]
    fn restore_continues_and_reports_failed_item() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort::new(catalog, snap.clone());
        let undo = apply_operations(
            &mut port,
            &snap,
            &[
                Change {
                    block: "block2".into(),
                    parameter: "Gain 1".into(),
                    value: "5.0".into(),
                },
                Change {
                    block: "block2".into(),
                    parameter: "Clipping 1".into(),
                    value: "Distortion".into(),
                },
            ],
            &[],
        )
        .unwrap()
        .undo
        .unwrap();
        port.writes.clear();
        port.write_calls = 0;
        port.fail_on_write = Some(0);
        let failures = restore_undo(&mut port, &undo);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].parameter, "Clipping 1");
        assert_eq!(port.writes, vec![(2, 0, WireValue::F32(0.22))]);
    }

    #[test]
    fn multiple_adds_track_after_position_across_insertions() {
        let (catalog, _tmp) = test_catalog();
        let snap = snapshot_from_preset(
            &test_preset(false, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "Multi Add".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());

        let outcome = apply_operations(
            &mut port,
            &snap,
            &[],
            &[
                BlockOp::Add {
                    model: "Brit J45 Brt".into(),
                    position: BlockPosition::Before("block1".into()),
                    stereo: None,
                    amp_cab: false,
                },
                BlockOp::Add {
                    model: "Dynamic Hall".into(),
                    position: BlockPosition::After("block1".into()),
                    stereo: None,
                    amp_cab: false,
                },
            ],
        )
        .unwrap();

        assert_eq!(outcome.error, None);
        // slot1 前に Brit J45 Brt (slot1)、ずれた元 block1 (= Tone Sovereign @ slot2) の直後 slot3 に Dynamic Hall
        assert_eq!(port.snapshot.blocks.len(), 3);
        assert_eq!(port.snapshot.blocks[0].model_number, 3);
        assert_eq!(port.snapshot.blocks[1].model_number, 0);
        assert_eq!(port.snapshot.blocks[2].model_number, 1);
    }

    fn test_preset(with_footswitch_on_slot1: bool, snapshot_diff_on_slot1: bool) -> Preset {
        use hx_proto::msgpack::Value as V;
        let block_slot = |model: i64| {
            hx_proto::msgmap! {
                19 => V::Int(6),
                20 => hx_proto::msgmap! {
                    9 => V::Int(0),
                    10 => V::Bool(true),
                    24 => hx_proto::msgmap! {
                        23 => V::Bool(false),
                        25 => V::Int(model),
                        26 => V::Int(-1),
                    },
                    11 => hx_proto::msgmap! {
                        2 => V::Int(3),
                        3 => V::Int(3),
                        4 => V::Array(vec![V::F32(0.22), V::F32(1.0), V::F32(0.0)]),
                    },
                },
            }
        };
        let empty = || hx_proto::msgmap! { 19 => V::Int(8), 20 => V::Nil };
        let input = hx_proto::msgmap! { 19 => V::Int(0), 20 => V::Nil };
        let output = hx_proto::msgmap! { 19 => V::Int(1), 20 => V::Nil };

        // slot 0: Input, slot 1: Tone Sovereign (0), slot 2: Empty, slot 3: Empty, slot 4: Output
        let assignments = if with_footswitch_on_slot1 {
            // Stomp ordinal 4 = Footswitch 1
            let entry = hx_proto::msgmap! {
                1 => hx_proto::msgmap! {
                    5 => V::Int(1),
                    9 => V::Int(1),
                    1 => V::Int(4),
                },
            };
            V::Array(vec![V::Nil, V::Nil, V::Nil, V::Nil, V::Array(vec![entry])])
        } else {
            V::Nil
        };

        let snapshot_section = if snapshot_diff_on_slot1 {
            let snap1 = hx_proto::msgmap! {
                0 => V::Bool(true),
                3 => V::Array(vec![
                    V::Array(vec![V::Nil, V::Bool(true)]),
                    V::Array(vec![V::Nil, V::Bool(true)]),
                    V::Array(vec![V::Nil, V::Bool(true)]),
                    V::Array(vec![V::Nil, V::Bool(false)]),
                    V::Array(vec![V::Nil, V::Bool(true)]),
                ]),
                4 => V::Str("SNAP 1".into()),
                5 => V::F32(0.0),
            };
            let snap2 = hx_proto::msgmap! {
                0 => V::Bool(true),
                3 => V::Array(vec![
                    V::Array(vec![V::Nil, V::Bool(true)]),
                    V::Array(vec![V::Nil, V::Bool(false)]),
                    V::Array(vec![V::Nil, V::Bool(true)]),
                    V::Array(vec![V::Nil, V::Bool(false)]),
                    V::Array(vec![V::Nil, V::Bool(true)]),
                ]),
                4 => V::Str("SNAP 2".into()),
                5 => V::F32(0.0),
            };
            hx_proto::msgmap! { 10 => V::Array(vec![snap1, snap2]) }
        } else {
            V::Nil
        };

        let tone = hx_proto::msgmap! {
            7 => hx_proto::msgmap! {
                0 => V::UInt(0x0380_0000),
                1 => V::Str("v3.80".into()),
            },
            0 => hx_proto::msgmap! {
                22 => V::Array(vec![input, block_slot(0), empty(), empty(), output]),
            },
            1 => V::Nil,
            3 => V::Nil,
            4 => assignments,
            2 => V::Nil,
            5 => V::Nil,
            6 => V::Nil,
            10 => snapshot_section,
        };
        let bytes = encode_test_preset(&tone);
        Preset::parse(&bytes).expect("test preset parses")
    }

    fn encode_test_preset(tone: &hx_proto::msgpack::Value) -> Vec<u8> {
        use hx_proto::msgpack::{Decoder, Encoder, Value};
        const SLOT_ORDER: [i64; 9] = [0, 1, 3, 4, 2, 5, 6, 7, 10];
        let magic = Encoder::encode(&Value::Str(Preset::MAGIC.to_owned()));
        let table_len = (1 + SLOT_ORDER.len() + 2) * 4;
        let table_hdr = Encoder::encode(&Value::Bin(vec![0; table_len], 1)).len() - table_len;
        let tone_at = magic.len() + table_hdr + table_len;
        let tone_bytes = Encoder::encode(tone);
        let mut key_at = std::collections::HashMap::new();
        let mut key_end = std::collections::HashMap::new();
        let count = (tone_bytes[0] & 0x0f) as usize;
        let mut at = 1usize;
        for _ in 0..count {
            let here = at;
            let mut d = Decoder::new(&tone_bytes[at..]);
            let key = d.value().unwrap().as_i64().unwrap();
            let after_key = tone_bytes.len() - d.remaining();
            let mut dv = Decoder::new(&tone_bytes[after_key..]);
            dv.value().unwrap();
            at = tone_bytes.len() - dv.remaining();
            key_at.insert(key, tone_at + here);
            key_end.insert(key, tone_at + at);
        }
        let snapshots_end = *key_end.get(&10).unwrap();
        let mut sections = Vec::with_capacity(table_len);
        sections.extend_from_slice(&(tone_at as u32).to_le_bytes());
        for key in SLOT_ORDER {
            sections.extend_from_slice(&(*key_at.get(&key).unwrap() as u32).to_le_bytes());
        }
        sections.extend_from_slice(&(snapshots_end as u32).to_le_bytes());
        sections.extend_from_slice(&(snapshots_end as u32).to_le_bytes());

        let mut blob = magic;
        blob.extend_from_slice(&Encoder::encode(&Value::Bin(sections, 1)));
        blob.extend_from_slice(&tone_bytes);
        blob
    }

    #[test]
    fn resolve_model_handles_mono_stereo_and_amp_cab() {
        let (catalog, _tmp) = test_catalog();
        // 指定なしなら Mono 優先
        let mono = resolve_model(&catalog, "Dynamic Hall", None, false).unwrap();
        assert_eq!(mono.model_number, 1);
        assert!(!mono.is_stereo);

        // Stereo 明示、および末尾 "(Stereo)" 表記
        let stereo = resolve_model(&catalog, "Dynamic Hall (Stereo)", None, false).unwrap();
        assert_eq!(stereo.model_number, 2);
        assert!(stereo.is_stereo);

        // Mono のみのモデルに Stereo 指定したらエラー、Stereo のみのモデルに Mono 指定したらエラー
        let err = resolve_model(&catalog, "Tone Sovereign", Some(true), false).unwrap_err();
        assert!(err.contains("Stereo 版がありません"), "{err}");
        let err_mono = resolve_model(&catalog, "Stereo Only", Some(false), false).unwrap_err();
        assert!(err_mono.contains("Mono 版がありません"), "{err_mono}");
        let stereo_default = resolve_model(&catalog, "Stereo Only", None, false).unwrap();
        assert_eq!(stereo_default.model_number, 5);

        // Amp 単体 vs Amp+Cab
        let amp_only = resolve_model(&catalog, "Brit J45 Brt", None, false).unwrap();
        assert_eq!(amp_only.model_number, 3);
        assert_eq!(amp_only.paired_cab, None);

        let amp_cab = resolve_model(&catalog, "Brit J45 Brt", None, true).unwrap();
        assert_eq!(amp_cab.model_number, 3);
        assert_eq!(amp_cab.paired_cab, Some(4));
        assert!(amp_cab.display_name.contains("Amp+Cab"));

        // Cab 定義のないモデルに amp_cab = true はエラー
        let err = resolve_model(&catalog, "Dynamic Hall", None, true).unwrap_err();
        assert!(err.contains("ペアの Cab 定義がありません"), "{err}");
    }

    #[test]
    fn snapshot_locks_blocks_assigned_to_footswitch_or_snapshots() {
        let (catalog, _tmp) = test_catalog();

        let p_fs = test_preset(true, false);
        let snap_fs = snapshot_from_preset(
            &p_fs,
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "FS Preset".into(),
        );
        assert!(snap_fs.blocks[0].locked);
        assert_eq!(snap_fs.free_block_slots, 2);
        assert!(render_chain(&snap_fs).contains("割り当て済み: 削除・差し替え不可"));
        assert!(render_chain(&snap_fs).contains("空きブロックスロット: 2 枠"));

        let p_snap = test_preset(false, true);
        let snap_sn = snapshot_from_preset(
            &p_snap,
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "Snap Preset".into(),
        );
        assert!(snap_sn.blocks[0].locked);

        // スナップショット間で常に ON / 常に OFF のブロックはロックされない、かつ ReadOnly 表示は [固定] を付けない
        let mut p_const = p_snap;
        if let Some(hx_proto::msgpack::Value::Array(slots)) = p_const.tone.at_mut(&[0, 22]) {
            let extra = slots[1].clone();
            slots[2] = extra.clone();
            slots[3] = extra;
        }
        let p_const = Preset::parse(&p_const.encode()).unwrap();
        let mut snap_const = snapshot_from_preset(
            &p_const,
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "Multi Snap".into(),
        );
        assert!(snap_const.blocks[0].locked, "true/false の slot 1 は固定");
        assert!(!snap_const.blocks[1].locked, "true/true の slot 2 は非固定");
        assert!(
            !snap_const.blocks[2].locked,
            "false/false の slot 3 は非固定"
        );
        // slot 3 はスナップショット配列で true/true、もし false/false でも非固定であることを確認
        snap_const.blocks[1].params.push(ParamView {
            index: 99,
            name: "Memo".into(),
            value: "abc".into(),
            native_value: 0.0,
            wire_kind: CatalogKind::Text,
            kind: ParamKindView::ReadOnly,
            locked: true,
            lock_reason: Some("文字入力".into()),
        });
        let rendered = render_chain(&snap_const);
        assert!(rendered.contains("Memo: abc (変更不可)\n"));
        assert!(!rendered.contains("Memo: abc (変更不可) [固定"));
    }

    #[test]
    fn operations_reject_removing_or_replacing_locked_block() {
        let (catalog, _tmp) = test_catalog();
        let snap = snapshot_from_preset(
            &test_preset(true, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "FS Preset".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());

        let err = apply_operations(
            &mut port,
            &snap,
            &[],
            &[BlockOp::Remove {
                block: "block1".into(),
            }],
        )
        .unwrap_err();
        assert!(err.contains("削除できません"), "{err}");
        assert!(port.events.is_empty());

        let err = apply_operations(
            &mut port,
            &snap,
            &[],
            &[BlockOp::Replace {
                block: "block1".into(),
                model: "Dynamic Hall".into(),
                stereo: None,
                amp_cab: false,
            }],
        )
        .unwrap_err();
        assert!(err.contains("モデル変更できません"), "{err}");
        assert!(port.events.is_empty());
    }

    #[test]
    fn set_enabled_applies_and_restores_as_lightweight_edits() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort::new(catalog, snap.clone());

        let outcome = apply_operations(
            &mut port,
            &snap,
            &[Change {
                block: "block2".into(),
                parameter: "Gain 1".into(),
                value: "5.0".into(),
            }],
            &[BlockOp::SetEnabled {
                block: "block2".into(),
                enabled: false,
            }],
        )
        .unwrap();

        assert!(!outcome.structure_changed);
        assert!(!outcome.added_or_replaced);
        assert_eq!(outcome.steps.len(), 2);
        assert_eq!(
            port.events,
            vec![
                FakeEvent::Param(2, 0, WireValue::F32(0.5)),
                FakeEvent::Enabled(2, false),
            ]
        );
        assert!(matches!(outcome.undo, Some(UndoEntry::Edits(_))));

        port.events.clear();
        let failures = restore_undo(&mut port, outcome.undo.as_ref().unwrap());
        assert!(failures.is_empty());
        assert_eq!(
            port.events,
            vec![
                FakeEvent::Enabled(2, true),
                FakeEvent::Param(2, 0, WireValue::F32(0.22)),
            ]
        );
    }

    #[test]
    fn add_before_slides_blocks_writes_preset_and_restores_snapshot_on_undo() {
        let (catalog, _tmp) = test_catalog();
        let preset = test_preset(false, false);
        let orig_bytes = preset.encode();
        let snap = snapshot_from_preset(
            &preset,
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "Add Test".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());

        // slot 1 に Tone Sovereign があり、その前に Dynamic Hall Stereo を挿す
        let outcome = apply_operations(
            &mut port,
            &snap,
            &[],
            &[BlockOp::Add {
                model: "Dynamic Hall".into(),
                position: BlockPosition::Before("block1".into()),
                stereo: Some(true),
                amp_cab: false,
            }],
        )
        .unwrap();

        assert!(outcome.structure_changed);
        assert!(outcome.added_or_replaced);
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.steps,
            vec![AppliedStep::Added {
                block_id: "block1".into(),
                model_name: "Dynamic Hall".into(),
            }]
        );
        // slot 1 を空けるために WritePreset が飛び、その後 slot 1 に model=2 (Dynamic Hall Stereo)
        assert_eq!(
            port.events,
            vec![FakeEvent::WritePreset, FakeEvent::Model(1, 2, None)]
        );
        assert_eq!(port.snapshot.blocks.len(), 2);
        assert_eq!(port.read_calls, 0);
        assert_eq!(port.snapshot.blocks[0].model_number, 2);
        assert_eq!(port.snapshot.blocks[1].model_number, 0);

        // Undo で元の編集バッファ丸ごと戻る
        let undo = outcome.undo.unwrap();
        assert!(matches!(undo, UndoEntry::PresetSnapshot { .. }));
        port.events.clear();
        let failures = restore_undo(&mut port, &undo);
        assert!(failures.is_empty());
        assert_eq!(port.events, vec![FakeEvent::WritePreset]);
        assert_eq!(
            port.snapshot.raw_preset.as_deref(),
            Some(orig_bytes.as_slice())
        );
    }

    #[test]
    fn add_at_end_places_without_sliding_when_free_slot_follows() {
        let (catalog, _tmp) = test_catalog();
        let snap = snapshot_from_preset(
            &test_preset(false, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "End Test".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());

        let outcome = apply_operations(
            &mut port,
            &snap,
            &[],
            &[BlockOp::Add {
                model: "Brit J45 Brt".into(),
                position: BlockPosition::End,
                stereo: None,
                amp_cab: true,
            }],
        )
        .unwrap();

        // slot 2 が空いているので WritePreset は飛ばず set_model(2, 3, Some(4)) のみ
        assert_eq!(port.events, vec![FakeEvent::Model(2, 3, Some(4))]);
        assert_eq!(port.snapshot.blocks[1].category, "Amp+Cab");
        assert_eq!(
            outcome.steps,
            vec![AppliedStep::Added {
                block_id: "block2".into(),
                model_name: "Brit J45 Brt (Amp+Cab)".into(),
            }]
        );
    }

    #[test]
    fn add_rolls_back_slide_if_device_refuses_model_e_g_dsp_limit() {
        let (catalog, _tmp) = test_catalog();
        let snap = snapshot_from_preset(
            &test_preset(false, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "DSP Fail".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());
        port.fail_on_set_model = Some("DSP 不足".into());

        let outcome = apply_operations(
            &mut port,
            &snap,
            &[],
            &[BlockOp::Add {
                model: "Dynamic Hall".into(),
                position: BlockPosition::Before("block1".into()),
                stereo: None,
                amp_cab: false,
            }],
        )
        .unwrap();

        assert!(outcome.error.unwrap().contains("DSP 不足"));
        assert!(outcome.steps.is_empty());
        // スライド WritePreset のあとにロールバック WritePreset が呼ばれている
        assert_eq!(
            port.events,
            vec![FakeEvent::WritePreset, FakeEvent::WritePreset]
        );
    }

    #[test]
    fn remove_and_replace_execute_and_reject_contradictions() {
        let (catalog, _tmp) = test_catalog();
        let snap = snapshot_from_preset(
            &test_preset(false, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "Remove Replace".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());

        // 差し替えと同時の旧パラメータ変更は事前検証で弾く
        let err = apply_operations(
            &mut port,
            &snap,
            &[Change {
                block: "block1".into(),
                parameter: "Gain 1".into(),
                value: "5.0".into(),
            }],
            &[BlockOp::Replace {
                block: "block1".into(),
                model: "Dynamic Hall".into(),
                stereo: None,
                amp_cab: false,
            }],
        )
        .unwrap_err();
        assert!(err.contains("差し替え後"), "{err}");
        assert!(port.events.is_empty());

        // Replace 単独なら実行され PresetSnapshot で Undo できる
        let outcome = apply_operations(
            &mut port,
            &snap,
            &[],
            &[BlockOp::Replace {
                block: "block1".into(),
                model: "Dynamic Hall".into(),
                stereo: None,
                amp_cab: false,
            }],
        )
        .unwrap();
        assert!(outcome.structure_changed);
        assert!(outcome.added_or_replaced);
        assert_eq!(port.events, vec![FakeEvent::Model(1, 1, None)]);

        // Clear (Remove)
        let snap2 = port.snapshot.clone();
        port.events.clear();
        let outcome2 = apply_operations(
            &mut port,
            &snap2,
            &[],
            &[BlockOp::Remove {
                block: "block1".into(),
            }],
        )
        .unwrap();
        assert!(outcome2.structure_changed);
        assert!(!outcome2.added_or_replaced);
        assert_eq!(port.events, vec![FakeEvent::Clear(1)]);
        assert_eq!(port.snapshot.blocks.len(), 0);
    }

    #[test]
    fn multi_add_tracks_shifted_slots_and_plan_checks_limits() {
        let (catalog, _tmp) = test_catalog();
        let mut snap_no_raw = sample_snapshot(&catalog, false);
        snap_no_raw.free_block_slots = 1;
        // raw_preset なしでも SetEnabled は通り、Add は弾く
        let (_, ops_ok) = plan_operations(
            &snap_no_raw,
            &[],
            &[BlockOp::SetEnabled {
                block: "block2".into(),
                enabled: false,
            }],
            &catalog,
        )
        .unwrap();
        assert_eq!(ops_ok.len(), 1);
        let err_raw = plan_operations(
            &snap_no_raw,
            &[],
            &[BlockOp::Add {
                model: "Dynamic Hall".into(),
                position: BlockPosition::End,
                stereo: None,
                amp_cab: false,
            }],
            &catalog,
        )
        .unwrap_err();
        assert!(err_raw.contains("構造情報"), "{err_raw}");

        // 上限ちょうど (2 + 10 = 12) は許可し、13 は拒否する (+ と * や > と >= を検証)
        let mut snap_with_raw = snap_no_raw.clone();
        snap_with_raw.raw_preset = Some(test_preset(false, false).encode());
        snap_with_raw.free_block_slots = 10;
        let changes_2 = vec![
            Change {
                block: "block2".into(),
                parameter: "Gain 1".into(),
                value: "3.0".into(),
            },
            Change {
                block: "block2".into(),
                parameter: "Clipping 1".into(),
                value: "Boost".into(),
            },
        ];
        let one_add = BlockOp::Add {
            model: "Dynamic Hall".into(),
            position: BlockPosition::End,
            stereo: None,
            amp_cab: false,
        };
        let ops_10 = vec![one_add.clone(); 10];
        assert!(plan_operations(&snap_with_raw, &changes_2, &ops_10, &catalog).is_ok());

        let mut ops_11 = ops_10;
        ops_11.push(one_add.clone());
        let err_13 = plan_operations(&snap_with_raw, &changes_2, &ops_11, &catalog).unwrap_err();
        assert!(err_13.contains("多すぎます"), "{err_13}");

        // 空き 1 枠で 2 個追加は拒否、1 個 Remove と併せて 2 個 Add なら許可
        snap_with_raw.free_block_slots = 1;
        let ops_2_adds = vec![one_add.clone(), one_add.clone()];
        let err_slots = plan_operations(&snap_with_raw, &[], &ops_2_adds, &catalog).unwrap_err();
        assert!(
            err_slots.contains("空きブロックスロットが足りません"),
            "{err_slots}"
        );

        let ops_rm_and_2_adds = vec![
            BlockOp::Remove {
                block: "block2".into(),
            },
            one_add.clone(),
            one_add,
        ];
        assert!(plan_operations(&snap_with_raw, &[], &ops_rm_and_2_adds, &catalog).is_ok());

        // Before("block1") + After("block1") の 2 回連続 Add で、スライド後の slot 2 を基準に slot 3 へ配置される
        let snap = snapshot_from_preset(
            &test_preset(false, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "37B".into(),
            "Multi Add".into(),
        );
        let mut port = FakePort::new(catalog, snap.clone());
        let outcome = apply_operations(
            &mut port,
            &snap,
            &[],
            &[
                BlockOp::Add {
                    model: "Dynamic Hall".into(),
                    position: BlockPosition::Before("block1".into()),
                    stereo: Some(false),
                    amp_cab: false,
                },
                BlockOp::Add {
                    model: "Brit J45 Brt".into(),
                    position: BlockPosition::After("block1".into()),
                    stereo: None,
                    amp_cab: false,
                },
            ],
        )
        .unwrap();
        assert_eq!(outcome.error, None);
        assert_eq!(
            outcome.steps,
            vec![
                AppliedStep::Added {
                    block_id: "block1".into(),
                    model_name: "Dynamic Hall".into(),
                },
                AppliedStep::Added {
                    block_id: "block3".into(),
                    model_name: "Brit J45 Brt".into(),
                },
            ]
        );
        assert_eq!(port.read_calls, 1);
        assert_eq!(port.snapshot.blocks.len(), 3);
        assert_eq!(port.snapshot.blocks[0].id, "block1");
        assert_eq!(port.snapshot.blocks[1].id, "block2");
        assert_eq!(port.snapshot.blocks[1].name, "Tone Sovereign");
        assert_eq!(port.snapshot.blocks[2].id, "block3");
        assert_eq!(port.snapshot.blocks[2].name, "Brit J45 Brt");
        assert_eq!(port.snapshot.blocks[2].category, "Amp");

        // block2 (slot != 1) を基準にした Remove + Before("block2") でも正しくスロットを解決できる
        let snap2 = port.snapshot.clone();
        let outcome2 = apply_operations(
            &mut port,
            &snap2,
            &[],
            &[
                BlockOp::Remove {
                    block: "block3".into(),
                },
                BlockOp::Add {
                    model: "Dynamic Hall".into(),
                    position: BlockPosition::Before("block2".into()),
                    stereo: Some(true),
                    amp_cab: false,
                },
            ],
        )
        .unwrap();
        assert_eq!(outcome2.error, None);
        assert_eq!(port.snapshot.blocks[1].id, "block2");
        assert_eq!(port.snapshot.blocks[1].model_number, 2);
    }

    #[test]
    fn snapshot_and_chain_view_carry_category_and_color() {
        let (catalog, _tmp) = test_catalog();
        let snap = snapshot_from_preset(
            &test_preset(false, false),
            &catalog,
            rpc::ControlLayout::Stomp,
            "12A".into(),
            "Clean View".into(),
        );
        let view = to_chain_view(&snap);
        assert_eq!(view.slot_label, "12A");
        assert_eq!(view.preset_name, "Clean View");
        assert_eq!(view.free_block_slots, 2);
        assert_eq!(view.blocks.len(), 1);
        assert_eq!(view.blocks[0].category, "Dist");
        assert_eq!(view.blocks[0].color_hex, "#f5901e");
        assert_eq!(view.blocks[0].params[0].kind, "number");
        assert_eq!(view.blocks[0].params[0].min.as_deref(), Some("0.0"));
        assert_eq!(view.blocks[0].params[0].max.as_deref(), Some("10.0"));
        assert_eq!(view.blocks[0].params[1].kind, "choice");
    }

    fn verify_usb_round_trip(idle_wait: Duration) {
        assert_eq!(
            std::env::var("HX_USB_LIVE").as_deref(),
            Ok("1"),
            "実機の編集バッファを書き換えるため、HX_USB_LIVE=1 を付けたときだけ走らせる"
        );
        let port = Arc::new(Mutex::new(Some(UsbHxPort::new().unwrap())));
        let before = port
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .read_snapshot()
            .unwrap();
        println!("接続成功:\n{}", render_chain(&before));

        if !idle_wait.is_zero() {
            let _guard = spawn_keepalive(port.clone(), KEEPALIVE_INTERVAL);
            std::thread::sleep(idle_wait);
        }

        let (block_id, param_name, next_val, old_val) = before
            .blocks
            .iter()
            .find_map(|b| {
                b.params.iter().find_map(|p| {
                    if p.locked {
                        return None;
                    }
                    if let ParamKindView::Number { min, max } = &p.kind {
                        let cur: f64 = p.value.split_whitespace().next()?.parse().ok()?;
                        let min_n: f64 = min.split_whitespace().next()?.parse().ok()?;
                        let max_n: f64 = max.split_whitespace().next()?.parse().ok()?;
                        let candidate = if cur + 1.0 <= max_n {
                            cur + 1.0
                        } else if cur - 1.0 >= min_n {
                            cur - 1.0
                        } else {
                            return None;
                        };
                        return Some((
                            b.id.clone(),
                            p.name.clone(),
                            format!("{candidate}"),
                            p.value.clone(),
                        ));
                    }
                    None
                })
            })
            .expect("動かせる数値パラメータが現在のプリセットにありません");

        let undo = apply_operations(
            port.lock().unwrap().as_mut().unwrap(),
            &before,
            &[Change {
                block: block_id.clone(),
                parameter: param_name.clone(),
                value: next_val.clone(),
            }],
            &[],
        )
        .unwrap()
        .undo
        .unwrap();
        let after = port
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .read_snapshot()
            .unwrap();
        let changed = after
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .unwrap()
            .params
            .iter()
            .find(|p| p.name == param_name)
            .unwrap();
        assert_ne!(changed.value, old_val, "変更が実機に反映されていません");
        println!(
            "変更確認: {block_id} / {param_name}: {old_val} -> {}",
            changed.value
        );

        let failures = restore_undo(port.lock().unwrap().as_mut().unwrap(), &undo);
        assert!(failures.is_empty(), "復元に失敗: {failures:?}");
        let restored = port
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .read_snapshot()
            .unwrap();
        let back = restored
            .blocks
            .iter()
            .find(|b| b.id == block_id)
            .unwrap()
            .params
            .iter()
            .find(|p| p.name == param_name)
            .unwrap();
        assert_eq!(back.value, old_val, "元の値に戻っていません");
        println!("復元確認: {block_id} / {param_name} = {}", back.value);
    }

    /// 実機の HX Stomp / HX Stomp XL で、読み込み → 1 パラメータ変更 → 読み返し → 復元 → 読み返しを確かめる。
    #[test]
    #[ignore]
    fn hx_usb_round_trip() {
        verify_usb_round_trip(Duration::ZERO);
    }

    struct FakeKeepalivePort {
        ticks: Arc<AtomicUsize>,
        fail: bool,
        dropped_session: bool,
    }

    impl KeepalivePort for FakeKeepalivePort {
        fn keepalive(&mut self) -> Result<(), String> {
            self.ticks.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                self.dropped_session = true;
                Err("keepalive failed".into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn keepalive_step_ticks_open_port_and_ignores_none() {
        let none_port: Mutex<Option<FakeKeepalivePort>> = Mutex::new(None);
        keepalive_step(&none_port);

        let ticks = Arc::new(AtomicUsize::new(0));
        let port = Mutex::new(Some(FakeKeepalivePort {
            ticks: ticks.clone(),
            fail: false,
            dropped_session: false,
        }));
        keepalive_step(&port);
        keepalive_step(&port);
        assert_eq!(ticks.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn keepalive_step_skips_while_port_mutex_is_locked() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let port = Mutex::new(Some(FakeKeepalivePort {
            ticks: ticks.clone(),
            fail: false,
            dropped_session: false,
        }));
        let _busy = port.lock().unwrap();
        keepalive_step(&port);
        assert_eq!(ticks.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn keepalive_step_continues_after_error() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let port = Mutex::new(Some(FakeKeepalivePort {
            ticks: ticks.clone(),
            fail: true,
            dropped_session: false,
        }));
        keepalive_step(&port);
        assert_eq!(ticks.load(Ordering::SeqCst), 1);
        assert!(port.lock().unwrap().as_ref().unwrap().dropped_session);
    }

    #[test]
    fn spawn_keepalive_runs_periodically_and_stops_on_drop() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let port = Arc::new(Mutex::new(Some(FakeKeepalivePort {
            ticks: ticks.clone(),
            fail: false,
            dropped_session: false,
        })));
        let guard = spawn_keepalive(port, Duration::from_millis(10));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while ticks.load(Ordering::SeqCst) < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "keepalive が周期実行されなかった"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let stopped_at = std::time::Instant::now();
        drop(guard);
        assert!(
            stopped_at.elapsed() < Duration::from_millis(200),
            "Drop で即座に止まらなかった"
        );
        let frozen = ticks.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(ticks.load(Ordering::SeqCst), frozen);
    }

    /// AI 問い合わせ待ちを模擬し、読み込みから 12 秒アイドルを挟んだあと書き込みと復元が通ることを確かめる。
    #[test]
    #[ignore]
    fn hx_usb_idle_keepalive_round_trip() {
        verify_usb_round_trip(Duration::from_secs(12));
    }
}
