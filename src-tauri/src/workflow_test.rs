//! サイクル・Undo・手動送信・自動ループの流れのテスト。
//!
//! AI の応答と録音は台本で与え、自動操作は本物の `automation::execute` を偽のデスクトップに
//! 対して走らせる。

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use base64::Engine as _;
use serde_json::Value;

use crate::ai::{ActionKind, AiAction, AiResponse, RestoreTarget};
use crate::automation::fake::{Ev, FakeDesktop, HX_PID, HX_WINDOW, hx_rect};
use crate::automation::{self, Outcome};
use crate::capture::{Screenshot, TargetWindow};
use crate::config::StoredSettings;
use crate::sound::Prepared;
use crate::workflow::{
    self, AdjustInput, AppState, Engine, LoopIteration, MAX_CONSECUTIVE_FAILURES, MAX_ROUNDS,
    MAX_UNDO_GROUPS, STOPPED_BEFORE_EXECUTE,
};

struct FakeEngine {
    desktop: FakeDesktop,
    answers: VecDeque<Result<AiResponse, String>>,
    /// AI に送ったリクエスト本文。
    requests: Vec<Value>,
    captures: usize,
    /// 自動ループの録音の台本。尽きたら `recording()` を返す。
    recordings: VecDeque<Result<Prepared, String>>,
    records: usize,
    /// n 回目 (1 始まり) の録音の直後に呼ぶ。
    on_record: Option<Box<dyn FnMut(usize)>>,
    /// n 回目 (1 始まり) の AI への問い合わせの直後に呼ぶ。
    on_ask: Option<Box<dyn FnMut(usize)>>,
    iterations: Vec<LoopIteration>,
    settles: usize,
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
        Self::with_desktop(FakeDesktop::new(), answers)
    }

    fn with_desktop(desktop: FakeDesktop, answers: Vec<Result<AiResponse, String>>) -> Self {
        Self {
            desktop,
            answers: answers.into(),
            requests: vec![],
            captures: 0,
            recordings: VecDeque::new(),
            records: 0,
            on_record: None,
            on_ask: None,
            iterations: vec![],
            settles: 0,
        }
    }

    /// n 回目 (0 始まり) のリクエストのテキスト部分。
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

impl Engine for FakeEngine {
    fn progress(&self, _stage: &str, _message: String) {}

