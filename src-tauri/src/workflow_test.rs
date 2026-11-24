//! サイクル・Undo・手動送信・自動ループの流れのテスト。
//!
//! AI の応答と録音・USB 操作の結果は台本で与え、`workflow` の状態遷移を確かめる。

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use base64::Engine as _;
use serde_json::Value;

use crate::ai::{AiBlockOp, AiChange, AiResponse, BlockOpAction, RestoreTarget};
use crate::config::StoredSettings;
use crate::hx_device::{
    AppliedChange, AppliedEdit, AppliedStep, BlockOp, BlockView, ChainSnapshot, Change,
    OperationsOutcome, ParamKindView, ParamView, RestoreFailure, UndoEntry,
};
use crate::sound::Prepared;
use crate::workflow::{
    self, AdjustInput, AppState, Engine, LoopIteration, MAX_CONSECUTIVE_FAILURES, MAX_UNDO_GROUPS,
    STOPPED_BEFORE_EXECUTE,
};

struct FakeEngine {
    chains: VecDeque<Result<ChainSnapshot, String>>,
    answers: VecDeque<Result<AiResponse, String>>,
    apply_results: VecDeque<Result<OperationsOutcome, String>>,
    restore_failures: VecDeque<Vec<RestoreFailure>>,
    /// AI に送ったリクエスト本文。
    requests: Vec<Value>,
    /// apply に渡った変更とブロック操作。
    applied_calls: Vec<(Vec<Change>, Vec<BlockOp>)>,
    /// restore に渡った Undo エントリ。
    restored_calls: Vec<UndoEntry>,
    /// 自動ループの録音の台本。尽きたら `recording()` を返す。
    recordings: VecDeque<Result<Prepared, String>>,
    records: usize,
    on_record: Option<Box<dyn FnMut(usize)>>,
    on_ask: Option<Box<dyn FnMut(usize)>>,
    iterations: Vec<LoopIteration>,
    settles: usize,
}

fn sample_chain() -> ChainSnapshot {
    ChainSnapshot {
        slot_label: "28B".into(),
        preset_name: "Test Preset".into(),
        tempo: Some(120.0),
        snapshots: vec!["SNAPSHOT 1".into()],
        free_block_slots: 4,
        raw_preset: None,
        blocks: vec![BlockView {
            id: "block5".into(),
            slot: 5,
            model_number: 1,
            name: "Brit J45 Brt".into(),
            enabled: true,
            locked: false,
            lock_reason: None,
            params: vec![
                ParamView {
                    index: 0,
                    name: "Bass".into(),
                    value: "5.0".into(),
                    native_value: 0.5,
                    wire_kind: hx_catalog::Kind::Continuous,
                    kind: ParamKindView::Number {
                        min: "0.0".into(),
                        max: "10.0".into(),
                    },
                    locked: false,
                    lock_reason: None,
                },
                ParamView {
                    index: 1,
                    name: "Treble".into(),
                    value: "7.2".into(),
                    native_value: 0.72,
                    wire_kind: hx_catalog::Kind::Continuous,
                    kind: ParamKindView::Number {
                        min: "0.0".into(),
                        max: "10.0".into(),
                    },
                    locked: false,
                    lock_reason: None,
                },
            ],
            readonly_notes: vec![],
        }],
    }
}

fn recording() -> Prepared {
    Prepared {
        wav: b"WAV".to_vec(),
        peak_dbfs: -12.0,
        seconds: 3.0,
    }
}

impl FakeEngine {
    fn new(answers: Vec<Result<AiResponse, String>>) -> Self {
        Self {
            chains: VecDeque::new(),
            answers: answers.into(),
            apply_results: VecDeque::new(),
            restore_failures: VecDeque::new(),
            requests: vec![],
            applied_calls: vec![],
            restored_calls: vec![],
            recordings: VecDeque::new(),
            records: 0,
            on_record: None,
            on_ask: None,
            iterations: vec![],
            settles: 0,
        }
    }

