//! OpenAI 互換の Chat Completions API で AI に音と現在のチェーン情報を渡し、変更を JSON で受け取る。
//!
//! 既定の接続先は Gemini の OpenAI 互換エンドポイントである。音声は `input_audio`、
//! 現在のチェーン状態はテキスト、出力はツール呼び出し (`submit_changes`) で受け取る。

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog;
use crate::config::ResolvedSettings;

/// 1 回の応答で受け付ける変更の上限。これを超える応答は丸ごと捨てる。
pub const MAX_CHANGES: usize = 12;

/// 会話の文脈として送る過去のやりとりの数。
pub const MAX_HISTORY_TURNS: usize = 8;

/// 過去のやりとり 1 つあたりに送る文字数の上限。変えた値は別に一覧で送るので、説明は短くてよい。
pub const MAX_HISTORY_CHARS: usize = 400;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// 1 回の問い合わせで、エフェクト一覧を検索してよい回数。超えたら答えを返させる。
pub const MAX_SEARCHES: usize = 3;
/// エフェクト一覧を検索する関数の名前。
pub const SEARCH_TOOL: &str = "search_effects";
/// 答え (変更の一覧) を返す関数の名前。
pub const SUBMIT_TOOL: &str = "submit_changes";

/// AI が提案する 1 件のパラメータ変更。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiChange {
    pub block: String,
    pub parameter: String,
    pub value: String,
}

/// ブロック操作の種類。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockOpAction {
    Enable,
    Disable,
    Add,
    Remove,
    Replace,
}

/// AI が提案する 1 件のブロック操作 (ON/OFF・追加・削除・モデル差し替え)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiBlockOp {
    pub action: BlockOpAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stereo: Option<bool>,
    #[serde(default)]
    pub amp_cab: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiResponse {
    pub explanation: String,
    pub changes: Vec<AiChange>,
    #[serde(default)]
    pub block_operations: Vec<AiBlockOp>,
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
    M4a,
}

impl AudioFormat {
    pub fn from_path(path: &str) -> Result<Self, String> {
        let lower = path.to_lowercase();
        if lower.ends_with(".wav") {
            Ok(Self::Wav)
        } else if lower.ends_with(".mp3") {
            Ok(Self::Mp3)
        } else if lower.ends_with(".m4a") {
            Ok(Self::M4a)
        } else {
            Err(format!(
                "対応していない音源形式です (WAV / MP3 / M4A のみ): {path}"
            ))
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
        }
    }

    pub fn mime(self) -> &'static str {
        match self {
            Self::Wav => "audio/wav",
            Self::Mp3 => "audio/mpeg",
            Self::M4a => "audio/mp4",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatTurn {
    /// "user" か "assistant"
    pub role: String,
    pub text: String,
}

/// Undo で元に戻せなかった項目の表示や、変更済み一覧に使う情報。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreTarget {
    pub block: String,
    pub parameter: String,
    pub value: String,
}

/// 実際に機器へ適用した変更 1 件 (画面表示用)。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DoneAction {
    pub block: String,
    pub parameter: String,
    pub value: String,
    pub previous_value: String,
}

pub struct RequestInput<'a> {
    pub instruction: &'a str,
    pub history: &'a [ChatTurn],
    /// リファレンスから切り出した区間 (WAV)。
    pub reference_audio: Option<&'a [u8]>,
    pub recording_wav: Option<&'a [u8]>,
    /// USB 経由で読み取った現在のチェーン状態のテキスト。
    pub chain_text: &'a str,
    /// このセッションでアプリが変えた値 (パラメータごとに最後の値)。
    pub changed_parameters: &'a [RestoreTarget],
}

pub const SYSTEM_PROMPT: &str = r#"あなたは Line 6 HX Stomp / HX Stomp XL を USB 接続で操作するギター音作りのアシスタントである。
渡されるもの:
- 現在のプリセットのシグナルチェーン・空きブロックスロット数・各ブロックのパラメータ一覧 (現在値・範囲・選択肢)
- HX Stomp から録音した現在のギターの音 (渡された場合)
- 目標にしたいリファレンス音源 (渡された場合)
- ユーザーの指示

やること:
1. 音を聴き比べ、渡されたブロックとパラメータ一覧から現在の設定を読む。
2. 指示 (とリファレンス) に近づけるためのパラメータ変更やブロック操作 (On/Off・追加・削除・モデル差し替え) を決める。
3. 変更をアプリが自動で実行できる一覧として返す。

