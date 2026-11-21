//! OpenAI 互換の Chat Completions API で AI に音とスクショを渡し、操作を JSON で受け取る。
//!
//! 既定の接続先は Gemini の OpenAI 互換エンドポイントである。音声は `input_audio`、
//! 画像は `image_url` (data URL)、出力は `response_format` の JSON スキーマで指定する。

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog;
use crate::config::ResolvedSettings;

/// 1 回の応答で受け付ける値の変更の上限。これを超える応答は丸ごと捨てる。
pub const MAX_SET_VALUE_ACTIONS: usize = 12;

/// 会話の文脈として送る過去のやりとりの数。
pub const MAX_HISTORY_TURNS: usize = 8;

/// 過去のやりとり 1 つあたりに送る文字数の上限。変えた値は別に一覧で送るので、説明は短くてよい。
pub const MAX_HISTORY_CHARS: usize = 400;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// 1 回の問い合わせで、エフェクト一覧を検索してよい回数。超えたら答えを返させる。
pub const MAX_SEARCHES: usize = 3;
/// エフェクト一覧を検索する関数の名前。
pub const SEARCH_TOOL: &str = "search_effects";
/// 答え (操作) を返す関数の名前。
pub const SUBMIT_TOOL: &str = "submit_actions";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// パラメータの数値欄をダブルクリックし、値を打ち込んで Enter で確定する。
    SetValue,
    /// シグナルチェーン上のブロックをクリックして選ぶ。選んだ後は撮り直して聞き直す。
    SelectBlock,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiAction {
    #[serde(rename = "type")]
    pub kind: ActionKind,
    #[serde(default)]
    pub block: String,
    #[serde(default)]
    pub parameter: String,
    /// 正規化座標 (0〜1000)。
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub previous_value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiResponse {
    pub explanation: String,
    pub actions: Vec<AiAction>,
    #[serde(default)]
    pub needs_another_round: bool,
    /// 目標に届いたので、これ以上変えなくてよいか (自動ループはこれで止まる)。
    #[serde(default)]
    pub goal_reached: bool,
}

/// 音源ファイルの形式。OpenAI 互換 API の `input_audio.format` にそのまま入る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioFormat {
    Wav,
    Mp3,
}