    fn request_text(&self, n: usize) -> String {
        self.requests[n]["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

fn default_apply(snap: &ChainSnapshot, changes: &[Change], ops: &[BlockOp]) -> OperationsOutcome {
    let mut steps = Vec::new();
    let mut edits = Vec::new();
    let mut structure_changed = false;
    let mut added_or_replaced = false;

    for (i, c) in changes.iter().enumerate() {
        let a = AppliedChange {
            slot: 5,
            block_id: c.block.clone(),
            block_name: "Brit J45 Brt".into(),
            param_index: i,
            param_name: c.parameter.clone(),
            previous_display: match c.parameter.as_str() {
                "Bass" => "5.0".into(),
                "Treble" => "7.2".into(),
                _ => "0.0".into(),
            },
            previous_native: 0.5,
            new_display: c.value.clone(),
            new_native: 0.6,
            wire_kind: hx_catalog::Kind::Continuous,
        };
        steps.push(AppliedStep::Param(a.clone()));
        edits.push(AppliedEdit::Param(a));
    }

    for op in ops {
        match op {
            BlockOp::SetEnabled { block, enabled } => {
                steps.push(AppliedStep::Enabled {
                    block_id: block.clone(),
                    previous_enabled: !*enabled,
                    new_enabled: *enabled,
                });
                edits.push(AppliedEdit::Enabled {
                    slot: 5,
                    block_id: block.clone(),
                    block_name: "Brit J45 Brt".into(),
                    previous_enabled: !*enabled,
                    new_enabled: *enabled,
                });
            }
            BlockOp::Remove { block } => {
                structure_changed = true;
                steps.push(AppliedStep::Removed {
                    block_id: block.clone(),
                    previous_name: "Brit J45 Brt".into(),
                });
            }
            BlockOp::Replace { block, model, .. } => {
                structure_changed = true;
                added_or_replaced = true;
                steps.push(AppliedStep::Replaced {
                    block_id: block.clone(),
                    previous_name: "Brit J45 Brt".into(),
                    new_name: model.clone(),
                });
            }
            BlockOp::Add { model, .. } => {
                structure_changed = true;
                added_or_replaced = true;
                steps.push(AppliedStep::Added {
                    block_id: "block6".into(),
                    model_name: model.clone(),
                });
            }
        }
    }

    let undo = if steps.is_empty() {
        None
    } else if structure_changed {
        Some(UndoEntry::PresetSnapshot {
            before_bytes: snap.raw_preset.clone().unwrap_or_else(|| vec![1, 2, 3]),
            summary: format!("構造変更 ({} 件)", steps.len()),
        })
    } else {
        Some(UndoEntry::Edits(edits))
    };

    OperationsOutcome {
        steps,
        undo,
        structure_changed,
        added_or_replaced,
        error: None,
    }
}

impl Engine for FakeEngine {
    fn progress(&self, _stage: &str, _message: String) {}

    async fn read_chain(&mut self) -> Result<ChainSnapshot, String> {
        self.chains
            .pop_front()
            .unwrap_or_else(|| Ok(sample_chain()))
    }

    async fn ask(&mut self, body: &Value) -> Result<AiResponse, String> {
        self.requests.push(body.clone());
        let n = self.requests.len();
        if let Some(f) = self.on_ask.as_mut() {
            f(n);
        }
        self.answers
            .pop_front()
            .expect("台本に無い AI への問い合わせ")
    }

    async fn apply(
        &mut self,
        snap: &ChainSnapshot,
        changes: &[Change],
        ops: &[BlockOp],
    ) -> Result<OperationsOutcome, String> {
        self.applied_calls.push((changes.to_vec(), ops.to_vec()));
        self.apply_results
            .pop_front()
            .unwrap_or_else(|| Ok(default_apply(snap, changes, ops)))
    }

    async fn restore(&mut self, entry: &UndoEntry) -> Vec<RestoreFailure> {
        self.restored_calls.push(entry.clone());
        self.restore_failures.pop_front().unwrap_or_default()
    }

    async fn record(&mut self) -> Result<Prepared, String> {
        self.records += 1;
        if let Some(f) = self.on_record.as_mut() {
            f(self.records);
        }
        self.recordings
            .pop_front()
            .unwrap_or_else(|| Ok(recording()))
    }

    fn iteration(&mut self, report: LoopIteration) {
        self.iterations.push(report);
    }

    async fn settle(&mut self) {
        self.settles += 1;
    }
}

fn state() -> AppState {
    AppState::new(
        std::env::temp_dir().join("unused.json"),
        StoredSettings::default(),
    )
}

fn change(param: &str, value: &str) -> AiChange {
    AiChange {
        block: "block5".into(),
        parameter: param.into(),
        value: value.into(),
    }
}

fn answer(changes: Vec<AiChange>) -> Result<AiResponse, String> {
    Ok(AiResponse {
        explanation: "説明".into(),
        changes,
        block_operations: vec![],
        goal_reached: false,
    })
}

fn reached() -> Result<AiResponse, String> {
    Ok(AiResponse {
        explanation: "目標に届いた".into(),
        changes: vec![],
        block_operations: vec![],
        goal_reached: true,
    })
}

fn applied(param: &str, prev_disp: &str, new_disp: &str) -> AppliedChange {
    AppliedChange {
        slot: 5,
        block_id: "block5".into(),
        block_name: "Brit J45 Brt".into(),
        param_index: if param == "Bass" { 0 } else { 1 },
        param_name: param.into(),
        previous_display: prev_disp.into(),
        previous_native: 0.5,
        new_display: new_disp.into(),
        new_native: 0.6,
        wire_kind: hx_catalog::Kind::Continuous,
    }
}

async fn adjust_with(
    engine: &mut FakeEngine,
    state: &AppState,
    instruction: &str,
) -> workflow::CycleReport {
    workflow::adjust(
        engine,
        state,
        "m",
        &AdjustInput {
            instruction,
            recording: Some(&recording()),
            history_label: None,
            stop: None,
        },
    )
    .await
    .unwrap()
}

async fn adjust(engine: &mut FakeEngine, state: &AppState) -> workflow::CycleReport {
    adjust_with(engine, state, "太く").await
}

fn undo_stack(state: &AppState) -> Vec<UndoEntry> {
    state.undo_stack.lock().unwrap().clone()
}

fn undo_edits(entry: &UndoEntry) -> Vec<AppliedChange> {
    match entry {
        UndoEntry::Edits(edits) => edits
            .iter()
            .filter_map(|e| match e {
                AppliedEdit::Param(a) => Some(a.clone()),
                _ => None,
            })
            .collect(),
        _ => panic!("expected Edits"),
    }
}

#[tokio::test]
async fn adjust_executes_changes_and_records_them_for_undo() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![
        change("Bass", "6.0"),
        change("Treble", "6.5"),
    ])]);
    let r = adjust(&mut e, &s).await;

    assert_eq!(r.error, None);
    assert_eq!(r.changes.len(), 2);
    assert_eq!(r.undo_depth, 1);
    assert_eq!(undo_stack(&s).len(), 1);
    let u = undo_edits(&undo_stack(&s)[0]);
    assert_eq!(u[0].previous_display, "5.0");
    assert_eq!(u[1].previous_display, "7.2");
    assert!(e.request_text(0).contains("[block5] Brit J45 Brt"));
    let chat = s.chat.lock().unwrap().clone();
    assert_eq!(chat.len(), 2);
    assert_eq!(chat[0].text, "太く");
    assert!(chat[1].text.contains("Treble: 7.2 → 6.5"));
}