チェーン一覧の見方:
- 先頭付近に `空きブロックスロット: N 枠` と書かれる。0 枠のときは削除しない限りブロックを追加できない。
- 各ブロックは `[block2] Tone Sovereign [ON]` のように `[blockN]` の識別子と名前・On/Off 状態で書かれる。
- `[割り当て済み: 削除・差し替え不可]` が付いたブロック (フットスイッチやコントローラー割り当て等) は削除・モデル差し替えをしてはいけない (On/Off や固定されていないパラメータの変更はできる)。
- その下に `- Gain 1: 2.2 (0.0〜10.0)` のように、パラメータ名・現在値・許容範囲や `[Overdrive / Boost / Distortion]` のような選択肢が並ぶ。
- `[固定: 変更不可]` や `(変更不可)` と書かれた項目は変更してはいけない。
- `(参照のみ)` と書かれた項目も自動では変えられない。

変更の決まり:
- パラメータ変更は `changes` に書く。各要素に `block` (例: "block2")、`parameter` (一覧にあるパラメータ名そのまま)、`value` (新しい値) を入れる。
- ブロック操作は `block_operations` に書く (`action`: "enable" / "disable" / "add" / "remove" / "replace")。
  - `enable` / `disable`: `block` のみ必須 (そのブロックの On/Off を切り替える)。
  - `remove`: `block` のみ必須 (固定されていないブロックを削除する)。
  - `add`: `model` に search_effects で確認した正確なモデル名を入れ、必要なら `position` ("end" / "before:block5" / "after:block2"、省略時は "end")・`stereo` (Stereo 版なら true)・`amp_cab` (Amp+Cab 1 ブロックにするなら true) を指定する。
  - `replace`: `block` と `model` (と必要なら `stereo` / `amp_cab`) を指定し、モデルを差し替える。
- `add` または `replace` を返したときは、アプリが実機へ反映した直後に新しいパラメータ一覧付きのチェーンを再送する。新しく追加・差し替えされたブロックの初期パラメータ設定は次の返答で行うため、同じ返答内の `changes` にまだ存在しないパラメータを書かないこと。
- 数値の `value` は一覧の範囲内で書く (単位は省いても、表示されている単位を付けてもよい。例: "6.5", "-3.0")。
- 選択肢の `value` は `[...]` の中に列挙された文字列から正確に選ぶ。
- 1 回の変更 (`changes` と `block_operations` の合計) は控えめにし、合計最大 12 個までにする。同じブロック・パラメータを 1 回の中に重複させない。
- 変更は一時編集バッファにだけ反映される (保存は人間が本体で行う)。

関数 (ツール):
- search_effects: HX Stomp のエフェクト一覧を category と query で検索する。ブロック追加・差し替え時に正確なモデル名を確かめるときや、現在のモデルが何を元にしたものか確かめるときに使う。1 回の問い合わせで 3 回まで。
- submit_changes: 答え (説明と変更一覧・ブロック操作一覧) を返す。最後に必ず 1 回呼ぶ。

目標への到達:
- 録音がユーザーの指示 (とリファレンス) に十分近く、これ以上変える必要が無いと判断したら goal_reached を true にし、changes を空にする (block_operations も空にする)。
- まだ変える余地があるなら goal_reached を false にする。自動ループでは、録音 → あなたへの問い合わせ → 変更 を、goal_reached が true になるまで繰り返す。前回の変更で行き過ぎたと聴こえたら戻してよい。

explanation は日本語で、何をなぜ変えるのか、録音とリファレンスの違いをどう聴いたのかを簡潔に書く。"#;

