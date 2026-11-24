//! AI API 呼び出し (HTTP の層) のテスト。偽の API サーバーを立てて確かめる。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::ai;
use crate::config::ResolvedSettings;

const NO_WAIT: [Duration; 2] = [Duration::ZERO, Duration::ZERO];

fn settings(base: &str) -> ResolvedSettings {
    ResolvedSettings {
        api_base_url: base.to_string(),
        api_key: "test-key".into(),
        model: "test-model".into(),
    }
}

fn ok_body() -> serde_json::Value {
    let content = json!({
        "explanation": "下げた",
        "changes": [{
            "block": "block5", "parameter": "Treble", "value": "6.0"
        }],
        "goal_reached": false
    });
    json!({ "choices": [{ "message": { "content": content.to_string() } }] })
}

fn overloaded() -> ResponseTemplate {
    ResponseTemplate::new(503).set_body_string(r#"{"error":{"message":"high demand"}}"#)
}

/// 再試行の通知を記録する。
fn recorder() -> (Arc<Mutex<Vec<String>>>, impl Fn(String) + Send + Sync) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let l = log.clone();
    (log, move |m| l.lock().unwrap().push(m))
}

async fn call(
    server: &MockServer,
    base_suffix: &str,
) -> (Result<ai::AiResponse, String>, Vec<String>) {
    let (log, on_retry) = recorder();
    let s = settings(&format!("{}{base_suffix}", server.uri()));
    // 再試行の回数が壊れて無限に繰り返しても、すぐ失敗として分かるようにする
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        ai::call_with_delays(&s, &json!({ "model": "test-model" }), &NO_WAIT, &on_retry),
    )
    .await
    .expect("再試行が終わらない");
    let retries = log.lock().unwrap().clone();
    (r, retries)
}

#[tokio::test]
async fn sends_bearer_key_to_chat_completions_and_parses_answer() {
    let server = MockServer::start().await;

    // base URL の末尾のスラッシュの有無に関わらず同じ URL になる
    for suffix in ["/v1", "/v1/"] {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .expect(1)
            .mount(&server)
            .await;
        let (r, retries) = call(&server, suffix).await;
        let r = r.unwrap();
        assert_eq!(r.changes[0].block, "block5");
        assert_eq!(r.changes[0].parameter, "Treble");
        assert_eq!(r.changes[0].value, "6.0");
        assert!(retries.is_empty());
        server.verify().await;
    }
}

#[tokio::test]
async fn retries_when_overloaded_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(overloaded())
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .expect(1)
        .mount(&server)
        .await;

    let (r, retries) = call(&server, "/v1").await;
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(retries.len(), 1);
    assert!(retries[0].contains("503"));
}

#[tokio::test]
async fn gives_up_after_retries_and_reports_last_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(overloaded())
        .expect(1 + NO_WAIT.len() as u64)
        .mount(&server)
        .await;

    let (r, retries) = call(&server, "/v1").await;
    let err = r.unwrap_err();
    assert!(err.contains("503") && err.contains("high demand"), "{err}");
    assert_eq!(retries.len(), NO_WAIT.len());
}

#[tokio::test]
async fn retries_on_rate_limit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
        .mount(&server)
        .await;
    let (r, retries) = call(&server, "/v1").await;
    assert!(r.is_ok());
    assert_eq!(retries.len(), 1);
}

#[tokio::test]
async fn does_not_retry_client_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;

    let (r, retries) = call(&server, "/v1").await;
    let err = r.unwrap_err();
    assert!(err.contains("401") && err.contains("bad key"), "{err}");
    assert!(retries.is_empty(), "キーの誤りは待っても直らない");
}

#[tokio::test]
async fn non_json_body_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy</html>"))
        .mount(&server)
        .await;
    let (r, _) = call(&server, "/v1").await;
    assert!(r.unwrap_err().contains("JSON ではありません"));
}

#[tokio::test]
async fn unreachable_server_is_an_error_with_cause() {
    // 何も待ち受けていないポート
    let s = settings("http://127.0.0.1:9/v1");
    let r = ai::call_with_delays(&s, &json!({}), &NO_WAIT, &|_| {}).await;
    let err = r.unwrap_err();
    assert!(err.contains("接続できません"), "{err}");
}