#[tokio::test]
async fn invalid_ai_response_writes_nothing() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![change("Bass", "")])]);
    let r = adjust(&mut e, &s).await;

    assert!(r.error.unwrap().contains("空の項目"));
    assert!(e.applied_calls.is_empty());
    assert!(r.changes.is_empty());
    assert!(undo_stack(&s).is_empty());
}

#[tokio::test]
async fn partial_apply_failure_keeps_executed_changes_undoable() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![
        change("Bass", "6.0"),
        change("Treble", "6.5"),
    ])]);
    let a = applied("Bass", "5.0", "6.0");
    e.apply_results.push_back(Ok(OperationsOutcome {
        steps: vec![AppliedStep::Param(a.clone())],
        undo: Some(UndoEntry::Edits(vec![AppliedEdit::Param(a)])),
        structure_changed: false,
        added_or_replaced: false,
        error: Some("USB 通信エラー".into()),
    }));
    let r = adjust(&mut e, &s).await;

    assert_eq!(r.error.as_deref(), Some("USB 通信エラー"));
    assert_eq!(r.changes.len(), 1);
    assert_eq!(undo_stack(&s).len(), 1);
    assert_eq!(undo_edits(&undo_stack(&s)[0])[0].param_name, "Bass");
    assert!(s.chat.lock().unwrap()[1].text.contains("途中で止まった"));
}