/// 応答の JSON スキーマ。Gemini の OpenAI 互換エンドポイントで通る範囲に留める。
pub fn response_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "explanation": { "type": "string" },
            "changes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "block": { "type": "string" },
                        "parameter": { "type": "string" },
                        "value": { "type": "string" }
                    },
                    "required": ["block", "parameter", "value"]
                }
            },
            "block_operations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["enable", "disable", "add", "remove", "replace"]
                        },
                        "block": { "type": "string" },
                        "model": { "type": "string" },
                        "position": { "type": "string" },
                        "stereo": { "type": "boolean" },
                        "amp_cab": { "type": "boolean" }
                    },
                    "required": ["action"]
                }
            },
            "goal_reached": { "type": "boolean" }
        },
        "required": ["explanation", "changes", "goal_reached"]
    })
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn prompt_text(input: &RequestInput) -> String {
    let mut t = String::new();
    t.push_str("ユーザーの指示: ");
    if input.instruction.trim().is_empty() {
        t.push_str("(指示なし。リファレンスに近づけるか、録音を聴いて改善点を提案すること)");
    } else {
        t.push_str(input.instruction.trim());
    }
    t.push('\n');

    match (
        input.reference_audio.is_some(),
        input.recording_wav.is_some(),
    ) {
        (true, true) => {
            t.push_str("1 つ目の音声がリファレンス (目標の音)、2 つ目の音声が現在の録音である。\n")
        }
        (false, true) => t.push_str("音声は現在の録音である。リファレンスは無い。\n"),
        (true, false) => t.push_str(
            "音声はリファレンスのみである。現在の音は録音されていないので、チェーンと指示から判断する。\n",
        ),
        (false, false) => t.push_str("音声は無い。現在のチェーンと指示から判断する。\n"),
    }

    if !input.changed_parameters.is_empty() {
        t.push_str(
            "このセッションでアプリが変えた値 (その後ユーザーが本体で変えたかもしれないので、下の現在値を優先する):\n",
        );
        for c in input.changed_parameters {
            t.push_str(&format!("- {} / {}: {}\n", c.block, c.parameter, c.value));
        }
    }

    t.push_str("\n現在のシグナルチェーンとパラメータ:\n");
    t.push_str(input.chain_text);
    if !input.chain_text.ends_with('\n') {
        t.push('\n');
    }
    t
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

    let mut content = vec![json!({ "type": "text", "text": prompt_text(input) })];
    if let Some(wav) = input.reference_audio {
        content.push(json!({
            "type": "input_audio",
            "input_audio": { "data": b64(wav), "format": "wav" }
        }));
    }
    if let Some(wav) = input.recording_wav {
        content.push(json!({
            "type": "input_audio",
            "input_audio": { "data": b64(wav), "format": "wav" }
        }));
    }
    messages.push(json!({ "role": "user", "content": content }));

    json!({
        "model": model,
        "messages": messages,
        "tools": tools(),
        // 答えも関数で返させる (本文の JSON より形が崩れにくく、検索と同じ流れで扱える)
        "tool_choice": "required"
    })
}

/// AI に渡す関数: エフェクト一覧の検索と、変更の提出。
pub fn tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": SEARCH_TOOL,
                "description": "HX Stomp のエフェクト一覧を検索する。モデルを勧めるときや、現在のモデルが何を元にしたものか確かめるときに使う。category と query の少なくとも一方を指定する。結果は最大 15 件で、total が全件数である。",
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
                "description": "答え (説明と、変更するパラメータの一覧) を返す。最後に必ず 1 回呼ぶ。",
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
pub fn run_search(call: &SearchCall) -> String {
    match catalog::search(call.category.as_deref(), call.query.as_deref()) {
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

fn parse_position(raw: Option<&str>) -> Result<crate::hx_device::BlockPosition, String> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(crate::hx_device::BlockPosition::End);
    };
    if s.eq_ignore_ascii_case("end") {
        return Ok(crate::hx_device::BlockPosition::End);
    }
    if let Some(target) = s.strip_prefix("before:") {
        let t = target.trim();
        if t.is_empty() {
            return Err("追加位置 'before:' のブロック指定が空です".into());
        }
        return Ok(crate::hx_device::BlockPosition::Before(t.to_string()));
    }
    if let Some(target) = s.strip_prefix("after:") {
        let t = target.trim();
        if t.is_empty() {
            return Err("追加位置 'after:' のブロック指定が空です".into());
        }
        return Ok(crate::hx_device::BlockPosition::After(t.to_string()));
    }
    Err(format!(
        "追加位置 '{s}' を解釈できません (\"end\", \"before:blockN\", \"after:blockN\" のいずれかを指定してください)"
    ))
}

fn require_non_empty(opt: Option<&String>, index: usize, field: &str) -> Result<String, String> {
    let val = opt.map(|s| s.trim()).unwrap_or("");
    if val.is_empty() {
        return Err(format!(
            "ブロック操作 {} に {field} がありません",
            index + 1
        ));
    }
    Ok(val.to_string())
}

