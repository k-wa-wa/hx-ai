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
use hx_proto::{DeviceProfile, Preset, Value as WireValue, rpc};
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

/// AI やワークフローから渡す 1 件の変更指示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// ブロック識別子 (例: `"block2"`)。
    pub block: String,
    /// パラメータの表示名 (例: `"Gain 1"`)。
    pub parameter: String,
    /// 画面表示相当の新しい値 (例: `"2.5"`, `"Boost"`)。
    pub value: String,
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
    pub enabled: bool,
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
    pub blocks: Vec<BlockView>,
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
    for a in preset.assignments(layout) {
        if let hx_proto::preset::Target::Param(idx) = a.target {
            locked_params
                .entry((a.block, idx))
                .or_insert_with(|| format!("コントローラー ({:?}) に割り当て済み", a.source));
        }
    }

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

            BlockView {
                id: format!("block{slot}"),
                slot,
                model_number,
                name: model_name,
                enabled: block.enabled,
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

    if snap.blocks.is_empty() {
        out.push_str("(ブロックがありません)\n");
        return out;
    }

    for b in &snap.blocks {
        let state = if b.enabled { "ON" } else { "BYPASSED (OFF)" };
        out.push_str(&format!("\n[{}] {} [{}]\n", b.id, b.name, state));
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

/// `apply_changes` の結果。部分的に適用されたあと失敗した場合も、適用済み分を返す。
#[derive(Debug, Clone, PartialEq)]
pub struct ApplyOutcome {
    pub applied: Vec<AppliedChange>,
    pub error: Option<String>,
}

/// 検証してから実機へ変更を送り、Undo 用の適用記録を返す。
/// 検証エラー (1 件も送っていない) は `Err`、送信途中の失敗は `Ok(ApplyOutcome { applied, error: Some(...) })`。
pub fn apply_changes(
    port: &mut dyn HxPort,
    snap: &ChainSnapshot,
    changes: &[Change],
) -> Result<ApplyOutcome, String> {
    let planned = plan_changes(snap, changes, port.catalog())?;
    let mut applied = Vec::with_capacity(planned.len());
    for w in planned {
        let wire = w.wire_kind.wire(w.new_native);
        if let Err(e) = port.write_param(w.slot, w.param_index, wire) {
            let msg = if applied.is_empty() {
                e
            } else {
                format!("{} 件適用したところで失敗しました: {e}", applied.len())
            };
            return Ok(ApplyOutcome {
                applied,
                error: Some(msg),
            });
        }
        applied.push(AppliedChange::from(w));
    }
    Ok(ApplyOutcome {
        applied,
        error: None,
    })
}

/// 適用済みの変更を逆順で元の生値に戻す。失敗した項目だけ返す。
pub fn restore_changes(port: &mut dyn HxPort, applied: &[AppliedChange]) -> Vec<RestoreFailure> {
    let mut failures = Vec::new();
    for a in applied.iter().rev() {
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
    failures
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
}

fn format_usb_error(err: hx_usb::Error) -> String {
    match err {
        hx_usb::Error::NotFound => "HX Stomp / HX Stomp XL が USB で見つかりません".into(),
        hx_usb::Error::Claim(detail) => format!(
            "HX Stomp の USB 制御をつかめません (HX Edit が開いていれば閉じてください): {detail}"
        ),
        other => format!("HX Stomp との USB 通信に失敗しました: {other}"),
    }
}

impl HxPort for UsbHxPort {
    fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    fn read_snapshot(&mut self) -> Result<ChainSnapshot, String> {
        let _ = self.ensure_session()?;
        let res = {
            let s = self.session.as_mut().unwrap();
            let profile: DeviceProfile = s.profile;
            let info = s.preset_info();
            let preset = s.read_preset();
            (profile, info, preset)
        };

        let (profile, info_res, preset_res) = res;
        let preset = match preset_res {
            Ok(p) => p,
            Err(e) => {
                if e.loses_session() {
                    self.session = None;
                }
                return Err(format_usb_error(e));
            }
        };

        let (slot_label, preset_name) = match info_res {
            Ok((_, index, name)) => (profile.slot_label(index), name),
            Err(e) => {
                if e.loses_session() {
                    self.session = None;
                }
                return Err(format_usb_error(e));
            }
        };

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
        let _ = self.ensure_session()?;
        let res = self
            .session
            .as_mut()
            .unwrap()
            .set_param(slot as i64, param_index as i64, value);
        if let Err(e) = res {
            if e.loses_session() {
                self.session = None;
            }
            return Err(format_usb_error(e));
        }
        Ok(())
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
                }
            ]
        }"#;
        std::fs::write(dir.join("HX_ModelCatalog.json"), browse).unwrap();

        let sym = r#"[
            {
                "symbol": "Test_ToneSovereign",
                "parameters": ["Gain1", "Clipping1", "Bright"]
            }
        ]"#;
        std::fs::write(dir.join("Helix.sym"), sym).unwrap();

        let catalog = Catalog::load_from(&dir).unwrap();
        (catalog, TempDir(dir))
    }

    struct FakePort {
        catalog: Catalog,
        snapshot: ChainSnapshot,
        writes: Vec<(usize, usize, WireValue)>,
        write_calls: usize,
        fail_on_write: Option<usize>,
    }

    impl HxPort for FakePort {
        fn read_snapshot(&mut self) -> Result<ChainSnapshot, String> {
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
            self.writes.push((slot, param_index, value));
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
            blocks: vec![BlockView {
                id: "block2".into(),
                slot: 2,
                model_number: 0,
                name: "Tone Sovereign".into(),
                enabled: true,
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
        let mut port = FakePort {
            catalog,
            snapshot: snap.clone(),
            writes: vec![],
            write_calls: 0,
            fail_on_write: None,
        };
        let err = apply_changes(
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
        )
        .unwrap_err();
        assert!(!err.is_empty());
        assert!(port.writes.is_empty(), "1 つでも不正なら 1 件も書かない");
    }

    #[test]
    fn apply_and_restore_writes_exact_wire_values_in_reverse_order() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort {
            catalog,
            snapshot: snap.clone(),
            writes: vec![],
            write_calls: 0,
            fail_on_write: None,
        };
        let outcome = apply_changes(
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
        )
        .unwrap();
        assert_eq!(outcome.error, None);
        let applied = outcome.applied;
        assert_eq!(
            port.writes,
            vec![(2, 0, WireValue::F32(0.5)), (2, 1, WireValue::Int(2)),]
        );
        port.writes.clear();
        let failures = restore_changes(&mut port, &applied);
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
        let mut port = FakePort {
            catalog,
            snapshot: snap.clone(),
            writes: vec![],
            write_calls: 0,
            fail_on_write: Some(1),
        };
        let outcome = apply_changes(
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
        )
        .unwrap();
        assert_eq!(outcome.applied.len(), 1);
        assert_eq!(outcome.applied[0].param_name, "Gain 1");
        assert!(outcome.error.unwrap().contains("1 件適用"));
    }

    #[test]
    fn restore_continues_and_reports_failed_item() {
        let (catalog, _tmp) = test_catalog();
        let snap = sample_snapshot(&catalog, false);
        let mut port = FakePort {
            catalog,
            snapshot: snap.clone(),
            writes: vec![],
            write_calls: 0,
            fail_on_write: None,
        };
        let applied = apply_changes(
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
        )
        .unwrap()
        .applied;
        port.writes.clear();
        port.write_calls = 0;
        port.fail_on_write = Some(0);
        let failures = restore_changes(&mut port, &applied);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].parameter, "Clipping 1");
        assert_eq!(port.writes, vec![(2, 0, WireValue::F32(0.22))]);
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

        let applied = apply_changes(
            port.lock().unwrap().as_mut().unwrap(),
            &before,
            &[Change {
                block: block_id.clone(),
                parameter: param_name.clone(),
                value: next_val.clone(),
            }],
        )
        .unwrap()
        .applied;
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

        let failures = restore_changes(port.lock().unwrap().as_mut().unwrap(), &applied);
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