#[tokio::test]
async fn ai_error_is_reported_without_changes() {
    let s = state();
    let mut e = FakeEngine::new(vec![Err("HTTP 503".into())]);
    let r = adjust(&mut e, &s).await;
    assert_eq!(r.error.as_deref(), Some("HTTP 503"));
    assert!(r.changes.is_empty());
    assert!(undo_stack(&s).is_empty());
}

#[tokio::test]
async fn no_changes_means_no_undo_entry() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![])]);
    let r = adjust(&mut e, &s).await;
    assert_eq!(r.error, None);
    assert_eq!(r.undo_depth, 0);
    assert_eq!(r.explanations, vec!["説明".to_string()]);
    assert!(undo_stack(&s).is_empty());
    assert!(e.applied_calls.is_empty());
}

#[tokio::test]
async fn previous_turns_are_sent_as_history() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![]), answer(vec![])]);
    adjust_with(&mut e, &s, "一回目の指示").await;
    adjust_with(&mut e, &s, "二回目").await;
    let second = e.requests[1].to_string();
    assert!(second.contains("一回目の指示"));
    let first = e.requests[0].to_string();
    assert!(!first.contains("二回目"));
}

#[tokio::test]
async fn only_the_selected_reference_clip_is_sent_as_16khz_mono() {
    let s = state();
    let samples: Vec<f32> = (0..44_100 * 60)
        .flat_map(|i| {
            let v = if i % 100 < 50 { 0.5 } else { -0.5 };
            [v, v]
        })
        .collect();
    let file = crate::sound::encode_wav(&samples, 2, 44_100).unwrap();
    let mut r =
        workflow::Reference::new("ref.wav".into(), file, crate::ai::AudioFormat::Wav).unwrap();
    r.set_start(30.0).unwrap();
    *s.reference.lock().unwrap() = Some(r);

    let mut e = FakeEngine::new(vec![answer(vec![])]);
    adjust(&mut e, &s).await;
    let content = &e.requests[0]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"];
    assert_eq!(content[1]["input_audio"]["format"], "wav");
    let sent = base64::engine::general_purpose::STANDARD
        .decode(content[1]["input_audio"]["data"].as_str().unwrap())
        .unwrap();
    let reader = hound::WavReader::new(std::io::Cursor::new(sent)).unwrap();
    assert_eq!(reader.spec().channels, 1);
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.len(), 20 * 16_000, "曲全体ではなく 20 秒だけ送る");
    assert_eq!(content[2]["input_audio"]["data"], "V0FW", "2 つ目は録音");
}

#[tokio::test]
async fn undo_stack_is_capped() {
    let s = state();
    for i in 0..MAX_UNDO_GROUPS + 3 {
        let val = format!("{i}");
        let mut e = FakeEngine::new(vec![answer(vec![change("Bass", &val)])]);
        adjust(&mut e, &s).await;
    }
    let stack = undo_stack(&s);
    assert_eq!(stack.len(), MAX_UNDO_GROUPS);
    assert_eq!(
        undo_edits(&stack[0])[0].new_display,
        "3",
        "古いものから捨てる"
    );
}