impl AudioFormat {
    pub fn from_path(path: &str) -> Result<Self, String> {
        let lower = path.to_lowercase();
        if lower.ends_with(".wav") {
            Ok(Self::Wav)
        } else if lower.ends_with(".mp3") {
            Ok(Self::Mp3)
        } else {
            Err(format!(
                "対応していない音源形式です (WAV か MP3 のみ): {path}"
            ))
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatTurn {
    /// "user" か "assistant"
    pub role: String,
    pub text: String,
}

/// Undo で元に戻す値 1 つ分。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreTarget {
    pub block: String,
    pub parameter: String,
    pub value: String,
}

/// 今回のリクエストの目的。
#[derive(Debug, Clone)]
pub enum Purpose<'a> {
    /// ユーザーの指示に沿って音を調整する。
    Adjust { instruction: &'a str },
    /// 直前の変更を元に戻す。
    Revert { targets: &'a [RestoreTarget] },
}

/// 同じサイクル内で既に実行した操作 (ブロック選択後の撮り直しで AI に伝える)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DoneAction {
    pub kind: ActionKind,
    pub block: String,
    pub parameter: String,
    pub value: String,
    pub previous_value: String,
}

pub struct RequestInput<'a> {
    pub purpose: Purpose<'a>,
    pub history: &'a [ChatTurn],
    /// リファレンスから切り出した区間 (WAV)。
    pub reference_audio: Option<&'a [u8]>,
    pub recording_wav: Option<&'a [u8]>,
    pub screenshot_png: &'a [u8],
    pub screenshot_size: (u32, u32),
    /// 何回目の撮影か (1 始まり)。
    pub round: usize,
    pub max_rounds: usize,
    pub done_in_this_cycle: &'a [DoneAction],
    /// 同じサイクルの前の撮影であなたが書いた説明 (2 回目以降は音声を送らないので、方針を伝える)。
    pub earlier_explanations: &'a [String],
    /// このセッションでアプリが打ち込んだ値 (パラメータごとに最後の値)。
    pub changed_parameters: &'a [RestoreTarget],
}

pub const SYSTEM_PROMPT: &str = r#"あなたは Line 6 HX Stomp を HX Edit (macOS) で操作するギター音作りのアシスタントである。
渡されるもの:
- HX Edit のウィンドウ全体のスクリーンショット (タイトルバーを含む)
- HX Stomp から録音した現在のギターの音 (渡された場合)
- 目標にしたいリファレンス音源 (渡された場合)
- ユーザーの指示

やること:
1. 音を聴き比べ、スクリーンショットからシグナルチェーンのブロック構成と、いま選ばれているブロックのパラメータの値を読み取る。
2. 指示 (とリファレンス) に近づけるためのパラメータ変更を決める。
3. 変更をアプリが自動で実行できる操作として返す。

操作の種類:
- "set_value": いま画面に表示されているパラメータの値を変える。x, y はそのパラメータの数値表示 (ノブの下などにある値の文字) の中心を指す。アプリはそこをダブルクリックして value を打ち込み Enter で確定する。
- "select_block": シグナルチェーン上のブロックをクリックして選ぶ。x, y はそのブロックのアイコンの中心を指す。パラメータは value / previous_value を空文字にする。

座標の決まり:
- x, y はスクリーンショットの左上を (0, 0)、右下を (1000, 1000) とする正規化座標で返す。x が横、y が縦である。
- タイトルバー (最上部) やウィンドウの外を指してはいけない。
- 座標に自信が無いパラメータは操作に入れず、explanation で手動での変更を勧める。

値の決まり:
- value と previous_value は単位を付けない数値の文字列で書く (例: "6.5", "-3.0", "40")。
- previous_value にはスクリーンショットから読み取った変更前の値をそのまま書く。Undo に使うので正確に読むこと。
- On/Off の切り替えや、モデルの種類・音符の長さなど数値でない設定は操作に入れず、explanation で手動の操作として説明する。
- 値はパラメータの範囲内にする。1 回の変更は控えめにし、変更は最大 12 個までにする。

ブロックの選び直し:
- 変えたいパラメータが画面に出ていないブロックにあるときは、いま見えているパラメータの set_value を先に並べ、最後に select_block を 1 つだけ置き、needs_another_round を true にする。アプリはそのブロックを選んでから撮り直し、もう一度あなたに聞く。
- それ以外のときは needs_another_round を false にする。
- 選び直した後は撮り直したスクリーンショットだけを渡し、音声は渡さない。select_block を返すときは、選んだ後に変える予定のパラメータと変える向きを explanation に書いておくこと。

関数 (ツール):
- search_effects: この機種のエフェクト一覧を category と query で検索する。別のモデルを勧めるとき、画面のモデルが何を元にしたものか確かめるときに使う。1 回の問い合わせで 3 回まで。
- submit_actions: 答えを返す。最後に必ず 1 回呼ぶ。
- モデルの切り替え (別のアンプやペダルに変えること) はアプリでは実行しない。勧めるときは search_effects で確かめた正確なモデル名を explanation に書き、手で切り替えるよう伝える。

目標への到達:
- 録音がユーザーの指示 (とリファレンス) に十分近く、これ以上変える必要が無いと判断したら goal_reached を true にし、actions を空にする。
- まだ変える余地があるなら goal_reached を false にする。自動ループでは、録音 → あなたへの問い合わせ → 変更 を、goal_reached が true になるまで繰り返す。前回の変更で行き過ぎたと聴こえたら戻してよい。

explanation は日本語で、何をなぜ変えるのか、録音とリファレンスの違いをどう聴いたのかを簡潔に書く。"#;

/// 応答の JSON スキーマ。Gemini の OpenAI 互換エンドポイントで通る範囲に留める。
pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "explanation": { "type": "string" },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "type": { "type": "string", "enum": ["set_value", "select_block"] },
                        "block": { "type": "string" },
                        "parameter": { "type": "string" },
                        "x": { "type": "number" },
                        "y": { "type": "number" },
                        "value": { "type": "string" },
                        "previous_value": { "type": "string" }
                    },
                    "required": ["type", "block", "parameter", "x", "y", "value", "previous_value"]
                }
            },
            "needs_another_round": { "type": "boolean" },
            "goal_reached": { "type": "boolean" }
        },
        "required": ["explanation", "actions", "needs_another_round", "goal_reached"]
    })
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn describe_done(done: &[DoneAction]) -> String {
    done.iter()
        .map(|d| match d.kind {
            ActionKind::SetValue => format!(
                "- {} / {}: {} → {}",
                d.block, d.parameter, d.previous_value, d.value
            ),
            ActionKind::SelectBlock => format!("- ブロック「{}」を選んだ", d.block),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn purpose_text(input: &RequestInput) -> String {
    let (w, h) = input.screenshot_size;
    let mut text = match &input.purpose {
        Purpose::Adjust { instruction } => {
            let mut t = String::new();
            t.push_str("ユーザーの指示: ");
            if instruction.trim().is_empty() {
                t.push_str(
                    "(指示なし。リファレンスに近づけるか、録音を聴いて改善点を提案すること)",
                );
            } else {
                t.push_str(instruction.trim());
            }
            t.push('\n');
            if input.round > 1 {
                t.push_str(
                    "音声は 1 回目の問い合わせで渡した (データ量を抑えるため今回は渡さない)。\
                     前の撮影でのあなたの説明:\n",
                );
                for e in input.earlier_explanations {
                    t.push_str(&format!("- {}\n", truncate(e.trim(), MAX_HISTORY_CHARS)));
                }
            } else {
                match (
                    input.reference_audio.is_some(),
                    input.recording_wav.is_some(),
                ) {
                    (true, true) => t.push_str(
                        "1 つ目の音声がリファレンス (目標の音)、2 つ目の音声が現在の録音である。\n",
                    ),
                    (false, true) => {
                        t.push_str("音声は現在の録音である。リファレンスは無い。\n")
                    }
                    (true, false) => t.push_str(
                        "音声はリファレンスのみである。現在の音は録音されていないので、画面と指示から判断する。\n",
                    ),
                    (false, false) => t.push_str(
                        "音声は無い。画面と指示から判断する。\n",
                    ),
                }
            }
            if !input.changed_parameters.is_empty() {
                t.push_str(
                    "このセッションでアプリが打ち込んだ値 (その後ユーザーが手で変えたかもしれないので、画面の値を優先する):\n",
                );
                for c in input.changed_parameters {
                    t.push_str(&format!("- {} / {}: {}\n", c.block, c.parameter, c.value));
                }
            }
            t
        }
        Purpose::Revert { targets } => {
            let list = targets
                .iter()
                .map(|t| format!("- {} / {} を {} に戻す", t.block, t.parameter, t.value))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "直前の変更を元に戻したい。次のパラメータを指定の値にする set_value を返すこと。\
                 value には下の値をそのまま書き、新しい調整は加えないこと。\n{list}\n"
            )
        }
    };
    text.push_str(&format!(
        "スクリーンショットは {w}×{h} ピクセル。撮影は {} 回目 (最大 {} 回)。\n",
        input.round, input.max_rounds
    ));
    if input.round >= input.max_rounds {
        text.push_str("これが最後の撮影なので select_block は返さず、needs_another_round は false にすること。\n");
    }
    if !input.done_in_this_cycle.is_empty() {
        text.push_str("今回のサイクルで既に実行した操作 (スクリーンショットは実行後のもの):\n");
        text.push_str(&describe_done(input.done_in_this_cycle));
        text.push('\n');
    }
    text
}

/// Chat Completions のリクエスト本文を組み立てる。
pub fn build_request(model: &str, input: &RequestInput) -> Value {
    let mut messages = vec![json!({ "role": "system", "content": SYSTEM_PROMPT })];

    let start = input.history.len().saturating_sub(MAX_HISTORY_TURNS);
    for turn in &input.history[start..] {
        messages.push(json!({
            "role": turn.role,
            "content": truncate(&turn.text, MAX_HISTORY_CHARS)
        }));
    }

    let mut content = vec![json!({ "type": "text", "text": purpose_text(input) })];
    // 音声は 1 回目の撮影でだけ送る (ブロックを選び直した後は画面だけを見ればよい)
    let audio = input.round == 1;
    if let Some(wav) = input.reference_audio.filter(|_| audio) {
        content.push(json!({
            "type": "input_audio",
            "input_audio": { "data": b64(wav), "format": "wav" }
        }));
    }
    if let Some(wav) = input.recording_wav.filter(|_| audio) {
        content.push(json!({
            "type": "input_audio",
            "input_audio": { "data": b64(wav), "format": "wav" }
        }));
    }
    content.push(json!({
        "type": "image_url",
        "image_url": { "url": format!("data:image/png;base64,{}", b64(input.screenshot_png)) }
    }));
    messages.push(json!({ "role": "user", "content": content }));

    json!({
        "model": model,
        "messages": messages,
        "tools": tools(),
        // 答えも関数で返させる (本文の JSON より形が崩れにくく、検索と同じ流れで扱える)
        "tool_choice": "required"
    })
}

/// AI に渡す関数: エフェクト一覧の検索と、答えの提出。
pub fn tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": SEARCH_TOOL,
                "description": "この機種のエフェクト一覧を検索する。モデルを勧めるときや、画面のモデルが何を元にしたものか確かめるときに使う。category と query の少なくとも一方を指定する。結果は最大 15 件で、total が全件数である。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "category": { "type": "string", "enum": catalog::CATEGORIES },
                        "query": {
                            "type": "string",
                            "description": "空白区切りのキーワード (すべてを含むものを返す)。モデル名・元にした機種・系統で引ける。例: \"BB\", \"TS\", \"Klon\", \"Marshall\", \"ファズ\""
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": SUBMIT_TOOL,
                "description": "答え (説明と、アプリが実行する操作) を返す。最後に必ず 1 回呼ぶ。",
                "parameters": response_schema()
            }
        }
    ])
}