    async fn capture(&mut self) -> Result<Screenshot, String> {
        self.captures += 1;
        Ok(Screenshot {
            png: b"PNG".to_vec(),
            width: 2000,
            height: 2000,
            window: TargetWindow {
                window_id: HX_WINDOW,
                pid: HX_PID as u32,
                app_name: "HX Edit".into(),
                title: "HX Edit".into(),
                rect: hx_rect(),
            },
        })
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

    async fn execute(
        &mut self,
        actions: Vec<AiAction>,
        window: TargetWindow,
    ) -> Result<Outcome, String> {
        Ok(automation::execute(&mut self.desktop, &actions, &window))
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

fn set(param: &str, x: f64, prev: &str, value: &str) -> AiAction {
    AiAction {
        kind: ActionKind::SetValue,
        block: "Amp".into(),
        parameter: param.into(),
        x,
        y: 800.0,
        value: value.into(),
        previous_value: prev.into(),
        // 偽の画面で y = 800 は 15 番目の数値の行
        field: 15,
    }
}

fn select(block: &str) -> AiAction {
    AiAction {
        kind: ActionKind::SelectBlock,
        block: block.into(),
        parameter: String::new(),
        x: 580.0,
        y: 190.0,
        value: String::new(),
        previous_value: String::new(),
        field: 0,
    }
}

fn answer(actions: Vec<AiAction>) -> Result<AiResponse, String> {
    let needs_another_round = actions
        .last()
        .is_some_and(|a| a.kind == ActionKind::SelectBlock);
    Ok(AiResponse {
        explanation: "説明".into(),
        actions,
        needs_another_round,
        goal_reached: false,
    })
}

/// 「目標に届いた」とだけ返す応答。
fn reached() -> Result<AiResponse, String> {
    Ok(AiResponse {
        explanation: "目標に届いた".into(),
        actions: vec![],
        needs_another_round: false,
        goal_reached: true,
    })
}

fn target(param: &str, value: &str) -> RestoreTarget {
    RestoreTarget {
        block: "Amp".into(),
        parameter: param.into(),
        value: value.into(),
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

fn undo_stack(state: &AppState) -> Vec<Vec<RestoreTarget>> {
    state.undo_stack.lock().unwrap().clone()
}

#[tokio::test]
async fn adjust_executes_changes_and_records_them_for_undo() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![
        set("Bass", 300.0, "5.0", "6.0"),
        set("Treble", 500.0, "7.2", "6.5"),
    ])]);
    let r = adjust(&mut e, &s).await;

    assert_eq!(r.error, None);
    assert_eq!(r.changes.len(), 2);
    assert_eq!(r.rounds, 1);
    assert_eq!(r.undo_depth, 1);
    assert_eq!(
        undo_stack(&s),
        vec![vec![target("Bass", "5.0"), target("Treble", "7.2")]]
    );
    assert_eq!(e.captures, 1);
    // 会話の履歴に指示と結果が残る
    let chat = s.chat.lock().unwrap().clone();
    assert_eq!(chat.len(), 2);
    assert_eq!(chat[0].text, "太く");
    assert!(chat[1].text.contains("Treble: 7.2 → 6.5"));
}

#[tokio::test]
async fn selecting_a_block_recaptures_and_asks_again() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        answer(vec![set("Bass", 300.0, "5.0", "6.0"), select("Delay")]),
        answer(vec![set("Mix", 500.0, "20", "30")]),
    ]);
    let r = adjust(&mut e, &s).await;

    assert_eq!(r.error, None);
    assert_eq!(r.rounds, 2);
    assert_eq!(e.captures, 2, "ブロックを選んだ後に撮り直す");
    // 2 回目の問い合わせには、1 回目に実行したことを伝える
    let second = e.request_text(1);
    assert!(second.contains("ブロック「Delay」を選んだ"), "{second}");
    assert!(second.contains("Bass: 5.0 → 6.0"), "{second}");
    // 撮り直した後は音声を送らず、1 回目の説明で方針を伝える
    assert!(!e.requests[1].to_string().contains("input_audio"));
    assert!(e.requests[0].to_string().contains("input_audio"));
    assert!(second.contains("- 説明"), "{second}");
    // ブロック選択は値の変更として数えない
    assert_eq!(r.changes.len(), 2);
    assert_eq!(undo_stack(&s)[0].len(), 2);
}

#[tokio::test]
async fn block_selection_stops_at_max_rounds() {
    let s = state();
    let answers = (0..MAX_ROUNDS)
        .map(|_| answer(vec![select("Delay")]))
        .collect();
    let mut e = FakeEngine::new(answers);
    let r = adjust(&mut e, &s).await;

    assert_eq!(e.captures, MAX_ROUNDS);
    assert!(
        e.request_text(MAX_ROUNDS - 1).contains("最後の撮影"),
        "最後の問い合わせでは選び直さないよう伝える"
    );
    assert!(r.error.unwrap().contains("上限"));
    assert!(e.answers.is_empty());
}

#[tokio::test]
async fn invalid_answer_executes_nothing() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![
        set("Bass", 300.0, "5.0", "6.0"),
        set("Mid", 400.0, "5.0", "6dB"),
    ])]);
    let r = adjust(&mut e, &s).await;

    assert!(r.error.unwrap().contains("数値ではありません"));
    assert!(e.desktop.events.is_empty(), "1 つでも不正なら何もしない");
    assert!(r.changes.is_empty());
    assert!(undo_stack(&s).is_empty());
}