#[tokio::test]
async fn real_retry_delays_are_bounded() {
    // 画面が長く固まらないよう、再試行の待ちの合計に上限を設ける
    let total: Duration = ai::RETRY_DELAYS.iter().sum();
    assert!(total <= Duration::from_secs(15), "{total:?}");
    assert!(
        !ai::RETRY_DELAYS.is_empty(),
        "混雑時に一度も再試行しないのは困る"
    );
}

// ---- エフェクト一覧の検索 (ツール呼び出し) ----

/// 決めた順に応答を返す偽のサーバー。
struct Script(std::sync::atomic::AtomicUsize, Vec<serde_json::Value>);

impl wiremock::Respond for Script {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let i = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let body = self.1.get(i).expect("台本より多く問い合わせた").clone();
        ResponseTemplate::new(200).set_body_json(body)
    }
}

fn tool_reply(calls: Vec<serde_json::Value>) -> serde_json::Value {
    json!({ "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": calls } }] })
}

fn search(id: &str, args: serde_json::Value) -> serde_json::Value {
    json!({ "id": id, "type": "function", "function": { "name": "search_effects", "arguments": args.to_string() } })
}

fn submit() -> serde_json::Value {
    let args =
        json!({ "explanation": "Heir Apparent を勧める", "changes": [], "goal_reached": false });
    json!({ "id": "s", "type": "function", "function": { "name": "submit_changes", "arguments": args.to_string() } })
}

async fn call_with_script(
    replies: Vec<serde_json::Value>,
) -> (
    Result<ai::AiResponse, String>,
    Vec<serde_json::Value>,
    Vec<String>,
) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Script(Default::default(), replies))
        .mount(&server)
        .await;
    let (log, on_progress) = recorder();
    let s = settings(&format!("{}/v1", server.uri()));
    let body = json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "BB 系に変えたい" }],
        "tool_choice": "required"
    });
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        ai::call_with_delays(&s, &body, &NO_WAIT, &on_progress),
    )
    .await
    .expect("終わらない");
    let sent = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    let progress = log.lock().unwrap().clone();
    (r, sent, progress)
}

#[tokio::test]
async fn search_results_are_sent_back_before_the_answer() {
    let (r, sent, progress) = call_with_script(vec![
        tool_reply(vec![search(
            "c1",
            json!({ "category": "drive", "query": "BB" }),
        )]),
        tool_reply(vec![submit()]),
    ])
    .await;
    assert_eq!(r.unwrap().explanation, "Heir Apparent を勧める");
    assert_eq!(sent.len(), 2);
    let messages = sent[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "元の発言 + AI の検索依頼 + 検索結果");
    assert_eq!(messages[1]["tool_calls"][0]["id"], "c1");
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "c1");
    let result: serde_json::Value =
        serde_json::from_str(messages[2]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["models"][0]["name"], "Heir Apparent",
        "HX Stomp の一覧から引く"
    );
    assert_eq!(sent[1]["tool_choice"], "required", "まだ検索してよい");
    assert_eq!(progress, vec!["エフェクト一覧を検索中 (drive / BB)"]);
}

#[tokio::test]
async fn after_the_search_limit_the_answer_is_forced() {
    let mut replies: Vec<_> = (0..ai::MAX_SEARCHES)
        .map(|i| tool_reply(vec![search(&format!("c{i}"), json!({ "query": "Klon" }))]))
        .collect();
    replies.push(tool_reply(vec![submit()]));
    let (r, sent, _) = call_with_script(replies).await;
    assert!(r.is_ok(), "{r:?}");
    let last = sent.last().unwrap();
    assert_eq!(
        last["tool_choice"],
        json!({ "type": "function", "function": { "name": "submit_changes" } })
    );
    assert_eq!(sent[ai::MAX_SEARCHES - 1]["tool_choice"], "required");
}

#[tokio::test]
async fn searching_beyond_the_limit_is_an_error() {
    let replies: Vec<_> = (0..=ai::MAX_SEARCHES)
        .map(|i| tool_reply(vec![search(&format!("c{i}"), json!({ "query": "Klon" }))]))
        .collect();
    let (r, sent, _) = call_with_script(replies).await;
    assert!(r.unwrap_err().contains("答えを返しませんでした"));
    assert_eq!(sent.len(), ai::MAX_SEARCHES + 1);
}
