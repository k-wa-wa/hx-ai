//! AI API 呼び出し (HTTP の層) のテスト。偽の API サーバーを立てて確かめる。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::ai::{self, ActionKind};
use crate::config::ResolvedSettings;

const NO_WAIT: [Duration; 2] = [Duration::ZERO, Duration::ZERO];

fn settings(base: &str) -> ResolvedSettings {
    ResolvedSettings {
        api_base_url: base.to_string(),
        api_key: "test-key".into(),
        model: "test-model".into(),
        target_app: "HX Edit".into(),
    }
}

fn ok_body() -> serde_json::Value {
    let content = json!({
        "explanation": "下げた",
        "actions": [{
            "type": "set_value", "block": "Amp", "parameter": "Treble",
            "x": 433, "y": 890, "value": "6.0", "previous_value": "7.2"
        }],
        "needs_another_round": false
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
    let r = ai::call_with_delays(&s, &json!({ "model": "test-model" }), &NO_WAIT, &on_retry).await;
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
        assert_eq!(r.actions[0].kind, ActionKind::SetValue);
        assert_eq!(r.actions[0].previous_value, "7.2");
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