#[tokio::test]
async fn partial_failure_keeps_executed_changes_undoable() {
    let s = state();
    let mut first_enter = true;
    let desktop = FakeDesktop::new().on_event(move |ev, w| {
        if *ev == Ev::Enter && first_enter {
            first_enter = false;
            w.frontmost = Some(42);
        }
    });
    let mut e = FakeEngine::with_desktop(
        desktop,
        vec![answer(vec![
            set("Bass", 300.0, "5.0", "6.0"),
            set("Treble", 500.0, "7.2", "6.5"),
        ])],
    );
    let r = adjust(&mut e, &s).await;

    assert!(r.error.unwrap().contains("最前面"));
    assert_eq!(r.changes.len(), 1);
    assert_eq!(undo_stack(&s), vec![vec![target("Bass", "5.0")]]);
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
async fn no_actions_means_no_undo_entry() {
    let s = state();
    let mut e = FakeEngine::new(vec![answer(vec![])]);
    let r = adjust(&mut e, &s).await;
    assert_eq!(r.error, None);
    assert_eq!(r.undo_depth, 0);
    assert_eq!(r.explanations, vec!["説明".to_string()]);
    assert!(undo_stack(&s).is_empty());
    assert!(e.desktop.events.is_empty());
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
    // 60 秒・44.1kHz ステレオの音源
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
        let prev = format!("{i}");
        let mut e = FakeEngine::new(vec![answer(vec![set("Bass", 300.0, &prev, "9")])]);
        adjust(&mut e, &s).await;
    }
    let stack = undo_stack(&s);
    assert_eq!(stack.len(), MAX_UNDO_GROUPS);
    assert_eq!(stack[0][0].value, "3", "古いものから捨てる");
}

#[tokio::test]
async fn revert_restores_and_pops_the_last_group() {
    let s = state();
    s.undo_stack.lock().unwrap().extend([
        vec![target("Drive", "3.0")],
        vec![target("Bass", "5.0"), target("Treble", "7.2")],
    ]);
    let mut e = FakeEngine::new(vec![answer(vec![
        set("Bass", 300.0, "6.0", "5.0"),
        set("Treble", 500.0, "6.5", "7.2"),
    ])]);
    let r = workflow::revert(&mut e, &s, "m").await.unwrap();

    assert_eq!(r.error, None);
    assert!(r.not_restored.is_empty());
    assert_eq!(
        undo_stack(&s),
        vec![vec![target("Drive", "3.0")]],
        "最後の 1 組だけ消える"
    );
    let text = e.request_text(0);
    assert!(text.contains("Amp / Bass を 5.0 に戻す"), "{text}");
    // Undo には録音もリファレンスも送らない
    let content = &e.requests[0]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"];
    assert_eq!(content.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn revert_keeps_values_that_were_not_restored() {
    let s = state();
    s.undo_stack
        .lock()
        .unwrap()
        .push(vec![target("Bass", "5.0"), target("Treble", "7.2")]);
    let mut e = FakeEngine::new(vec![answer(vec![set("Treble", 500.0, "6.5", "7.2")])]);
    let r = workflow::revert(&mut e, &s, "m").await.unwrap();

    assert!(r.error.unwrap().contains("1 個の値を戻せませんでした"));
    assert_eq!(r.not_restored, vec![target("Bass", "5.0")]);
    assert_eq!(undo_stack(&s), vec![vec![target("Bass", "5.0")]]);
}

#[tokio::test]
async fn revert_rejects_values_that_were_not_recorded() {
    let s = state();
    s.undo_stack
        .lock()
        .unwrap()
        .push(vec![target("Bass", "5.0")]);
    let mut e = FakeEngine::new(vec![answer(vec![set("Bass", 300.0, "6.0", "8.0")])]);
    let r = workflow::revert(&mut e, &s, "m").await.unwrap();

    assert!(r.error.is_some());
    assert!(e.desktop.events.is_empty(), "控えていない値は打ち込まない");
    assert_eq!(undo_stack(&s), vec![vec![target("Bass", "5.0")]]);
}

#[tokio::test]
async fn revert_after_selecting_block() {
    let s = state();
    s.undo_stack
        .lock()
        .unwrap()
        .push(vec![target("Bass", "5.0"), target("Mix", "20")]);
    let mut e = FakeEngine::new(vec![
        answer(vec![set("Bass", 300.0, "6.0", "5.0"), select("Delay")]),
        answer(vec![set("Mix", 500.0, "30", "20")]),
    ]);
    let r = workflow::revert(&mut e, &s, "m").await.unwrap();
    assert_eq!(r.error, None);
    assert!(undo_stack(&s).is_empty());
}

#[tokio::test]
async fn revert_without_history_is_an_error() {
    let s = state();
    let mut e = FakeEngine::new(vec![]);
    let err = workflow::revert(&mut e, &s, "m").await.unwrap_err();
    assert!(err.contains("戻せる変更がありません"));
    assert!(e.requests.is_empty());
}

#[tokio::test]
async fn changed_parameters_are_carried_to_later_requests() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        answer(vec![set("Treble", 500.0, "7.2", "6.5")]),
        answer(vec![]),
    ]);
    adjust(&mut e, &s).await;
    assert!(!e.request_text(0).contains("アプリが打ち込んだ値"));
    adjust(&mut e, &s).await;
    assert!(e.request_text(1).contains("- Amp / Treble: 6.5"));
}