/// エフェクト一覧の検索の依頼 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchCall {
    pub id: String,
    pub category: Option<String>,
    pub query: Option<String>,
}

/// AI の返事。答えか、検索の依頼か。
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Final(AiResponse),
    /// `message` は AI の発言そのもので、会話に戻してから検索結果を続ける。
    Searches {
        message: Value,
        calls: Vec<SearchCall>,
    },
}

/// 関数の引数は JSON の文字列で届く (オブジェクトで返す実装もあるので両方受ける)。
fn arguments(call: &Value) -> Result<Value, String> {
    match call.pointer("/function/arguments") {
        Some(Value::String(s)) => serde_json::from_str(strip_code_fence(s)).map_err(|e| {
            format!(
                "AI の関数呼び出しの引数を読めません: {e}: {}",
                truncate(s, 300)
            )
        }),
        Some(v @ Value::Object(_)) => Ok(v.clone()),
        _ => Ok(json!({})),
    }
}

/// Chat Completions の応答本文から、AI の返事 (答えか検索の依頼) を取り出す。
pub fn parse_reply(body: &Value) -> Result<Reply, String> {
    let message = body.pointer("/choices/0/message").cloned();
    let calls = message
        .as_ref()
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
        .filter(|c| !c.is_empty());
    let Some(calls) = calls else {
        // 関数を使わず本文に JSON を書く相手にも対応する
        return parse_response(body).map(Reply::Final);
    };
    let name = |c: &Value| {
        c.pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    if let Some(submit) = calls.iter().find(|c| name(c) == SUBMIT_TOOL) {
        let args = arguments(submit)?;
        return serde_json::from_value(args.clone())
            .map(Reply::Final)
            .map_err(|e| {
                format!(
                    "AI の答えの形が違います: {e}: {}",
                    truncate(&args.to_string(), 500)
                )
            });
    }
    let mut searches = Vec::new();
    for c in calls {
        let n = name(c);
        if n != SEARCH_TOOL {
            return Err(format!("AI が知らない関数を呼びました: {n}"));
        }
        let args = arguments(c)?;
        let text = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        searches.push(SearchCall {
            id: c
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            category: text("category"),
            query: text("query"),
        });
    }
    Ok(Reply::Searches {
        message: message.unwrap_or(Value::Null),
        calls: searches,
    })
}

/// 検索を実行し、AI に返す文字列にする。失敗も AI に伝えて、条件を直させる。
pub fn run_search(device: crate::device::Device, call: &SearchCall) -> String {
    match catalog::search(device, call.category.as_deref(), call.query.as_deref()) {
        Ok(r) => serde_json::to_string(&r).unwrap_or_default(),
        Err(e) => json!({ "error": e }).to_string(),
    }
}

/// モデルの出力がコードフェンスで囲まれていても JSON として読めるようにする。
fn strip_code_fence(s: &str) -> &str {
    let t = s.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    let rest = rest.strip_prefix("json").unwrap_or(rest);
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

/// Chat Completions の応答本文から AI の JSON を取り出す。
pub fn parse_response(body: &Value) -> Result<AiResponse, String> {
    let choice = body.get("choices").and_then(|c| c.get(0)).ok_or_else(|| {
        format!(
            "応答に choices がありません: {}",
            truncate(&body.to_string(), 500)
        )
    })?;
    let content = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            let reason = choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .unwrap_or("不明");
            format!("応答に本文がありません (finish_reason: {reason})")
        })?;
    serde_json::from_str(strip_code_fence(content)).map_err(|e| {
        format!(
            "AI の応答を JSON として読めません: {e}\n応答: {}",
            truncate(content, 500)
        )
    })
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_chars).collect::<String>())
    }
}