/// AI の `block_operations` を検証付きで実機層 (`hx_device::BlockOp`) へ変換する。
pub fn to_device_block_ops(ops: &[AiBlockOp]) -> Result<Vec<crate::hx_device::BlockOp>, String> {
    use crate::hx_device::BlockOp;
    let mut out = Vec::with_capacity(ops.len());
    for (i, op) in ops.iter().enumerate() {
        let converted = match op.action {
            BlockOpAction::Enable => BlockOp::SetEnabled {
                block: require_non_empty(op.block.as_ref(), i, "block")?,
                enabled: true,
            },
            BlockOpAction::Disable => BlockOp::SetEnabled {
                block: require_non_empty(op.block.as_ref(), i, "block")?,
                enabled: false,
            },
            BlockOpAction::Remove => BlockOp::Remove {
                block: require_non_empty(op.block.as_ref(), i, "block")?,
            },
            BlockOpAction::Add => BlockOp::Add {
                model: require_non_empty(op.model.as_ref(), i, "model")?,
                position: parse_position(op.position.as_deref())?,
                stereo: op.stereo,
                amp_cab: op.amp_cab,
            },
            BlockOpAction::Replace => BlockOp::Replace {
                block: require_non_empty(op.block.as_ref(), i, "block")?,
                model: require_non_empty(op.model.as_ref(), i, "model")?,
                stereo: op.stereo,
                amp_cab: op.amp_cab,
            },
        };
        out.push(converted);
    }
    Ok(out)
}

