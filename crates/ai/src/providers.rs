//! Provider wire formats: request bodies, auth headers and response parsing.
//!
//! - Anthropic Messages API (`anthropic-version: 2026-10-21`), structured
//!   output via `output_config.format`.
//! - OpenAI Responses API with `text.format` JSON schema; `store: false` so
//!   article text is not retained as a stored response.
//! - Gemini `generateContent` with `responseMimeType` + `responseJsonSchema`.
//! - OpenAI on a ChatGPT plan: the same Responses API, but streaming is
//!   required, `max_output_tokens` is rejected and system text goes in
//!   `instructions`. The stream is read to its terminal event, whose
//!   `response` object is parsed like a normal reply.
//! - A local or self-hosted server (llama.cpp `llama-server`, Ollama, vLLM,
//!   LM Studio): OpenAI chat completions at `{base}/chat/completions` with
//!   `response_format: json_schema`. Without schema support the schema is
//!   put in the system prompt instead ([`local_body`] with `schema: false`).
//!   Reasoning models' `<think>` blocks are dropped before parsing.

use juttusihti_core::pricing::{Provider, Usage};
use serde_json::{Value, json};

use crate::{AiError, Auth, Endpoint, Outcome, Reply, Request};

pub const ANTHROPIC_VERSION: &str = "2026-10-21";

pub fn auth_headers(provider: Provider, token: &str) -> Vec<(&'static str, String)> {
    match provider {
        Provider::Anthropic => vec![("x-api-key", token.into()), ("anthropic-version", ANTHROPIC_VERSION.into())],
        Provider::Openai | Provider::Local => vec![("authorization", format!("Bearer {token}"))],
        Provider::Gemini => vec![("x-goog-api-key", token.into())],
    }
}

fn base(e: &Endpoint) -> &str {
    e.base_url.trim_end_matches('/')
}

fn anthropic_body(req: &Request) -> Value {
    json!({
        "model": req.model,
        "system": [{"type": "text", "text": req.system, "cache_control": {"type": "ephemeral"}}],
        "messages": [{"role": "user", "content": req.user}],
        "output_config": {"format": {"type": "json_schema", "schema": req.schema}},
    })
}

fn openai_input(req: &Request) -> Value {
    json!([
        {"role": "system", "content": req.system},
        {"role": "user", "content": req.user},
    ])
}

fn openai_format(req: &Request) -> Value {
    json!({"format": {"type": "json_schema", "name": req.schema_name, "schema": req.schema, "strict": true}})
}

fn gemini_request(req: &Request) -> Value {
    json!({
        "systemInstruction": {"parts": [{"text": req.system}]},
        "contents": [{"role": "user", "parts": [{"text": req.user}]}],
        "generationConfig": {
            "responseMimeType": "application/json",
            "responseJsonSchema": req.schema,
            "maxOutputTokens": req.max_output_tokens,
        },
    })
}

/// The chat-completions URL of a local server.
pub fn local_url(e: &Endpoint) -> String {
    format!("{}/chat/completions", base(e))
}

/// Chat-completions request for a local server: with the JSON schema in
/// `response_format`, or (`schema: false`) with the schema described in
/// the system prompt for servers that cannot constrain output.
pub fn local_body(req: &Request, schema: bool) -> Value {
    let system = if schema {
        req.system.clone()
    } else {
        format!(
            "{}\n\nReply with one JSON object and nothing else: no Markdown, no explanation. \
             It must match this JSON schema:\n{}",
            req.system, req.schema
        )
    };
    let mut body = json!({
        "model": req.model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": req.user},
        ],
        "max_tokens": req.max_output_tokens,
        "stream": false,
    });
    if schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": {"name": req.schema_name, "schema": req.schema, "strict": true},
        });
    }
    body
}