#[tokio::test]
async fn revert_restores_directly_without_calling_ai() {
    let s = state();
    s.undo_stack.lock().unwrap().extend([
        UndoEntry::Edits(vec![AppliedEdit::Param(applied("Drive", "3.0", "4.0"))]),
        UndoEntry::Edits(vec![
            AppliedEdit::Param(applied("Bass", "5.0", "6.0")),
            AppliedEdit::Param(applied("Treble", "7.2", "6.5")),
        ]),
    ]);
    let mut e = FakeEngine::new(vec![]);
    let r = workflow::revert(&mut e, &s).await.unwrap();

    assert_eq!(r.error, None);
    assert!(r.not_restored.is_empty());
    assert!(e.requests.is_empty(), "Undo で AI は呼ばない");
    assert_eq!(e.restored_calls.len(), 1);
    assert_eq!(r.changes.len(), 2);
    assert_eq!(r.changes[0].value, "5.0");
    assert_eq!(r.changes[0].previous_value, "6.0");
    assert_eq!(undo_stack(&s).len(), 1, "最後の 1 組だけ消える");
}

#[tokio::test]
async fn revert_keeps_values_that_were_not_restored() {
    let s = state();
    s.undo_stack.lock().unwrap().push(UndoEntry::Edits(vec![
        AppliedEdit::Param(applied("Bass", "5.0", "6.0")),
        AppliedEdit::Param(applied("Treble", "7.2", "6.5")),
    ]));
    let mut e = FakeEngine::new(vec![]);
    e.restore_failures.push_back(vec![RestoreFailure {
        block: "block5".into(),
        parameter: "Bass".into(),
        value: "5.0".into(),
        reason: "USB 切断".into(),
    }]);
    let r = workflow::revert(&mut e, &s).await.unwrap();

    assert!(r.error.unwrap().contains("1 個の値を戻せませんでした"));
    assert_eq!(
        r.not_restored,
        vec![RestoreTarget {
            block: "block5".into(),
            parameter: "Bass".into(),
            value: "5.0".into(),
        }]
    );
    assert_eq!(undo_stack(&s).len(), 1);
    let rem = undo_edits(&undo_stack(&s)[0]);
    assert_eq!(rem.len(), 1);
    assert_eq!(rem[0].param_name, "Bass");
}

#[tokio::test]
async fn add_block_triggers_followup_to_set_initial_params_and_undoes_as_one_snapshot() {
    let s = state();
    let mut snap1 = sample_chain();
    snap1.raw_preset = Some(vec![9, 8, 7]);
    let mut snap2 = sample_chain();
    snap2.blocks.push(BlockView {
        id: "block6".into(),
        slot: 6,
        model_number: 1,
        name: "Dynamic Hall".into(),
        enabled: true,
        locked: false,
        lock_reason: None,
        params: vec![],
        readonly_notes: vec![],
    });

    let mut e = FakeEngine::new(vec![
        Ok(AiResponse {
            explanation: "Dynamic Hall を最後に追加します".into(),
            changes: vec![],
            block_operations: vec![AiBlockOp {
                action: BlockOpAction::Add,
                block: None,
                model: Some("Dynamic Hall".into()),
                position: Some("end".into()),
                stereo: Some(false),
                amp_cab: false,
            }],
            goal_reached: false,
        }),
        Ok(AiResponse {
            explanation: "Mix を 25% に設定します".into(),
            changes: vec![AiChange {
                block: "block6".into(),
                parameter: "Mix".into(),
                value: "25 %".into(),
            }],
            block_operations: vec![],
            goal_reached: true,
        }),
    ]);
    e.chains = vec![Ok(snap1), Ok(snap2)].into();

    let r = adjust_with(&mut e, &s, "リバーブを足して Mix 25% に").await;

    assert_eq!(r.error, None);
    assert!(r.goal_reached);
    assert_eq!(e.requests.len(), 2, "追加後に再取得して 2 ステップ目が走る");
    assert!(e.request_text(1).contains("[block6] Dynamic Hall"));
    assert_eq!(r.changes.len(), 2);
    assert_eq!(r.changes[0].parameter, "ブロック追加");
    assert_eq!(r.changes[0].value, "Dynamic Hall");
    assert_eq!(r.changes[1].block, "block6");
    assert_eq!(r.changes[1].parameter, "Mix");

    // 1 つの PresetSnapshot として Undo でき、revert で戻る
    let stack = undo_stack(&s);
    assert_eq!(stack.len(), 1);
    assert!(matches!(stack[0], UndoEntry::PresetSnapshot { .. }));
    let rev = workflow::revert(&mut e, &s).await.unwrap();
    assert_eq!(rev.error, None);
    assert_eq!(rev.changes[0].parameter, "編集バッファ復元");
    assert!(undo_stack(&s).is_empty());
}