/// 数値として打ち込んでよい文字列かを確かめる (単位や記号は受け付けない)。
pub fn is_numeric_value(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty()
        && t.len() <= 12
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
        && t.parse::<f64>().is_ok_and(f64::is_finite)
}

/// AI の応答を実行してよいか、実行前にまとめて確かめる。1 つでも不正なら何も実行しない。
pub fn validate(resp: &AiResponse, purpose: &Purpose, last_round: bool) -> Result<(), String> {
    let set_values: Vec<_> = resp
        .actions
        .iter()
        .filter(|a| a.kind == ActionKind::SetValue)
        .collect();
    if set_values.len() > MAX_SET_VALUE_ACTIONS {
        return Err(format!(
            "AI が {} 個の変更を返しました (上限 {MAX_SET_VALUE_ACTIONS} 個)。実行しません",
            set_values.len()
        ));
    }
    for (i, a) in resp.actions.iter().enumerate() {
        let label = format!("操作 {} ({} / {})", i + 1, a.block, a.parameter);
        if !(0.0..=1000.0).contains(&a.x) || !(0.0..=1000.0).contains(&a.y) {
            return Err(format!(
                "{label} の座標が 0〜1000 の範囲外です: ({}, {})",
                a.x, a.y
            ));
        }
        match a.kind {
            ActionKind::SetValue => {
                if !is_numeric_value(&a.value) {
                    return Err(format!("{label} の値が数値ではありません: {:?}", a.value));
                }
            }
            ActionKind::SelectBlock => {
                if i + 1 != resp.actions.len() {
                    return Err(format!("{label}: select_block は最後に 1 つだけ置けます"));
                }
                if last_round {
                    return Err(format!(
                        "{label}: 撮影回数の上限に達したので、これ以上ブロックを選び直せません"
                    ));
                }
            }
        }
    }
    if let Purpose::Revert { targets } = purpose {
        for a in &set_values {
            let expected = targets.iter().any(|t| t.value.trim() == a.value.trim());
            if !expected {
                return Err(format!(
                    "Undo の応答に、戻す値の一覧に無い値が含まれています ({} / {} = {})。実行しません",
                    a.block, a.parameter, a.value
                ));
            }
        }
    }
    Ok(())
}

/// reqwest のエラーは表示だけでは原因 (タイムアウト・TLS など) が分からないので、原因を連ねて返す。
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(src) = cur {
        out.push_str(&format!(": {src}"));
        cur = src.source();
    }
    if out.contains("timed out") {
        out.push_str(&format!(" ({} 秒で打ち切った)", REQUEST_TIMEOUT.as_secs()));
    }
    out
}

/// 混雑・レート制限など、時間を置けば通る見込みのある HTTP ステータスか。
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// 再試行するまでの待ち時間。要素の数が再試行の回数になる。
pub(crate) const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(3), Duration::from_secs(8)];

/// API を呼び出して答えを返す。AI がエフェクト一覧を検索したら、結果を渡して問い合わせ直す。
/// 混雑 (429 / 5xx) のときだけ待って再試行する。`on_progress` には画面に出す文言を渡す。
pub async fn call(
    settings: &ResolvedSettings,
    body: &Value,
    on_progress: &(dyn Fn(String) + Send + Sync),
) -> Result<AiResponse, String> {
    call_with_delays(settings, body, &RETRY_DELAYS, on_progress).await
}