pub fn completion(e: &Endpoint, req: &Request) -> (String, Value) {
    let (url, mut body) = completion_body(e, req);
    if e.provider == Provider::Openai
        && let Some(effort) = req.reasoning_effort
    {
        body["reasoning"] = json!({"effort": effort});
    }
    (url, body)
}

fn completion_body(e: &Endpoint, req: &Request) -> (String, Value) {
    match e.provider {
        Provider::Anthropic => {
            let mut body = anthropic_body(req);
            body["max_tokens"] = json!(req.max_output_tokens);
            (format!("{}/v1/messages", base(e)), body)
        }
        Provider::Openai if matches!(e.auth, Auth::ChatGpt(_)) => (
            format!("{}/v1/responses", base(e)),
            json!({
                "model": req.model,
                "instructions": req.system,
                "input": [{"role": "user", "content": req.user}],
                "text": openai_format(req),
                "store": false,
                "stream": true,
            }),
        ),
        Provider::Openai => (
            format!("{}/v1/responses", base(e)),
            json!({
                "model": req.model,
                "input": openai_input(req),
                "max_output_tokens": req.max_output_tokens,
                "text": openai_format(req),
                "store": false,
            }),
        ),
        Provider::Gemini => (format!("{}/v1beta/models/{}:generateContent", base(e), req.model), gemini_request(req)),
        Provider::Local => (local_url(e), local_body(req, true)),
    }
}

/// The provider's token-counting request; `None` for local servers, which
/// have no standard counter.
pub fn count_tokens(e: &Endpoint, req: &Request) -> Option<(String, Value)> {
    Some(match e.provider {
        Provider::Anthropic => (format!("{}/v1/messages/count_tokens", base(e)), anthropic_body(req)),
        Provider::Openai => (
            format!("{}/v1/responses/input_tokens", base(e)),
            json!({"model": req.model, "input": openai_input(req), "text": openai_format(req)}),
        ),
        Provider::Gemini => {
            let mut inner = gemini_request(req);
            inner["model"] = json!(format!("models/{}", req.model));
            (format!("{}/v1beta/models/{}:countTokens", base(e), req.model), json!({"generateContentRequest": inner}))
        }
        Provider::Local => return None,
    })
}

/// Model ids from an OpenAI-style `GET /models` reply (`{"data": [{"id"}]}`;
/// Ollama and LM Studio answer the same way), at most 200.
pub fn parse_models(v: &Value) -> Result<Vec<String>, AiError> {
    let data = v["data"].as_array().ok_or_else(|| AiError::Malformed("no model list in the reply".into()))?;
    Ok(data
        .iter()
        .filter_map(|m| m["id"].as_str())
        .filter(|id| crate::valid_model_for(Provider::Local, id))
        .take(200)
        .map(str::to_string)
        .collect())
}

pub fn parse_count(provider: Provider, v: &Value) -> Result<u64, AiError> {
    let field = match provider {
        Provider::Anthropic | Provider::Openai | Provider::Local => &v["input_tokens"],
        Provider::Gemini => &v["totalTokens"],
    };
    field.as_u64().ok_or_else(|| AiError::Malformed("token count missing".into()))
}

/// Best-effort error text from a provider error body, bounded.
pub fn error_message(body: &[u8]) -> String {
    let text = match serde_json::from_slice::<Value>(body) {
        Ok(v) => v["error"]["message"].as_str().map(str::to_string).unwrap_or_else(|| v.to_string()),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    };
    text.chars().take(500).collect()
}

const PLAN_LIMIT_CODES: [&str; 1] = ["subscription_sharing_usage_limit_exceeded"];

/// Whether an error body says the ChatGPT plan allowance is used up.
pub fn is_plan_limit(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["code"].as_str().map(|c| PLAN_LIMIT_CODES.contains(&c)))
        .unwrap_or(false)
}

/// Whether an OpenAI 429 body says the API account is out of credit
/// (`insufficient_quota`) rather than rate limited.
pub fn is_no_credit(body: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<Value>(body) else { return false };
    let e = &v["error"];
    e["code"] == "insufficient_quota"
        || e["type"] == "insufficient_quota"
        || e["message"].as_str().is_some_and(|m| m.contains("no credits remaining"))
}