#[tokio::test]
async fn reverted_values_update_changed_parameters() {
    let s = state();
    s.undo_stack
        .lock()
        .unwrap()
        .push(vec![target("Treble", "7.2")]);
    let mut e = FakeEngine::new(vec![answer(vec![set("Treble", 500.0, "6.5", "7.2")])]);
    workflow::revert(&mut e, &s, "m").await.unwrap();
    assert_eq!(*s.changed.lock().unwrap(), vec![target("Treble", "7.2")]);
}

// ---- 手動モード: 録音を添えて送る ----

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

/// n 回目のリクエストで送った音声の数。
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
async fn send_without_recording_asks_with_screen_only() {
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
    let mut e = FakeEngine::new(vec![answer(vec![set("Bass", 300.0, "5.0", "6.0")])]);
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
    let mut first_enter = true;
    let desktop = FakeDesktop::new().on_event(move |ev, w| {
        if *ev == Ev::Enter && first_enter {
            first_enter = false;
            w.frontmost = Some(42);
        }
    });
    let mut e = FakeEngine::with_desktop(
        desktop,
        vec![answer(vec![
            set("Bass", 300.0, "5.0", "6.0"),
            set("Treble", 500.0, "7.2", "6.5"),
        ])],
    );
    let r = send(&mut e, &s, "太く").await.unwrap();
    assert!(r.error.is_some());
    assert!(r.interrupted, "ガードレールで止まった");
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
        answer(vec![set("Bass", 300.0, "5.0", "6.0")]),
        answer(vec![set("Bass", 300.0, "6.0", "6.5")]),
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
    // 毎回の録音を送り、指示は毎回のリクエストに入れる
    for n in 0..3 {
        assert_eq!(audio_count(&e, n), 1);
        assert!(e.request_text(n).contains("リファレンスに近づけて"));
    }
    // 会話の履歴には長い指示を繰り返さず、何回目かだけを残す
    let chat = s.chat.lock().unwrap().clone();
    assert_eq!(chat[0].text, "(自動ループ 1 回目)");
    assert_eq!(chat[4].text, "(自動ループ 3 回目)");
}

#[tokio::test]
async fn loop_stops_at_the_iteration_limit() {
    let s = state();
    let answers = (0..3)
        .map(|i| {
            answer(vec![set(
                "Bass",
                300.0,
                &format!("{i}"),
                &format!("{}", i + 1),
            )])
        })
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
            actions: vec![set("Bass", 300.0, "5.0", "5.5")],
            needs_another_round: false,
            goal_reached: true,
        }),
        reached(),
    ]);
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 2, "変えた後の音を聴いて確かめる");
}

