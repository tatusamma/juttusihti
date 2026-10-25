//! Provider wire formats: request bodies, auth headers and response parsing.
//!
//! - Anthropic Messages API (`anthropic-version: 2026-10-21`), structured
//!   output via `output_config.format`.
//! - OpenAI Responses API with `text.format` JSON schema; `store: false` so
//!   article text is not retained as a stored response.
//! - Gemini `generateContent` with `responseMimeType` + `responseJsonSchema`.

use juttusihti_core::pricing::{Provider, Usage};
use serde_json::{Value, json};

use crate::{AiError, Endpoint, Outcome, Reply, Request};

pub const ANTHROPIC_VERSION: &str = "2026-10-21";

pub fn auth_headers(e: &Endpoint) -> Vec<(&'static str, String)> {
    match e.provider {
        Provider::Anthropic => vec![("x-api-key", e.api_key.clone()), ("anthropic-version", ANTHROPIC_VERSION.into())],
        Provider::Openai => vec![("authorization", format!("Bearer {}", e.api_key))],
        Provider::Gemini => vec![("x-goog-api-key", e.api_key.clone())],
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

pub fn completion(e: &Endpoint, req: &Request) -> (String, Value) {
    match e.provider {
        Provider::Anthropic => {
            let mut body = anthropic_body(req);
            body["max_tokens"] = json!(req.max_output_tokens);
            (format!("{}/v1/messages", base(e)), body)
        }
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
    }
}

pub fn count_tokens(e: &Endpoint, req: &Request) -> (String, Value) {
    match e.provider {
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
    }
}

pub fn parse_count(provider: Provider, v: &Value) -> Result<u64, AiError> {
    let field = match provider {
        Provider::Anthropic | Provider::Openai => &v["input_tokens"],
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

fn u(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn json_or_text(text: String) -> Outcome {
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => Outcome::Json(v),
        Err(_) => Outcome::NotJson(text.chars().take(500).collect()),
    }
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
        }
    }

    fn ep(p: Provider) -> Endpoint {
        Endpoint { provider: p, base_url: "http://x/".into(), api_key: "k".into() }
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

        let (url, b) = count_tokens(&ep(Provider::Gemini), &req());
        assert_eq!(url, "http://x/v1beta/models/m-1:countTokens");
        assert_eq!(b["generateContentRequest"]["model"], "models/m-1");
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