/// Reads a Responses API event stream (server-sent events) and returns the
/// final `response` object. Only `response.completed` or
/// `response.incomplete` count as an answer; a stream that ends without
/// one has an unknown outcome.
///
/// The terminal event's `output` may be empty when the answer was already
/// streamed (observed on ChatGPT plans), so output items and text deltas
/// are collected along the way and used to fill it in.
pub fn final_response_from_stream(bytes: &[u8]) -> Result<Value, AiError> {
    let text = String::from_utf8_lossy(bytes);
    let mut items: Vec<Value> = Vec::new();
    let mut deltas = String::new();
    let mut handle = |data: &str| -> Option<Result<Value, AiError>> {
        let v: Value = serde_json::from_str(data).ok()?;
        match v["type"].as_str()? {
            "response.output_item.done" => {
                if v["item"].is_object() {
                    items.push(v["item"].clone());
                }
                None
            }
            "response.output_text.delta" => {
                deltas.push_str(v["delta"].as_str().unwrap_or_default());
                None
            }
            "response.completed" | "response.incomplete" => {
                let mut response = v["response"].clone();
                let has_output = response["output"].as_array().is_some_and(|o| !o.is_empty());
                if !has_output && response.is_object() {
                    response["output"] = if !items.is_empty() {
                        Value::Array(std::mem::take(&mut items))
                    } else if !deltas.is_empty() {
                        json!([{"type": "message", "content": [{"type": "output_text", "text": deltas}]}])
                    } else {
                        json!([])
                    };
                }
                Some(Ok(response))
            }
            kind @ ("response.failed" | "error") => {
                let err = if kind == "error" { &v["error"] } else { &v["response"]["error"] };
                let err = if err.is_null() { &v } else { err };
                let code = err["code"].as_str().unwrap_or_default();
                let message: String = err["message"].as_str().unwrap_or(code).chars().take(500).collect();
                Some(Err(if PLAN_LIMIT_CODES.contains(&code) {
                    AiError::PlanLimit(message)
                } else {
                    AiError::Malformed(format!("response failed ({code}): {message}"))
                }))
            }
            _ => None,
        }
    };
    let mut data = String::new();
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if !data.is_empty()
                && let Some(r) = handle(&data)
            {
                return r;
            }
            data.clear();
        } else if let Some(d) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    Err(AiError::Unknown("the response stream ended before the answer was complete".into()))
}

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// Parses model text as JSON. Tolerates a Markdown code fence or prose
/// around one JSON object; the result is still validated by the caller.
fn json_or_text(text: String) -> Outcome {
    // Reasoning models on local servers may put their thinking first.
    let t = match text.trim_start().strip_prefix("<think>").and_then(|rest| rest.split_once("</think>")) {
        Some((_, answer)) => answer.trim(),
        None => text.trim(),
    };
    let unfenced = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .and_then(|rest| rest.trim_end().strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(t);
    let object = match (unfenced.find('{'), unfenced.rfind('}')) {
        (Some(a), Some(b)) if a < b => Some(&unfenced[a..=b]),
        _ => None,
    };
    for candidate in [Some(unfenced), object].into_iter().flatten() {
        if let Ok(v) = serde_json::from_str::<Value>(candidate) {
            return Outcome::Json(v);
        }
    }
    Outcome::NotJson(text.chars().take(500).collect())
}

pub fn parse_completion(provider: Provider, v: &Value) -> Result<Reply, AiError> {
    match provider {
        Provider::Anthropic => {
            let usage = &v["usage"];
            let usage = Usage {
                input: u(&usage["input_tokens"]),
                cached_input: u(&usage["cache_read_input_tokens"]),
                cache_write: u(&usage["cache_creation_input_tokens"]),
                output: u(&usage["output_tokens"]),
            };
            let stop = v["stop_reason"].as_str().unwrap_or_default();
            let text: String = v["content"]
                .as_array()
                .ok_or_else(|| AiError::Malformed("missing content".into()))?
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            let outcome = match stop {
                "refusal" => Outcome::Refused(
                    v["stop_details"]["category"]
                        .as_str()
                        .map_or_else(|| "declined".to_string(), |c| format!("declined ({c})")),
                ),
                "max_tokens" => Outcome::Truncated,
                _ => json_or_text(text),
            };
            // Anthropic bills thinking as output but does not split it out.
            Ok(Reply { outcome, usage, reasoning_tokens: 0 })
        }
        Provider::Openai => {
            let usage = &v["usage"];
            let input = u(&usage["input_tokens"]);
            let cached = u(&usage["input_tokens_details"]["cached_tokens"]).min(input);
            let usage_out = Usage {
                input: input - cached,
                cached_input: cached,
                cache_write: 0,
                output: u(&usage["output_tokens"]),
            };
            let reasoning = u(&usage["output_tokens_details"]["reasoning_tokens"]);
            let mut text = String::new();
            let mut refusal = None;
            for item in v["output"].as_array().map(Vec::as_slice).unwrap_or_default() {
                if item["type"] != "message" {
                    continue;
                }
                for c in item["content"].as_array().map(Vec::as_slice).unwrap_or_default() {
                    match c["type"].as_str() {
                        Some("output_text") => text.push_str(c["text"].as_str().unwrap_or_default()),
                        Some("refusal") => refusal = Some(c["refusal"].as_str().unwrap_or("declined").to_string()),
                        _ => {}
                    }
                }
            }
            let outcome = if let Some(r) = refusal {
                Outcome::Refused(r.chars().take(300).collect())
            } else if v["status"] == "incomplete" {
                match v["incomplete_details"]["reason"].as_str() {
                    Some("content_filter") => Outcome::Refused("content filter".into()),
                    _ => Outcome::Truncated,
                }
            } else if v["status"] == "completed" {
                json_or_text(text)
            } else {
                return Err(AiError::Malformed(format!("unexpected status {}", v["status"])));
            };
            Ok(Reply { outcome, usage: usage_out, reasoning_tokens: reasoning })
        }
        Provider::Gemini => {
            let m = &v["usageMetadata"];
            let prompt = u(&m["promptTokenCount"]);
            let cached = u(&m["cachedContentTokenCount"]).min(prompt);
            let thoughts = u(&m["thoughtsTokenCount"]);
            let usage = Usage {
                input: prompt - cached,
                cached_input: cached,
                cache_write: 0,
                output: u(&m["candidatesTokenCount"]) + thoughts,
            };
            if let Some(reason) = v["promptFeedback"]["blockReason"].as_str() {
                return Ok(Reply {
                    outcome: Outcome::Refused(format!("blocked ({reason})")),
                    usage,
                    reasoning_tokens: thoughts,
                });
            }
            let cand = &v["candidates"][0];
            if cand.is_null() {
                return Err(AiError::Malformed("no candidates".into()));
            }
            let text: String = cand["content"]["parts"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .filter(|p| p["thought"] != true)
                .filter_map(|p| p["text"].as_str())
                .collect();
            let outcome = match cand["finishReason"].as_str().unwrap_or("STOP") {
                "STOP" => json_or_text(text),
                "MAX_TOKENS" => Outcome::Truncated,
                other => Outcome::Refused(format!("stopped ({other})")),
            };
            Ok(Reply { outcome, usage, reasoning_tokens: thoughts })
        }
        Provider::Local => {
            let usage = &v["usage"];
            let prompt = u(&usage["prompt_tokens"]);
            let cached = u(&usage["prompt_tokens_details"]["cached_tokens"]).min(prompt);
            let usage_out = Usage {
                input: prompt - cached,
                cached_input: cached,
                cache_write: 0,
                output: u(&usage["completion_tokens"]),
            };
            let reasoning = u(&usage["completion_tokens_details"]["reasoning_tokens"]);
            let choice = &v["choices"][0];
            if choice.is_null() {
                return Err(AiError::Malformed("no choices".into()));
            }
            let message = &choice["message"];
            let outcome = if let Some(r) = message["refusal"].as_str().filter(|r| !r.is_empty()) {
                Outcome::Refused(r.chars().take(300).collect())
            } else {
                match choice["finish_reason"].as_str().unwrap_or("stop") {
                    "length" => Outcome::Truncated,
                    "content_filter" => Outcome::Refused("content filter".into()),
                    _ => json_or_text(message["content"].as_str().unwrap_or_default().to_string()),
                }
            };
            Ok(Reply { outcome, usage: usage_out, reasoning_tokens: reasoning })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> Request {
        Request {
            model: "m-1".into(),
            system: "sys".into(),
            user: "usr".into(),
            schema_name: "s".into(),
            schema: json!({"type": "object"}),
            max_output_tokens: 100,
            reasoning_effort: None,
        }
    }

    fn ep(p: Provider) -> Endpoint {
        Endpoint { provider: p, base_url: "http://x/".into(), auth: Auth::ApiKey("k".into()) }
    }

    #[test]
    fn builds_provider_requests() {
        let (url, b) = completion(&ep(Provider::Anthropic), &req());
        assert_eq!(url, "http://x/v1/messages");
        assert_eq!(b["max_tokens"], 100);
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert!(b.get("temperature").is_none(), "sampling params are removed in 2026-10-21");

        let (url, b) = completion(&ep(Provider::Openai), &req());
        assert_eq!(url, "http://x/v1/responses");
        assert_eq!(b["store"], false);
        assert_eq!(b["text"]["format"]["strict"], true);

        let (url, b) = completion(&ep(Provider::Gemini), &req());
        assert_eq!(url, "http://x/v1beta/models/m-1:generateContent");
        assert_eq!(b["generationConfig"]["maxOutputTokens"], 100);

        let (url, b) = count_tokens(&ep(Provider::Gemini), &req()).unwrap();
        assert_eq!(url, "http://x/v1beta/models/m-1:countTokens");
        assert_eq!(b["generateContentRequest"]["model"], "models/m-1");
    }

    #[test]
    fn builds_local_requests_with_and_without_a_schema() {
        let mut e = ep(Provider::Local);
        e.base_url = "http://192.168.1.20:8080/v1/".into();
        let r = Request { reasoning_effort: Some("low"), ..req() };
        let (url, b) = completion(&e, &r);
        assert_eq!(url, "http://192.168.1.20:8080/v1/chat/completions");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "usr");
        assert_eq!(b["max_tokens"], 100);
        assert_eq!(b["stream"], false);
        assert_eq!(b["response_format"]["type"], "json_schema");
        assert_eq!(b["response_format"]["json_schema"]["name"], "s");
        assert_eq!(b["response_format"]["json_schema"]["schema"], json!({"type": "object"}));
        assert!(b.get("reasoning").is_none(), "OpenAI-only parameters are not sent");
        assert!(b.get("store").is_none());

        let plain = local_body(&r, false);
        assert!(plain.get("response_format").is_none());
        let system = plain["messages"][0]["content"].as_str().unwrap();
        assert!(system.starts_with("sys\n\n") && system.contains(r#"{"type":"object"}"#), "{system}");
        assert!(count_tokens(&e, &r).is_none());
        assert_eq!(auth_headers(Provider::Local, "k"), vec![("authorization", "Bearer k".to_string())]);
    }

    #[test]
    fn parses_local_chat_completions() {
        let v = json!({"choices": [{"message": {"role": "assistant", "content": "{\"a\":4}"}, "finish_reason": "stop"}],
                       "usage": {"prompt_tokens": 80, "completion_tokens": 12, "prompt_tokens_details": {"cached_tokens": 30}}});
        let r = parse_completion(Provider::Local, &v).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 4})));
        assert_eq!((r.usage.input, r.usage.cached_input, r.usage.output), (50, 30, 12));
        // Thinking first, then a fenced answer, no usage block (some servers omit it).
        let think = json!({"choices": [{"message": {"content": "<think>\nThe user wants {json}.\n</think>\n```json\n{\"a\":5}\n```"},
                           "finish_reason": "stop"}]});
        let r = parse_completion(Provider::Local, &think).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 5})));
        assert_eq!(r.usage.total(), 0);
        let cut = json!({"choices": [{"message": {"content": "{\"a\":"}, "finish_reason": "length"}], "usage": {}});
        assert_eq!(parse_completion(Provider::Local, &cut).unwrap().outcome, Outcome::Truncated);
        let refused = json!({"choices": [{"message": {"content": null, "refusal": "no"}, "finish_reason": "stop"}]});
        assert_eq!(parse_completion(Provider::Local, &refused).unwrap().outcome, Outcome::Refused("no".into()));
        let prose = json!({"choices": [{"message": {"content": "Sure! Here are labels."}, "finish_reason": "stop"}]});
        assert!(matches!(parse_completion(Provider::Local, &prose).unwrap().outcome, Outcome::NotJson(_)));
        assert!(parse_completion(Provider::Local, &json!({"choices": []})).is_err());
        let models = json!({"object": "list", "data": [{"id": "qwen3:8b"}, {"id": "bad id"}, {"id": "Qwen/Qwen3-8B"}]});
        assert_eq!(parse_models(&models).unwrap(), vec!["qwen3:8b", "Qwen/Qwen3-8B"]);
        assert!(parse_models(&json!({"models": []})).is_err());
    }

    #[test]
    fn reads_the_final_response_from_a_stream() {
        let completed = json!({"type": "response.completed", "response": {"status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "{\"a\":1}"}]}],
            "usage": {"input_tokens": 9, "output_tokens": 3}}});
        let sse = format!(
            "event: response.created\ndata: {}\n\nevent: response.output_text.delta\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
            json!({"type": "response.created", "response": {"status": "in_progress"}}),
            json!({"type": "response.output_text.delta", "delta": "{\"a\""}),
            completed
        );
        let v = final_response_from_stream(sse.as_bytes()).unwrap();
        let r = parse_completion(Provider::Openai, &v).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 1})));
        assert_eq!(r.usage.total(), 12);

        let limit = format!(
            "data: {}\n\n",
            json!({"type": "response.failed", "response": {"error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "limit"}}})
        );
        assert!(matches!(final_response_from_stream(limit.as_bytes()), Err(AiError::PlanLimit(_))));
        let cut = format!("data: {}\n\n", json!({"type": "response.output_text.delta", "delta": "x"}));
        assert!(matches!(final_response_from_stream(cut.as_bytes()), Err(AiError::Unknown(_))));
        assert!(is_plan_limit(br#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"m"}}"#));
        assert!(!is_plan_limit(br#"{"error":{"code":"rate_limit_exceeded"}}"#));
        assert!(is_no_credit(br#"{"error":{"type":"insufficient_quota","code":"insufficient_quota","message":"m"}}"#));
        assert!(is_no_credit(
            br#"{"error":{"message":"You have no credits remaining. Add credits to continue using the API."}}"#
        ));
        assert!(!is_no_credit(br#"{"error":{"code":"rate_limit_exceeded","type":"requests","message":"slow down"}}"#));
    }

    #[test]
    fn rebuilds_output_missing_from_the_completed_event() {
        let item = json!({"type": "message", "content": [{"type": "output_text", "text": "{\"a\":2}"}]});
        let empty_done = json!({"type": "response.completed",
            "response": {"status": "completed", "output": [], "usage": {"input_tokens": 5, "output_tokens": 4}}});
        let from_items =
            format!("data: {}\n\ndata: {}\n\n", json!({"type": "response.output_item.done", "item": item}), empty_done);
        let v = final_response_from_stream(from_items.as_bytes()).unwrap();
        assert_eq!(parse_completion(Provider::Openai, &v).unwrap().outcome, Outcome::Json(json!({"a": 2})));

        let from_deltas = format!(
            "data: {}\n\ndata: {}\n\ndata: {}\n\n",
            json!({"type": "response.output_text.delta", "delta": "{\"a\":"}),
            json!({"type": "response.output_text.delta", "delta": "3}"}),
            empty_done
        );
        let v = final_response_from_stream(from_deltas.as_bytes()).unwrap();
        let r = parse_completion(Provider::Openai, &v).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 3})));
        assert_eq!(r.usage.total(), 9);
    }

    #[test]
    fn json_in_fences_or_prose_is_accepted() {
        assert_eq!(json_or_text("```json\n{\"a\":1}\n```".into()), Outcome::Json(json!({"a": 1})));
        assert_eq!(json_or_text("```\n{\"a\":1}\n```".into()), Outcome::Json(json!({"a": 1})));
        assert_eq!(json_or_text("Here you go: {\"a\":1} done".into()), Outcome::Json(json!({"a": 1})));
        assert!(matches!(json_or_text("no json".into()), Outcome::NotJson(_)));
        assert!(matches!(json_or_text(String::new()), Outcome::NotJson(_)));
    }

    #[test]
    fn parses_anthropic() {
        let v = json!({"content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": "{\"a\":1}"}],
                       "stop_reason": "end_turn",
                       "usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2, "output_tokens": 7}});
        let r = parse_completion(Provider::Anthropic, &v).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 1})));
        assert_eq!(r.usage, Usage { input: 10, cached_input: 5, cache_write: 2, output: 7 });
        let refused =
            json!({"content": [], "stop_reason": "refusal", "stop_details": {"category": "cyber"}, "usage": {}});
        assert_eq!(
            parse_completion(Provider::Anthropic, &refused).unwrap().outcome,
            Outcome::Refused("declined (cyber)".into())
        );
    }

    #[test]
    fn parses_openai() {
        let v = json!({"status": "completed",
                       "output": [{"type": "reasoning"}, {"type": "message", "content": [{"type": "output_text", "text": "{\"a\":2}"}]}],
                       "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 40}, "output_tokens": 30,
                                 "output_tokens_details": {"reasoning_tokens": 20}}});
        let r = parse_completion(Provider::Openai, &v).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 2})));
        assert_eq!((r.usage.input, r.usage.cached_input, r.usage.output, r.reasoning_tokens), (60, 40, 30, 20));
        let inc = json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "output": [], "usage": {}});
        assert_eq!(parse_completion(Provider::Openai, &inc).unwrap().outcome, Outcome::Truncated);
    }

    #[test]
    fn parses_gemini() {
        let v = json!({"candidates": [{"content": {"parts": [{"text": "plan", "thought": true}, {"text": "{\"a\":3}"}]}, "finishReason": "STOP"}],
                       "usageMetadata": {"promptTokenCount": 50, "cachedContentTokenCount": 10, "candidatesTokenCount": 5, "thoughtsTokenCount": 15}});
        let r = parse_completion(Provider::Gemini, &v).unwrap();
        assert_eq!(r.outcome, Outcome::Json(json!({"a": 3})));
        assert_eq!((r.usage.input, r.usage.cached_input, r.usage.output, r.reasoning_tokens), (40, 10, 20, 15));
        let blocked = json!({"promptFeedback": {"blockReason": "SAFETY"}, "usageMetadata": {"promptTokenCount": 5}});
        assert!(matches!(parse_completion(Provider::Gemini, &blocked).unwrap().outcome, Outcome::Refused(_)));
    }
}
