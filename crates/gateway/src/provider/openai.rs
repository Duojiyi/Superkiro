//! OpenAI-compatible model provider implementation (Spec §15.2).

use super::{
    endpoint_with_suffix, process_byte_stream, sanitize_upstream_error_body, BoxFuture, BoxStream,
    ChatRequest, ModelProvider, ProviderConfig, ProviderDelta, ProviderError, ProviderStreamEvent,
    TokenUsage,
};
use futures_util::TryStreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;
use std::time::Duration;

pub struct OpenAiProvider;

impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn endpoint_url(&self, base_url: &str) -> String {
        endpoint_with_suffix(base_url, "/v1", "/chat/completions")
    }

    fn translate_request(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let mut obj = serde_json::json!({
                    "role": m.role,
                });
                if !m.content.is_null() {
                    obj["content"] = m.content.clone();
                } else {
                    obj["content"] = Value::Null;
                }
                if let Some(ref name) = m.name {
                    obj["name"] = Value::String(name.clone());
                }
                if let Some(ref tid) = m.tool_call_id {
                    obj["tool_call_id"] = Value::String(tid.clone());
                }
                if !m.tool_calls.is_empty() {
                    let tc_list: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            let args_str = if tc.arguments.is_string() {
                                tc.arguments.as_str().unwrap().to_string()
                            } else {
                                tc.arguments.to_string()
                            };
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": args_str
                                }
                            })
                        })
                        .collect();
                    obj["tool_calls"] = Value::Array(tc_list);
                }
                obj
            })
            .collect();

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        if let Some(effort) = req.reasoning_effort {
            use kiro_wire::requests::conversation::ReasoningEffort::*;
            body["reasoning_effort"] = serde_json::json!(match effort {
                Low => "low",
                Medium => "medium",
                High | Xhigh | Max => "high",
            });
        }
        if let Some(temp) = req.temperature.filter(|_| req.reasoning_effort.is_none()) {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    let spec = if let Some(s) = t.get("toolSpecification") {
                        s
                    } else {
                        t
                    };
                    let name = spec.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let desc = spec
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or("");
                    let schema = spec
                        .get("inputSchema")
                        .or_else(|| spec.get("input_schema"))
                        .or_else(|| spec.get("parameters"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
                    let parameters = if let Some(inner) = schema.get("json") {
                        inner.clone()
                    } else {
                        schema
                    };
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": desc,
                            "parameters": parameters
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        Ok(body)
    }

    fn parse_stream_line(&self, line: &str) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        let line = line.trim();
        if !line.starts_with("data:") {
            return Ok(Vec::new());
        }

        let data = line[5..].trim();
        if data == "[DONE]" {
            return Ok(vec![ProviderStreamEvent::Done]);
        }

        let val: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Parse(format!("Failed to parse OpenAI JSON: {}", e)))?;

        if val.get("error").is_some_and(|error| !error.is_null())
            || val.get("type").and_then(Value::as_str) == Some("error")
        {
            return Err(super::retry::stream_error(&val));
        }

        let mut events = Vec::new();

        // 1. Check for token usage in chunk
        if let Some(usage) = self.extract_usage(&val) {
            events.push(ProviderStreamEvent::Usage(usage));
        }

        // 2. Check choices delta
        if let Some(choices) = val.get("choices").and_then(|c| c.as_array()) {
            if let Some(first) = choices.first() {
                if let Some(delta) = first.get("delta") {
                    // Content text
                    if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                        if !content.is_empty() {
                            events.push(ProviderStreamEvent::Delta(ProviderDelta::Text(
                                content.to_string(),
                            )));
                        }
                    }

                    // Reasoning content (DeepSeek / OpenAI reasoning models)
                    if let Some(reasoning) = delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .and_then(|r| r.as_str())
                    {
                        if !reasoning.is_empty() {
                            events.push(ProviderStreamEvent::Delta(ProviderDelta::Reasoning(
                                reasoning.to_string(),
                            )));
                        }
                    }

                    // Tool calls
                    if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
                        for tc in tool_calls {
                            // Some compatible upstreams omit the index; the call's id then
                            // tells parallel calls apart.
                            let idx = tc.get("index").and_then(|i| i.as_u64()).map(|i| i as usize);
                            let id = tc
                                .get("id")
                                .and_then(|i| i.as_str())
                                .map(ToString::to_string);
                            let func = tc.get("function");
                            let name = func
                                .and_then(|f| f.get("name"))
                                .and_then(|n| n.as_str())
                                .map(ToString::to_string);
                            let args = func
                                .and_then(|f| f.get("arguments"))
                                .and_then(|a| a.as_str())
                                .unwrap_or("")
                                .to_string();

                            events.push(ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk {
                                index: idx,
                                id,
                                name,
                                arguments: args,
                            }));
                        }
                    }
                }

                // Stop reason / finish reason
                if let Some(finish_reason) = first.get("finish_reason").and_then(|f| f.as_str()) {
                    events.push(ProviderStreamEvent::StopReason(finish_reason.to_string()));
                }
            }
        }

        Ok(events)
    }

    fn extract_usage(&self, value: &Value) -> Option<TokenUsage> {
        let usage = value.get("usage")?;
        let prompt_tokens = usage.get("prompt_tokens")?.as_u64()?;
        let completion_tokens = usage.get("completion_tokens")?.as_u64()?;
        let total_tokens = usage
            .get("total_tokens")
            .and_then(|t| t.as_u64())
            .unwrap_or(prompt_tokens.saturating_add(completion_tokens));

        let cached_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|c| c.as_u64());

        let uncached_prompt_tokens = prompt_tokens.saturating_sub(cached_tokens.unwrap_or(0));

        Some(TokenUsage {
            uncached_prompt_tokens,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            output_tokens_final: true,
            cache_read_input_tokens: cached_tokens,
            cache_creation_input_tokens: None,
        })
    }

    fn chat_stream<'a>(
        &'a self,
        client: &'a reqwest::Client,
        config: &'a ProviderConfig,
        request: &'a ChatRequest,
    ) -> BoxFuture<
        'a,
        Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>,
    > {
        Box::pin(async move {
            let url = self.endpoint_url(&config.base_url);
            let body = self.translate_request(request)?;

            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", config.api_key))
                    .map_err(|e| ProviderError::Serialization(e.to_string()))?,
            );

            // Bound connection and response-header wait separately from the
            // long-lived streaming body timeout.
            let resp = tokio::time::timeout(
                Duration::from_secs(15).min(config.timeout),
                client
                    .post(&url)
                    .headers(headers)
                    .json(&body)
                    .timeout(config.timeout)
                    .send(),
            )
            .await
            .map_err(|_| ProviderError::Timeout)?
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout
                } else {
                    ProviderError::Network(e.to_string())
                }
            })?;

            let status = resp.status();
            if !status.is_success() {
                let err_text = sanitize_upstream_error_body(resp.text().await.unwrap_or_default());
                return Err(ProviderError::Http(status, err_text));
            }

            let byte_stream = resp.bytes_stream().map_err(reqwest::Error::from);
            let event_stream =
                process_byte_stream(byte_stream, |line| OpenAiProvider.parse_stream_line(line));

            Ok(event_stream)
        })
    }
}