#[tokio::test]
async fn followup_step_rejects_adding_another_block() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        Ok(AiResponse {
            explanation: "追加".into(),
            changes: vec![],
            block_operations: vec![AiBlockOp {
                action: BlockOpAction::Add,
                block: None,
                model: Some("Dynamic Hall".into()),
                position: Some("end".into()),
                stereo: None,
                amp_cab: false,
            }],
            goal_reached: false,
        }),
        Ok(AiResponse {
            explanation: "さらに追加しようとする".into(),
            changes: vec![],
            block_operations: vec![AiBlockOp {
                action: BlockOpAction::Add,
                block: None,
                model: Some("Glitz".into()),
                position: Some("end".into()),
                stereo: None,
                amp_cab: false,
            }],
            goal_reached: false,
        }),
    ]);
    let r = adjust_with(&mut e, &s, "リバーブ").await;
    assert!(r.error.unwrap().contains("別のブロック追加"));
    // ステップ 1 の追加自体は実行済みなので Undo は残る
    assert_eq!(undo_stack(&s).len(), 1);
}

#[tokio::test]
async fn revert_without_history_is_an_error() {
    let s = state();
    let mut e = FakeEngine::new(vec![]);
    let err = workflow::revert(&mut e, &s).await.unwrap_err();
    assert!(err.contains("戻せる変更がありません"));
}

#[tokio::test]
async fn changed_parameters_are_carried_to_later_requests() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![change("Treble", "6.5")]), answer(vec![])]);
    adjust(&mut e, &s).await;
    assert!(!e.request_text(0).contains("アプリが変えた値"));
    adjust(&mut e, &s).await;
    assert!(e.request_text(1).contains("- block5 / Treble: 6.5"));
}

#[tokio::test]
async fn reverted_values_update_changed_parameters() {
    let s = state();
    s.undo_stack
        .lock()
        .unwrap()
        .push(UndoEntry::Edits(vec![AppliedEdit::Param(applied(
            "Treble", "7.2", "6.5",
        ))]));
    let mut e = FakeEngine::new(vec![]);
    workflow::revert(&mut e, &s).await.unwrap();
    assert_eq!(
        *s.changed.lock().unwrap(),
        vec![RestoreTarget {
            block: "block5".into(),
            parameter: "Treble".into(),
            value: "7.2".into(),
        }]
    );
}

// ---- 手動モード ----

fn pending(state: &AppState) -> Option<Prepared> {
    state.pending.lock().unwrap().clone()
}

async fn send(
    engine: &mut FakeEngine,
    state: &AppState,
    instruction: &str,
) -> Result<workflow::CycleReport, String> {
    workflow::send(engine, state, "m", instruction).await
}

fn audio_count(e: &FakeEngine, n: usize) -> usize {
    e.requests[n].to_string().matches("input_audio\":{").count()
}

#[tokio::test]
async fn send_needs_instruction_or_recording() {
    let s = state();
    let mut e = FakeEngine::new(vec![]);
    let err = send(&mut e, &s, "  ").await.unwrap_err();
    assert!(err.contains("指示を書くか、録音してから"), "{err}");
    assert!(e.requests.is_empty());
}

#[tokio::test]
async fn send_with_only_a_recording_leaves_a_placeholder_in_history() {
    let s = state();
    *s.pending.lock().unwrap() = Some(recording());
    let mut e = FakeEngine::new(vec![answer(vec![])]);
    send(&mut e, &s, " ").await.unwrap();
    assert_eq!(s.chat.lock().unwrap()[0].text, "(指示なしで調整を依頼)");
}