/// AI の応答の基本形を確かめる (件数上限・空欄)。パラメータ実在と範囲は `hx_device` で検証する。
pub fn validate(resp: &AiResponse) -> Result<(), String> {
    let total = resp.changes.len() + resp.block_operations.len();
    if total > MAX_CHANGES {
        return Err(format!(
            "AI が {total} 個の変更を返しました (上限 {MAX_CHANGES} 個)。実行しません"
        ));
    }
    for (i, c) in resp.changes.iter().enumerate() {
        if c.block.trim().is_empty() || c.parameter.trim().is_empty() || c.value.trim().is_empty() {
            return Err(format!(
                "変更 {} に空の項目があります (block={:?}, parameter={:?}, value={:?})",
                i + 1,
                c.block,
                c.parameter,
                c.value
            ));
        }
    }
    to_device_block_ops(&resp.block_operations)?;
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
                "content": run_search(c)
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

    fn change(block: &str, param: &str, value: &str) -> AiChange {
        AiChange {
            block: block.into(),
            parameter: param.into(),
            value: value.into(),
        }
    }

    fn resp(changes: Vec<AiChange>) -> AiResponse {
        AiResponse {
            explanation: "x".into(),
            changes,
            block_operations: vec![],
            goal_reached: false,
        }
    }

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
        let schema = response_schema();
        let sample = AiResponse {
            explanation: String::new(),
            changes: vec![change("block2", "Gain 1", "3.0")],
            block_operations: vec![AiBlockOp {
                action: BlockOpAction::Add,
                block: Some("block2".into()),
                model: Some("Dynamic Hall".into()),
                position: Some("end".into()),
                stereo: Some(false),
                amp_cab: false,
            }],
            goal_reached: false,
        };
        let json = serde_json::to_value(&sample).unwrap();
        assert_eq!(keys(&schema["properties"]), keys(&json));
        assert_eq!(
            strings(&schema["required"]),
            vec!["changes", "explanation", "goal_reached"]
        );

        let item = &schema["properties"]["changes"]["items"];
        assert_eq!(keys(&item["properties"]), keys(&json["changes"][0]));
        assert_eq!(strings(&item["required"]), keys(&json["changes"][0]));

        let op_item = &schema["properties"]["block_operations"]["items"];
        assert_eq!(
            keys(&op_item["properties"]),
            keys(&json["block_operations"][0])
        );
        assert_eq!(strings(&op_item["required"]), vec!["action"]);
    }

    #[test]
    fn validate_and_convert_block_operations() {
        let ok = AiResponse {
            explanation: "x".into(),
            changes: vec![],
            block_operations: vec![
                AiBlockOp {
                    action: BlockOpAction::Add,
                    block: None,
                    model: Some("Dynamic Hall".into()),
                    position: Some("after:block2".into()),
                    stereo: Some(true),
                    amp_cab: false,
                },
                AiBlockOp {
                    action: BlockOpAction::Enable,
                    block: Some("block5".into()),
                    model: None,
                    position: None,
                    stereo: None,
                    amp_cab: false,
                },
            ],
            goal_reached: false,
        };
        validate(&ok).unwrap();
        let ops = to_device_block_ops(&ok.block_operations).unwrap();
        assert_eq!(ops.len(), 2);

        let bad_pos = AiResponse {
            explanation: "x".into(),
            changes: vec![],
            block_operations: vec![AiBlockOp {
                action: BlockOpAction::Add,
                block: None,
                model: Some("Dynamic Hall".into()),
                position: Some("middle".into()),
                stereo: None,
                amp_cab: false,
            }],
            goal_reached: false,
        };
        assert!(validate(&bad_pos).unwrap_err().contains("追加位置"));

        let missing_block = AiResponse {
            explanation: "x".into(),
            changes: vec![],
            block_operations: vec![AiBlockOp {
                action: BlockOpAction::Remove,
                block: None,
                model: None,
                position: None,
                stereo: None,
                amp_cab: false,
            }],
            goal_reached: false,
        };
        assert!(validate(&missing_block).unwrap_err().contains("block"));
    }

    #[test]
    fn request_offers_search_and_submit_tools_and_requires_one() {
        let body = build_request("m", &input("太く", &[]));
        assert_eq!(body["tool_choice"], "required");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], SEARCH_TOOL);
        assert_eq!(
            tools[0]["function"]["parameters"]["properties"]["category"]["enum"],
            json!(catalog::CATEGORIES)
        );
        assert_eq!(tools[1]["function"]["name"], SUBMIT_TOOL);
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
        json!({ "explanation": "x", "changes": [], "goal_reached": true })
    }

    #[test]
    fn parse_reply_reads_submitted_answer() {
        let r = parse_reply(&reply(vec![tool_call("1", SUBMIT_TOOL, final_args())])).unwrap();
        let Reply::Final(r) = r else { panic!("{r:?}") };
        assert!(r.goal_reached);
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
        let ok = run_search(&SearchCall {
            id: "a".into(),
            category: Some("drive".into()),
            query: Some("TS".into()),
        });
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["total"], 2);
        assert_eq!(v["models"][0]["name"], "Scream 808");
        let err = run_search(&SearchCall {
            id: "a".into(),
            category: Some("fuzzbox".into()),
            query: None,
        });
        assert!(err.contains("不明なカテゴリ"), "{err}");
    }

    #[test]
    fn system_prompt_states_the_rules_and_tools() {
        assert!(SYSTEM_PROMPT.contains(&format!("合計最大 {MAX_CHANGES} 個")));
        assert!(SYSTEM_PROMPT.contains(SEARCH_TOOL));
        assert!(SYSTEM_PROMPT.contains(SUBMIT_TOOL));
        assert!(SYSTEM_PROMPT.contains(&format!("{MAX_SEARCHES} 回まで")));
        assert!(SYSTEM_PROMPT.contains("goal_reached を true にし、changes を空にする"));
        assert!(SYSTEM_PROMPT.contains("[固定: 変更不可]"));
        assert!(SYSTEM_PROMPT.contains("[割り当て済み: 削除・差し替え不可]"));
        assert!(SYSTEM_PROMPT.contains("保存は人間"));
    }

    #[test]
    fn mime_types() {
        assert_eq!(AudioFormat::Wav.mime(), "audio/wav");
        assert_eq!(AudioFormat::Mp3.mime(), "audio/mpeg");
        assert_eq!(AudioFormat::M4a.mime(), "audio/mp4");
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
        assert_eq!(AudioFormat::from_path("x.M4A").unwrap(), AudioFormat::M4a);
        assert!(AudioFormat::from_path("x.ogg").is_err());
        assert_eq!(AudioFormat::Wav.as_str(), "wav");
        assert_eq!(AudioFormat::Mp3.as_str(), "mp3");
        assert_eq!(AudioFormat::M4a.as_str(), "m4a");
    }

    #[test]
    fn validate_accepts_normal_and_maximum_changes() {
        let ok = resp(vec![change("block2", "Gain 1", "4.5")]);
        assert!(validate(&ok).is_ok());

        let max = resp(vec![change("block2", "Gain 1", "1"); MAX_CHANGES]);
        assert!(validate(&max).is_ok());
    }

    #[test]
    fn validate_rejects_empty_fields_and_too_many_changes() {
        let too_many = resp(vec![change("block2", "Gain 1", "1"); MAX_CHANGES + 1]);
        assert!(validate(&too_many).is_err());

        for bad in [
            change("", "Gain 1", "1.0"),
            change("block2", " ", "1.0"),
            change("block2", "Gain 1", ""),
        ] {
            assert!(validate(&resp(vec![bad])).is_err());
        }
    }

    fn input<'a>(instruction: &'a str, history: &'a [ChatTurn]) -> RequestInput<'a> {
        RequestInput {
            instruction,
            history,
            reference_audio: Some(b"REF"),
            recording_wav: Some(b"WAV"),
            chain_text: "[block2] Tone Sovereign [ON]\n  - Gain 1: 2.2 (0.0〜10.0)\n",
            changed_parameters: &[],
        }
    }

    #[test]
    fn build_request_contains_text_and_audio_without_images() {
        let body = build_request("gemini-3.8-flash", &input("太く", &[]));
        assert_eq!(body["model"], "gemini-3.8-flash");
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        let content = messages.last().unwrap()["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "text");
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("太く"));
        assert!(text.contains("[block2] Tone Sovereign"));
        assert!(text.ends_with("Gain 1: 2.2 (0.0〜10.0)\n"));
        assert!(!text.ends_with("\n\n"));

        let mut no_nl = input("太く", &[]);
        no_nl.chain_text = "[block2] Tone Sovereign";
        let text2 = build_request("gemini-3.8-flash", &no_nl)["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text2.ends_with("[block2] Tone Sovereign\n"));
        assert_eq!(content[1]["input_audio"]["format"], "wav");
        assert_eq!(content[1]["input_audio"]["data"], "UkVG");
        assert_eq!(content[2]["input_audio"]["data"], "V0FW");
        assert_eq!(content[2]["input_audio"]["format"], "wav");
    }

    #[test]
    fn build_request_keeps_only_recent_history() {
        let history: Vec<ChatTurn> = (0..20)
            .map(|i| ChatTurn {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                text: format!("turn {i}"),
            })
            .collect();
        let body = build_request("m", &input("太く", &history));
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1 + MAX_HISTORY_TURNS + 1);
        assert_eq!(messages[1]["content"], "turn 12");
    }

    #[test]
    fn parse_response_reads_json_content_and_strips_fence() {
        let content = r#"{"explanation":"高域を下げた","changes":[{"block":"block5","parameter":"Treble","value":"4.0"}],"goal_reached":false}"#;
        let body = json!({ "choices": [{ "message": { "content": content } }] });
        let r = parse_response(&body).unwrap();
        assert_eq!(r.changes[0].block, "block5");
        assert_eq!(r.changes[0].value, "4.0");

        let fenced = "```json\n{\"explanation\":\"x\",\"changes\":[],\"goal_reached\":false}\n```";
        let body2 = json!({ "choices": [{ "message": { "content": fenced } }] });
        assert!(parse_response(&body2).unwrap().changes.is_empty());
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
    fn history_turns_are_truncated() {
        let history = vec![ChatTurn {
            role: "assistant".into(),
            text: "あ".repeat(MAX_HISTORY_CHARS * 3),
        }];
        let body = build_request("m", &input("太く", &history));
        let sent = body["messages"][1]["content"].as_str().unwrap();
        assert_eq!(sent.chars().count(), MAX_HISTORY_CHARS + 1);
        assert!(sent.ends_with('…'));
        assert_eq!(MAX_HISTORY_CHARS, 400);
    }

    #[test]
    fn changed_parameters_are_listed_in_prompt() {
        let changed = vec![RestoreTarget {
            block: "block5".into(),
            parameter: "Treble".into(),
            value: "6.0".into(),
        }];
        let mut i = input("太く", &[]);
        i.changed_parameters = &changed;
        let text = last_content(&build_request("m", &i))[0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains("アプリが変えた値"), "{text}");
        assert!(text.contains("- block5 / Treble: 6.0"), "{text}");
        let plain = last_content(&build_request("m", &input("太く", &[])))[0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!plain.contains("アプリが変えた値"));
    }

    #[test]
    fn audio_description_matches_what_is_sent() {
        let text = |reference: bool, recording: bool| {
            let mut i = input("太く", &[]);
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
}
