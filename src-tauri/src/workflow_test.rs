//! サイクルと Undo の流れのテスト。
//!
//! AI の応答は台本で与え、自動操作は本物の `automation::execute` を偽のデスクトップに
//! 対して走らせる。

use std::collections::VecDeque;

use serde_json::Value;

use crate::ai::{ActionKind, AiAction, AiResponse, RestoreTarget};
use crate::automation::fake::{Ev, FakeDesktop, HX_PID, HX_WINDOW, hx_rect};
use crate::automation::{self, Outcome};
use crate::capture::{Screenshot, TargetWindow};
use crate::config::StoredSettings;
use crate::workflow::{self, AppState, Engine, MAX_ROUNDS, MAX_UNDO_GROUPS};

struct FakeEngine {
    desktop: FakeDesktop,
    answers: VecDeque<Result<AiResponse, String>>,
    /// AI に送ったリクエスト本文。
    requests: Vec<Value>,
    captures: usize,
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
    })
}

fn target(param: &str, value: &str) -> RestoreTarget {
    RestoreTarget {
        block: "Amp".into(),
        parameter: param.into(),
        value: value.into(),
    }
}

async fn adjust(engine: &mut FakeEngine, state: &AppState) -> workflow::CycleReport {
    workflow::adjust(engine, state, "m", "太く", b"WAV", -12.0)
        .await
        .unwrap()
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
    workflow::adjust(&mut e, &s, "m", "一回目の指示", b"WAV", -12.0)
        .await
        .unwrap();
    workflow::adjust(&mut e, &s, "m", "二回目", b"WAV", -12.0)
        .await
        .unwrap();
    let second = e.requests[1].to_string();
    assert!(second.contains("一回目の指示"));
    let first = e.requests[0].to_string();
    assert!(!first.contains("二回目"));
}

#[tokio::test]
async fn reference_audio_is_sent_when_set() {
    let s = state();
    *s.reference.lock().unwrap() = Some(workflow::Reference {
        bytes: b"MP3DATA".to_vec(),
        format: crate::ai::AudioFormat::Mp3,
    });
    let mut e = FakeEngine::new(vec![answer(vec![])]);
    adjust(&mut e, &s).await;
    let content = &e.requests[0]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["content"];
    assert_eq!(content[1]["input_audio"]["format"], "mp3");
    assert_eq!(content[2]["input_audio"]["format"], "wav");
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