#[tokio::test]
async fn send_without_recording_asks_with_chain_only() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![])]);
    let r = send(&mut e, &s, "ディレイを足して").await.unwrap();
    assert_eq!(r.recording_peak_dbfs, None);
    assert_eq!(audio_count(&e, 0), 0);
    assert!(e.request_text(0).contains("音声は無い"));
}

#[tokio::test]
async fn send_attaches_the_recording_and_uses_it_up() {
    let s = state();
    *s.pending.lock().unwrap() = Some(recording());
    let mut e = FakeEngine::new(vec![answer(vec![change("Bass", "6.0")])]);
    let r = send(&mut e, &s, "").await.unwrap();
    assert_eq!(audio_count(&e, 0), 1);
    assert_eq!(r.recording_peak_dbfs, Some(-12.0));
    assert!(!r.recording_kept);
    assert_eq!(pending(&s), None, "送ったら消す");
}

#[tokio::test]
async fn send_keeps_the_recording_when_nothing_changed_and_it_failed() {
    let s = state();
    *s.pending.lock().unwrap() = Some(recording());
    let mut e = FakeEngine::new(vec![Err("HTTP 503".into())]);
    let r = send(&mut e, &s, "太く").await.unwrap();
    assert!(r.recording_kept);
    assert_eq!(pending(&s), Some(recording()), "送り直せるように残す");
}

#[tokio::test]
async fn send_uses_up_the_recording_once_values_changed_even_if_it_failed_later() {
    let s = state();
    *s.pending.lock().unwrap() = Some(recording());
    let mut e = FakeEngine::new(vec![answer(vec![
        change("Bass", "6.0"),
        change("Treble", "6.5"),
    ])]);
    let a = applied("Bass", "5.0", "6.0");
    e.apply_results.push_back(Ok(OperationsOutcome {
        steps: vec![AppliedStep::Param(a.clone())],
        undo: Some(UndoEntry::Edits(vec![AppliedEdit::Param(a)])),
        structure_changed: false,
        added_or_replaced: false,
        error: Some("USB 切断".into()),
    }));
    let r = send(&mut e, &s, "太く").await.unwrap();
    assert!(r.error.is_some());
    assert!(!r.recording_kept, "音が変わったので古い録音は使わない");
    assert_eq!(pending(&s), None);
}

// ---- 自動ループ ----

async fn run_loop(e: &mut FakeEngine, s: &AppState, max: usize) -> workflow::LoopSummary {
    workflow::auto_loop(e, s, "m", "リファレンスに近づけて", max)
        .await
        .unwrap()
}

#[tokio::test]
async fn loop_repeats_until_ai_says_the_goal_is_reached() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        answer(vec![change("Bass", "6.0")]),
        answer(vec![change("Bass", "6.5")]),
        reached(),
    ]);
    let r = run_loop(&mut e, &s, 10).await;

    assert_eq!(r.iterations, 3);
    assert!(r.reason.contains("目標に届いた"), "{}", r.reason);
    assert_eq!(e.records, 3);
    assert_eq!(e.settles, 2, "値を変えた後は反映を待ってから録る");
    assert_eq!(r.undo_depth, 2, "1 回ごとに Undo できる");
    let numbers: Vec<(usize, usize)> = e.iterations.iter().map(|i| (i.iteration, i.max)).collect();
    assert_eq!(numbers, vec![(1, 10), (2, 10), (3, 10)]);
    assert!(e.iterations[2].report.goal_reached);
    for n in 0..3 {
        assert_eq!(audio_count(&e, n), 1);
        assert!(e.request_text(n).contains("リファレンスに近づけて"));
    }
    let chat = s.chat.lock().unwrap().clone();
    assert_eq!(chat[0].text, "(自動ループ 1 回目)");
    assert_eq!(chat[4].text, "(自動ループ 3 回目)");
}