#[tokio::test]
async fn loop_stops_when_a_guard_interrupts_the_operation() {
    let s = state();
    let desktop = FakeDesktop::new().on_event(|ev, w| {
        if *ev == Ev::Enter {
            w.frontmost = Some(42);
        }
    });
    let mut e = FakeEngine::with_desktop(
        desktop,
        vec![answer(vec![
            set("Bass", 300.0, "5.0", "6.0"),
            set("Treble", 500.0, "7.2", "6.5"),
        ])],
    );
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(r.iterations, 1);
    assert!(r.reason.contains("自動操作が止まった"), "{}", r.reason);
    assert_eq!(r.undo_depth, 1, "止まる前の変更は戻せる");
}

#[tokio::test]
async fn loop_tolerates_failures_but_stops_after_too_many_in_a_row() {
    let s = state();
    let mut e = FakeEngine::new(vec![
        Err("HTTP 503".into()),
        answer(vec![set("Bass", 300.0, "5.0", "6.0")]),
        Err("HTTP 503".into()),
        // 録音の失敗 (下で台本に入れる) もここで 1 回と数える
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
    // 録音に失敗した回の報告にも、画面の「戻す」の数を合わせるための Undo の数を入れる
    assert_eq!(e.iterations[3].report.undo_depth, 1);
}

#[tokio::test]
async fn stop_signal_ends_the_loop_before_the_next_recording_is_sent() {
    let s = state();
    let stop = s.loop_stop.clone();
    let mut e = FakeEngine::new(vec![answer(vec![set("Bass", 300.0, "5.0", "6.0")])]);
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
    let mut e = FakeEngine::new(vec![answer(vec![set("Bass", 300.0, "5.0", "6.0")])]);
    e.on_ask = Some(Box::new(move |_| stop.store(true, Ordering::SeqCst)));
    let r = run_loop(&mut e, &s, 10).await;
    assert!(r.reason.contains("停止ボタン"), "{}", r.reason);
    assert!(e.desktop.events.is_empty(), "止めた後は操作しない");
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
        answer(vec![set("Bass", 300.0, "5.0", "6.0")]),
        reached(),
    ]);
    e.on_ask = Some(Box::new(move |n| {
        if n == 1 {
            workflow::add_loop_instruction(&mut extra.lock().unwrap(), "もう少し歪ませて").unwrap();
        }
    }));
    run_loop(&mut e, &s, 10).await;
    assert!(!e.request_text(0).contains("もう少し歪ませて"));
    let second = e.request_text(1);
    assert!(second.contains("リファレンスに近づけて"), "{second}");
    assert!(second.contains("- もう少し歪ませて"), "{second}");
}

#[tokio::test]
async fn loop_clears_the_previous_stop_signal_and_extra_instructions() {
    let s = state();
    s.loop_stop.store(true, Ordering::SeqCst);
    s.loop_extra.lock().unwrap().push("前のループの指示".into());
    let mut e = FakeEngine::new(vec![reached()]);
    let r = run_loop(&mut e, &s, 10).await;
    assert_eq!(
        r.iterations, 1,
        "前に押した停止で始まらない、ということがない"
    );
    assert!(!e.request_text(0).contains("前のループの指示"));
}

#[tokio::test]
async fn loop_rejects_invalid_iteration_counts() {
    let s = state();
    let mut e = FakeEngine::new(vec![]);
    assert!(workflow::auto_loop(&mut e, &s, "m", "x", 0).await.is_err());
    assert!(
        workflow::auto_loop(&mut e, &s, "m", "x", workflow::MAX_LOOP_ITERATIONS + 1)
            .await
            .is_err()
    );
    assert_eq!(e.records, 0);
}