/// `call` の本体。テストでは待ち時間を 0 にして呼ぶ。
pub(crate) async fn call_with_delays(
    settings: &ResolvedSettings,
    body: &Value,
    retry_delays: &[Duration],
    on_progress: &(dyn Fn(String) + Send + Sync),
) -> Result<AiResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP クライアントを作れません: {e}"))?;
    let mut body = body.clone();
    let mut searches = 0;
    loop {
        let json = post(&client, settings, &body, retry_delays, on_progress).await?;
        let (message, calls) = match parse_reply(&json)? {
            Reply::Final(r) => return Ok(r),
            Reply::Searches { message, calls } => (message, calls),
        };
        if searches >= MAX_SEARCHES {
            return Err(format!(
                "AI がエフェクト一覧の検索を {MAX_SEARCHES} 回より多く続け、答えを返しませんでした"
            ));
        }
        let messages = body
            .get_mut("messages")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "リクエストに messages がありません".to_string())?;
        messages.push(message);
        for c in &calls {
            searches += 1;
            on_progress(format!(
                "エフェクト一覧を検索中 ({})",
                [c.category.as_deref(), c.query.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
            messages.push(json!({
                "role": "tool",
                "tool_call_id": c.id,
                "content": run_search(settings.device, c)
            }));
        }
        if searches >= MAX_SEARCHES {
            // これ以上は検索させず、答えを返させる
            body["tool_choice"] =
                json!({ "type": "function", "function": { "name": SUBMIT_TOOL } });
        }
    }
}

/// 1 回の POST。混雑のときだけ待って再試行する。
async fn post(
    client: &reqwest::Client,
    settings: &ResolvedSettings,
    body: &Value,
    retry_delays: &[Duration],
    on_progress: &(dyn Fn(String) + Send + Sync),
) -> Result<Value, String> {
    let url = format!(
        "{}/chat/completions",
        settings.api_base_url.trim_end_matches('/')
    );
    let mut attempt = 0;
    loop {
        let res = client
            .post(&url)
            .bearer_auth(&settings.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("AI API ({url}) に接続できません: {}", error_chain(&e)))?;
        let status = res.status();
        let text = res
            .text()
            .await
            .map_err(|e| format!("AI API の応答を読めません: {}", error_chain(&e)))?;
        if status.is_success() {
            return serde_json::from_str(&text).map_err(|e| {
                format!(
                    "AI API の応答が JSON ではありません: {e}: {}",
                    truncate(&text, 500)
                )
            });
        }
        let error = format!(
            "AI API がエラーを返しました (HTTP {status}): {}",
            truncate(&text, 800)
        );
        match retry_delays.get(attempt) {
            Some(delay) if is_retryable_status(status.as_u16()) => {
                on_progress(format!(
                    "AI API が混雑しています (HTTP {}). {} 秒後に再試行します ({}/{})",
                    status.as_u16(),
                    delay.as_secs(),
                    attempt + 1,
                    retry_delays.len()
                ));
                tokio::time::sleep(*delay).await;
                attempt += 1;
            }
            _ => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(kind: ActionKind, value: &str) -> AiAction {
        AiAction {
            kind,
            block: "Amp".into(),
            parameter: "Treble".into(),
            x: 500.0,
            y: 800.0,
            value: value.into(),
            previous_value: "5.0".into(),
        }
    }

    fn resp(actions: Vec<AiAction>) -> AiResponse {
        AiResponse {
            explanation: "x".into(),
            actions,
            needs_another_round: false,
            goal_reached: false,
        }
    }

    const ADJUST: Purpose = Purpose::Adjust {
        instruction: "太く",
    };

    /// JSON の値のキーを並べる。
    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    fn strings(v: &Value) -> Vec<String> {
        let mut s: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect();
        s.sort();
        s
    }

    #[test]
    fn response_schema_matches_rust_types() {
        // AI に渡すスキーマと、応答を読む Rust の型がずれていないかを確かめる
        let schema = response_schema();
        let sample = AiResponse {
            explanation: String::new(),
            actions: vec![action(ActionKind::SetValue, "1")],
            needs_another_round: false,
            goal_reached: false,
        };
        let json = serde_json::to_value(&sample).unwrap();
        assert_eq!(keys(&schema["properties"]), keys(&json));
        assert_eq!(strings(&schema["required"]), keys(&json));

        let item = &schema["properties"]["actions"]["items"];
        assert_eq!(keys(&item["properties"]), keys(&json["actions"][0]));
        assert_eq!(strings(&item["required"]), keys(&json["actions"][0]));

        let kinds: Vec<String> = [ActionKind::SetValue, ActionKind::SelectBlock]
            .iter()
            .map(|k| {
                serde_json::to_value(k)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let mut kinds_sorted = kinds.clone();
        kinds_sorted.sort();
        assert_eq!(strings(&item["properties"]["type"]["enum"]), kinds_sorted);
    }

    #[test]
    fn request_offers_search_and_submit_tools_and_requires_one() {
        let body = build_request("m", &input(ADJUST, &[]));
        assert_eq!(body["tool_choice"], "required");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], "search_effects");
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["category"]["enum"],
            json!(catalog::CATEGORIES)
        );
        assert_eq!(tools[1]["function"]["name"], "submit_actions");
        assert_eq!(tools[1]["function"]["parameters"], response_schema());
        assert!(body.get("response_format").is_none());
    }

    fn tool_call(id: &str, name: &str, args: Value) -> Value {
        json!({ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } })
    }

    fn reply(calls: Vec<Value>) -> Value {
        json!({ "choices": [{ "message": { "role": "assistant", "content": null, "tool_calls": calls } }] })
    }

    fn final_args() -> Value {
        json!({ "explanation": "x", "actions": [], "needs_another_round": false, "goal_reached": true })
    }

    #[test]
    fn parse_reply_reads_submitted_answer() {
        let r = parse_reply(&reply(vec![tool_call("1", SUBMIT_TOOL, final_args())])).unwrap();
        let Reply::Final(r) = r else { panic!("{r:?}") };
        assert!(r.goal_reached);
        // 引数がオブジェクトのまま届いても読む
        let obj = json!({ "choices": [{ "message": { "tool_calls": [
            { "id": "1", "function": { "name": SUBMIT_TOOL, "arguments": final_args() } }
        ] } }] });
        assert!(matches!(parse_reply(&obj).unwrap(), Reply::Final(_)));
    }

    #[test]
    fn parse_reply_collects_searches_with_the_original_message() {
        let body = reply(vec![
            tool_call(
                "a",
                SEARCH_TOOL,
                json!({ "category": "drive", "query": "BB" }),
            ),
            tool_call("b", SEARCH_TOOL, json!({ "query": "Klon" })),
        ]);
        let Reply::Searches { message, calls } = parse_reply(&body).unwrap() else {
            panic!()
        };
        assert_eq!(message, body["choices"][0]["message"]);
        assert_eq!(
            calls,
            vec![
                SearchCall {
                    id: "a".into(),
                    category: Some("drive".into()),
                    query: Some("BB".into())
                },
                SearchCall {
                    id: "b".into(),
                    category: None,
                    query: Some("Klon".into())
                },
            ]
        );
    }

    #[test]
    fn submit_wins_over_searches_in_the_same_reply() {
        let body = reply(vec![
            tool_call("a", SEARCH_TOOL, json!({ "query": "BB" })),
            tool_call("b", SUBMIT_TOOL, final_args()),
        ]);
        assert!(matches!(parse_reply(&body).unwrap(), Reply::Final(_)));
    }

    #[test]
    fn parse_reply_rejects_unknown_functions_and_bad_arguments() {
        let unknown = reply(vec![tool_call("a", "delete_everything", json!({}))]);
        assert!(
            parse_reply(&unknown)
                .unwrap_err()
                .contains("delete_everything")
        );
        let bad = json!({ "choices": [{ "message": { "tool_calls": [
            { "id": "1", "function": { "name": SUBMIT_TOOL, "arguments": "{not json" } }
        ] } }] });
        assert!(parse_reply(&bad).unwrap_err().contains("引数を読めません"));
        let wrong_shape = reply(vec![tool_call(
            "1",
            SUBMIT_TOOL,
            json!({ "explanation": "x" }),
        )]);
        assert!(
            parse_reply(&wrong_shape)
                .unwrap_err()
                .contains("形が違います")
        );
    }

    #[test]
    fn parse_reply_falls_back_to_json_content() {
        let content = final_args().to_string();
        let body = json!({ "choices": [{ "message": { "content": content } }] });
        assert!(matches!(parse_reply(&body).unwrap(), Reply::Final(_)));
    }

    #[test]
    fn search_results_and_errors_are_passed_back_as_json() {
        let ok = run_search(
            crate::device::Device::Gp5,
            &SearchCall {
                id: "a".into(),
                category: Some("drive".into()),
                query: Some("TS".into()),
            },
        );
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["total"], 1);
        assert_eq!(v["models"][0]["name"], "Green OD");
        let err = run_search(
            crate::device::Device::Gp5,
            &SearchCall {
                id: "a".into(),
                category: Some("fuzzbox".into()),
                query: None,
            },
        );
        assert!(err.contains("不明なカテゴリ"), "{err}");
    }

    #[test]
    fn system_prompt_states_the_rules_the_validator_enforces() {
        // 検証で弾く規則を AI に伝えていないと、毎回弾かれてしまう
        assert!(SYSTEM_PROMPT.contains("(1000, 1000)"));
        assert!(SYSTEM_PROMPT.contains(&format!("最大 {MAX_SET_VALUE_ACTIONS} 個")));
        assert!(SYSTEM_PROMPT.contains("select_block を 1 つだけ"));
        assert!(SYSTEM_PROMPT.contains("単位を付けない数値"));
    }

    #[test]
    fn mime_types() {
        assert_eq!(AudioFormat::Wav.mime(), "audio/wav");
        assert_eq!(AudioFormat::Mp3.mime(), "audio/mpeg");
    }

    #[test]
    fn truncate_keeps_short_text_and_cuts_long_text_by_chars() {
        assert_eq!(truncate("あいう", 3), "あいう");
        assert_eq!(truncate("あいうえ", 3), "あいう…");
        assert_eq!(truncate("", 0), "");
    }

    #[test]
    fn error_chain_includes_causes_and_timeout_note() {
        #[derive(Debug)]
        struct E(&'static str, Option<Box<E>>);
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(self.0)
            }
        }
        impl std::error::Error for E {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                self.1.as_deref().map(|e| e as _)
            }
        }
        let e = E(
            "送れない",
            Some(Box::new(E("接続", Some(Box::new(E("timed out", None)))))),
        );
        let s = error_chain(&e);
        assert!(s.starts_with("送れない: 接続: timed out"), "{s}");
        assert!(s.contains(&format!("{} 秒で打ち切った", REQUEST_TIMEOUT.as_secs())));
        assert_eq!(error_chain(&E("単独", None)), "単独");
    }

    #[test]
    fn retryable_statuses() {
        for s in [429, 500, 502, 503, 504] {
            assert!(is_retryable_status(s), "{s}");
        }
        for s in [400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(s), "{s}");
        }
    }

    #[test]
    fn audio_format_from_path() {
        assert_eq!(
            AudioFormat::from_path("/a/b.WAV").unwrap(),
            AudioFormat::Wav
        );
        assert_eq!(AudioFormat::from_path("x.mp3").unwrap(), AudioFormat::Mp3);
        assert!(AudioFormat::from_path("x.m4a").is_err());
    }

    #[test]
    fn numeric_values() {
        for ok in ["6.5", "-3.0", "40", "+1", "0"] {
            assert!(is_numeric_value(ok), "{ok}");
        }
        for ng in [
            "",
            "6.5dB",
            "1/8",
            "On",
            "6 5",
            "1e3",
            "--",
            "1234567890123",
        ] {
            assert!(!is_numeric_value(ng), "{ng}");
        }
    }

    #[test]
    fn validate_accepts_normal_response() {
        let r = resp(vec![action(ActionKind::SetValue, "4.5")]);
        assert!(validate(&r, &ADJUST, false).is_ok());
    }

    #[test]
    fn validate_rejects_non_numeric_value() {
        let r = resp(vec![action(ActionKind::SetValue, "4.5dB")]);
        assert!(validate(&r, &ADJUST, false).is_err());
    }

    #[test]
    fn validate_rejects_out_of_range_coordinates() {
        let mut a = action(ActionKind::SetValue, "4.5");
        a.x = 1200.0;
        assert!(validate(&resp(vec![a]), &ADJUST, false).is_err());
    }

    #[test]
    fn validate_accepts_exactly_the_maximum() {
        let r = resp(vec![
            action(ActionKind::SetValue, "1");
            MAX_SET_VALUE_ACTIONS
        ]);
        assert!(validate(&r, &ADJUST, false).is_ok());
    }

    #[test]
    fn validate_rejects_too_many_actions() {
        let r = resp(vec![
            action(ActionKind::SetValue, "1");
            MAX_SET_VALUE_ACTIONS + 1
        ]);
        assert!(validate(&r, &ADJUST, false).is_err());
    }

    #[test]
    fn validate_select_block_must_be_last_and_not_on_last_round() {
        let ok = resp(vec![
            action(ActionKind::SetValue, "1"),
            action(ActionKind::SelectBlock, ""),
        ]);
        assert!(validate(&ok, &ADJUST, false).is_ok());
        assert!(validate(&ok, &ADJUST, true).is_err());
        let ng = resp(vec![
            action(ActionKind::SelectBlock, ""),
            action(ActionKind::SetValue, "1"),
        ]);
        assert!(validate(&ng, &ADJUST, false).is_err());
    }

    #[test]
    fn validate_revert_only_allows_listed_values() {
        let targets = vec![RestoreTarget {
            block: "Amp".into(),
            parameter: "Treble".into(),
            value: "5.0".into(),
        }];
        let purpose = Purpose::Revert { targets: &targets };
        assert!(
            validate(
                &resp(vec![action(ActionKind::SetValue, "5.0")]),
                &purpose,
                false
            )
            .is_ok()
        );
        assert!(
            validate(
                &resp(vec![action(ActionKind::SetValue, "7.0")]),
                &purpose,
                false
            )
            .is_err()
        );
    }

    fn input<'a>(purpose: Purpose<'a>, history: &'a [ChatTurn]) -> RequestInput<'a> {
        RequestInput {
            purpose,
            history,
            reference_audio: Some(b"REF"),
            recording_wav: Some(b"WAV"),
            screenshot_png: b"PNG",
            screenshot_size: (2400, 1600),
            round: 1,
            max_rounds: 4,
            done_in_this_cycle: &[],
            earlier_explanations: &[],
            changed_parameters: &[],
        }
    }

    #[test]
    fn build_request_contains_all_media_in_order() {
        let body = build_request("gemini-3.8-flash", &input(ADJUST, &[]));
        assert_eq!(body["model"], "gemini-3.8-flash");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        let content = messages.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert!(content[0]["text"].as_str().unwrap().contains("太く"));
        assert!(content[0]["text"].as_str().unwrap().contains("2400×1600"));
        assert_eq!(content[1]["input_audio"]["format"], "wav");
        // 期待値は b64 で作らず固定値で書く (b64 が壊れても気づけるように)
        assert_eq!(content[1]["input_audio"]["data"], "UkVG");
        assert_eq!(content[2]["input_audio"]["data"], "V0FW");
        assert_eq!(content[2]["input_audio"]["format"], "wav");
        assert_eq!(content[3]["image_url"]["url"], "data:image/png;base64,UE5H");
    }

    #[test]
    fn build_request_keeps_only_recent_history() {
        let history: Vec<ChatTurn> = (0..20)
            .map(|i| ChatTurn {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                text: format!("turn {i}"),
            })
            .collect();
        let body = build_request("m", &input(ADJUST, &history));
        let messages = body["messages"].as_array().unwrap();
        // system + 履歴 + 今回
        assert_eq!(messages.len(), 1 + MAX_HISTORY_TURNS + 1);
        assert_eq!(messages[1]["content"], "turn 12");
    }

    #[test]
    fn build_request_for_revert_lists_targets() {
        let targets = vec![RestoreTarget {
            block: "Amp".into(),
            parameter: "Bass".into(),
            value: "5.0".into(),
        }];
        let mut i = input(Purpose::Revert { targets: &targets }, &[]);
        i.recording_wav = None;
        i.reference_audio = None;
        let body = build_request("m", &i);
        let content = body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .clone();
        assert!(
            content[0]["text"]
                .as_str()
                .unwrap()
                .contains("Amp / Bass を 5.0 に戻す")
        );
        assert_eq!(content.len(), 2, "テキストと画像だけ");
    }

    #[test]
    fn parse_response_reads_json_content() {
        let content = r#"{"explanation":"高域を下げた","actions":[{"type":"set_value","block":"Amp","parameter":"Treble","x":512,"y":830,"value":"4.0","previous_value":"5.5"}],"needs_another_round":false}"#;
        let body = json!({ "choices": [{ "message": { "content": content } }] });
        let r = parse_response(&body).unwrap();
        assert_eq!(r.actions[0].kind, ActionKind::SetValue);
        assert_eq!(r.actions[0].previous_value, "5.5");
    }

    #[test]
    fn parse_response_strips_code_fence() {
        let content =
            "```json\n{\"explanation\":\"x\",\"actions\":[],\"needs_another_round\":false}\n```";
        let body = json!({ "choices": [{ "message": { "content": content } }] });
        assert!(parse_response(&body).unwrap().actions.is_empty());
    }

    #[test]
    fn parse_response_reports_missing_content() {
        let body = json!({ "choices": [{ "message": {}, "finish_reason": "length" }] });
        assert!(parse_response(&body).unwrap_err().contains("length"));
        assert!(parse_response(&json!({})).is_err());
    }

    fn last_content(body: &Value) -> Vec<Value> {
        body["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[test]
    fn audio_is_sent_only_on_the_first_round() {
        let explanations = vec!["Delay を選んでから Mix を上げる".to_string()];
        let mut i = input(ADJUST, &[]);
        i.round = 2;
        i.earlier_explanations = &explanations;
        let content = last_content(&build_request("m", &i));
        let types: Vec<&str> = content
            .iter()
            .map(|c| c["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, vec!["text", "image_url"], "2 回目は音声を送らない");
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("1 回目の問い合わせで渡した"), "{text}");
        assert!(text.contains("- Delay を選んでから Mix を上げる"), "{text}");
        assert!(!text.contains("2 つ目の音声"), "{text}");
    }

    #[test]
    fn history_turns_are_truncated() {
        let history = vec![ChatTurn {
            role: "assistant".into(),
            text: "あ".repeat(MAX_HISTORY_CHARS * 3),
        }];
        let body = build_request("m", &input(ADJUST, &history));
        let sent = body["messages"][1]["content"].as_str().unwrap();
        assert_eq!(
            sent.chars().count(),
            MAX_HISTORY_CHARS + 1,
            "上限 + 省略記号"
        );
        assert!(sent.ends_with('…'));
        assert_eq!(MAX_HISTORY_CHARS, 400);
    }

    #[test]
    fn changed_parameters_are_listed_for_adjust() {
        let changed = vec![RestoreTarget {
            block: "Amp".into(),
            parameter: "Treble".into(),
            value: "6.0".into(),
        }];
        let mut i = input(ADJUST, &[]);
        i.changed_parameters = &changed;
        let text = last_content(&build_request("m", &i))[0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("アプリが打ち込んだ値"), "{text}");
        assert!(text.contains("- Amp / Treble: 6.0"), "{text}");
        let plain = last_content(&build_request("m", &input(ADJUST, &[])))[0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!plain.contains("アプリが打ち込んだ値"));
    }

    #[test]
    fn audio_description_matches_what_is_sent() {
        let text = |reference: bool, recording: bool| {
            let mut i = input(ADJUST, &[]);
            if !reference {
                i.reference_audio = None;
            }
            if !recording {
                i.recording_wav = None;
            }
            last_content(&build_request("m", &i))[0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert!(text(true, true).contains("1 つ目の音声がリファレンス"));
        assert!(text(false, true).contains("リファレンスは無い"));
        assert!(text(true, false).contains("現在の音は録音されていない"));
        assert!(text(false, false).contains("音声は無い"));
    }

    #[test]
    fn system_prompt_explains_the_tools_and_their_limit() {
        assert!(SYSTEM_PROMPT.contains(SEARCH_TOOL));
        assert!(SYSTEM_PROMPT.contains(SUBMIT_TOOL));
        assert!(SYSTEM_PROMPT.contains(&format!("{MAX_SEARCHES} 回まで")));
        assert!(SYSTEM_PROMPT.contains("モデルの切り替え"));
    }

    #[test]
    fn system_prompt_explains_goal_reached() {
        assert!(SYSTEM_PROMPT.contains("goal_reached を true にし、actions を空にする"));
    }

    #[test]
    fn system_prompt_says_audio_is_not_resent_after_selecting_a_block() {
        assert!(SYSTEM_PROMPT.contains("音声は渡さない"));
        assert!(SYSTEM_PROMPT.contains("変える予定のパラメータ"));
    }
}