#[tokio::test]
async fn loop_stops_at_the_iteration_limit() {
    let s = state();
    let answers = (0..3)
        .map(|i| answer(vec![change("Bass", &format!("{}", i + 1))]))
        .collect();
    let mut e = FakeEngine::new(answers);
    let r = run_loop(&mut e, &s, 3).await;
    assert_eq!(r.iterations, 3);
    assert!(r.reason.contains("上限の 3 回"), "{}", r.reason);
    assert_eq!(e.records, 3);
}

#[tokio::test]
async fn loop_stops_when_ai_returns_nothing_to_change() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![])]);
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 1);
    assert!(
        r.reason.contains("自動で変えられる値を返さなかった"),
        "{}",
        r.reason
    );
}

#[tokio::test]
async fn loop_continues_when_goal_is_reached_but_values_were_still_changed() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        Ok(AiResponse {
            explanation: "ほぼ届いた。最後に少しだけ".into(),
            changes: vec![change("Bass", "5.5")],
            block_operations: vec![],
            goal_reached: true,
        }),
        reached(),
    ]);
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 2, "変えた後の音を聴いて確かめる");
}

#[tokio::test]
async fn loop_tolerates_failures_but_stops_after_too_many_in_a_row() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        Err("HTTP 503".into()),
        answer(vec![change("Bass", "6.0")]),
        Err("HTTP 503".into()),
        Err("HTTP 503".into()),
    ]);
    e.recordings = vec![
        Ok(recording()),
        Ok(recording()),
        Ok(recording()),
        Err("録音がほぼ無音でした".into()),
        Ok(recording()),
    ]
    .into();
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 5);
    assert!(
        r.reason
            .contains(&format!("{MAX_CONSECUTIVE_FAILURES} 回続けて失敗")),
        "{}",
        r.reason
    );
    assert_eq!(e.requests.len(), 4, "録音に失敗した回は AI に聞かない");
    assert_eq!(
        e.iterations[3].report.error.as_deref(),
        Some("録音がほぼ無音でした")
    );
    assert_eq!(e.iterations[3].report.undo_depth, 1);
}

#[tokio::test]
async fn stop_signal_ends_the_loop_before_the_next_recording_is_sent() {
    let s = state();
    let stop = s.loop_stop.clone();
    let mut e = FakeEngine::new(vec![answer(vec![change("Bass", "6.0")])]);
    e.on_record = Some(Box::new(move |n| {
        if n == 2 {
            stop.store(true, Ordering::SeqCst);
        }
    }));
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 1);
    assert!(r.reason.contains("停止ボタン"), "{}", r.reason);
    assert_eq!(e.requests.len(), 1, "止めた後の録音は送らない");
}

#[tokio::test]
async fn stop_signal_during_ai_request_skips_executing_its_answer() {
    let s = state();
    let stop = s.loop_stop.clone();
    let mut e = FakeEngine::new(vec![answer(vec![change("Bass", "6.0")])]);
    e.on_ask = Some(Box::new(move |_| stop.store(true, Ordering::SeqCst)));
    let r = run_loop(&mut e, &s, 10).await;
    assert!(r.reason.contains("停止ボタン"), "{}", r.reason);
    assert!(e.applied_calls.is_empty(), "止めた後は操作しない");
    assert_eq!(
        e.iterations[0].report.error.as_deref(),
        Some(STOPPED_BEFORE_EXECUTE)
    );
    assert_eq!(r.undo_depth, 0);
}

#[tokio::test]
async fn instructions_added_during_the_loop_are_used_from_the_next_iteration() {
    let s = state();
    let extra = s.loop_extra.clone();
    let mut e = FakeEngine::new(vec![
        answer(vec![change("Bass", "6.0")]),
        answer(vec![change("Bass", "6.5")]),
        reached(),
    ]);
    e.on_ask = Some(Box::new(move |n| {
        if n == 1 {
            workflow::add_loop_instruction(&mut extra.lock().unwrap(), "やっぱり歪み控えめに")
                .unwrap();
        }
    }));
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 3);
    assert!(!e.request_text(0).contains("歪み控えめに"));
    assert!(e.request_text(1).contains("やっぱり歪み控えめに"));
    assert!(e.request_text(2).contains("やっぱり歪み控えめに"));
}
