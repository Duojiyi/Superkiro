//! Anthropic-compatible model provider implementation (Spec §15.2).

use super::{
    endpoint_with_suffix, process_byte_stream, sanitize_upstream_error_body, BoxFuture, BoxStream,
    ChatRequest, ModelProvider, ProviderConfig, ProviderDelta, ProviderError, ProviderStreamEvent,
    TokenUsage,
};
use futures_util::TryStreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::Value;
use std::time::Duration;

pub struct AnthropicProvider;

fn normalize_content_block(val: &Value) -> Value {
    if let Some(obj) = val.as_object() {
        if obj.get("type").and_then(|t| t.as_str()) == Some("image_url") {
            if let Some(image_url) = obj
                .get("image_url")
                .and_then(|u| u.get("url"))
                .and_then(|u| u.as_str())
            {
                if let Some(rest) = image_url.strip_prefix("data:") {
                    if let Some((media_type, data)) = rest.split_once(";base64,") {
                        return serde_json::json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            }
                        });
                    }
                }
            }
        }
    }
    val.clone()
}

impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn endpoint_url(&self, base_url: &str) -> String {
        endpoint_with_suffix(base_url, "/v1", "/messages")
    }

    fn translate_request(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        let mut system_text = String::new();
        let mut raw_messages: Vec<(&'static str, Vec<Value>)> = Vec::new();

        for m in &req.messages {
            if m.role == "system" {
                if !system_text.is_empty() {
                    system_text.push('\n');
                }
                if let serde_json::Value::String(s) = &m.content {
                    system_text.push_str(s);
                } else {
                    system_text.push_str(&m.content.to_string());
                }
            } else if m.role == "assistant" {
                let mut blocks = Vec::new();
                if let Value::String(s) = &m.content {
                    if !s.is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": s,
                        }));
                    }
                } else if let Value::Array(arr) = &m.content {
                    for item in arr {
                        blocks.push(normalize_content_block(item));
                    }
                }
                for tc in &m.tool_calls {
                    blocks.push(serde_json::json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments,
                    }));
                }
                if !blocks.is_empty() {
                    raw_messages.push(("assistant", blocks));
                }
            } else if m.role == "tool" || m.tool_call_id.is_some() {
                let content = if let Value::String(s) = &m.content {
                    Value::String(s.clone())
                } else {
                    Value::String(m.content.to_string())
                };
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": content,
                });
                if let Some(true) = m.is_error {
                    block["is_error"] = serde_json::json!(true);
                }
                raw_messages.push(("user", vec![block]));
            } else {
                // user role
                let mut blocks = Vec::new();
                if let Value::String(s) = &m.content {
                    if !s.is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": s,
                        }));
                    }
                } else if let Value::Array(arr) = &m.content {
                    for item in arr {
                        blocks.push(normalize_content_block(item));
                    }
                }
                if !blocks.is_empty() {
                    raw_messages.push(("user", blocks));
                }
            }
        }

        // Merge consecutive turns with the same role
        let mut messages: Vec<Value> = Vec::new();
        for (role, blocks) in raw_messages {
            if let Some(last) = messages.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some(role) {
                    if let Some(existing_content) =
                        last.get_mut("content").and_then(|c| c.as_array_mut())
                    {
                        existing_content.extend(blocks);
                        continue;
                    }
                }
            }
            messages.push(serde_json::json!({
                "role": role,
                "content": blocks,
            }));
        }

        if messages.is_empty() {
            messages.push(serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": ""}]
            }));
        } else if messages[0].get("role").and_then(|r| r.as_str()) == Some("assistant") {
            messages.insert(
                0,
                serde_json::json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "(continue)"}]
                }),
            );
        }

        let max_tokens = req.max_tokens.unwrap_or(4096);
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "max_tokens": max_tokens,
            "stream": true,
        });

        if !system_text.is_empty() {
            body["system"] = serde_json::json!(system_text);
        }

        if let Some(temp) = req.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if let Some(effort) = req.reasoning_effort {
            use kiro_wire::requests::conversation::ReasoningEffort::*;
            if max_tokens <= 1024 {
                return Err(ProviderError::Serialization(
                    "Thinking requires max output greater than 1024".into(),
                ));
            }
            let model = req.model.to_ascii_lowercase();
            let adaptive = model.contains("sonnet-4-6")
                || model.contains("sonnet-4.6")
                || model.contains("opus-4-6")
                || model.contains("opus-4.6");
            if adaptive {
                body["thinking"] = serde_json::json!({"type": "adaptive"});
                body["output_config"] = serde_json::json!({"effort": match effort {
                    Low => "low", Medium => "medium", Max if model.contains("opus") => "max",
                    High | Xhigh | Max => "high",
                }});
            } else {
                let budget = match effort {
                    Low => 1024,
                    Medium => 4096,
                    High => 8192,
                    Xhigh => 16384,
                    Max => 24576,
                };
                body["thinking"] = serde_json::json!({"type": "enabled", "budget_tokens": budget.min(max_tokens - 1)});
            }
            body.as_object_mut().unwrap().remove("temperature");
        }

        if !req.tools.is_empty() {
            let normalized_tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    if let Some(spec) = t.get("toolSpec").or_else(|| t.get("toolSpecification")) {
                        let name = spec
                            .get("name")
                            .cloned()
                            .unwrap_or(Value::String(String::new()));
                        let desc = spec
                            .get("description")
                            .cloned()
                            .unwrap_or(Value::String(String::new()));
                        let input_schema = spec
                            .get("inputSchema")
                            .or_else(|| spec.get("input_schema"))
                            .or_else(|| spec.get("input").and_then(|i| i.get("json")))
                            .cloned()
                            .unwrap_or_else(
                                || serde_json::json!({"type": "object", "properties": {}}),
                            );
                        serde_json::json!({
                            "name": name,
                            "description": desc,
                            "input_schema": input_schema,
                        })
                    } else if let Some(func) = t.get("function") {
                        let name = func
                            .get("name")
                            .cloned()
                            .unwrap_or(Value::String(String::new()));
                        let desc = func
                            .get("description")
                            .cloned()
                            .unwrap_or(Value::String(String::new()));
                        let input_schema = func.get("parameters").cloned().unwrap_or_else(
                            || serde_json::json!({"type": "object", "properties": {}}),
                        );
                        serde_json::json!({
                            "name": name,
                            "description": desc,
                            "input_schema": input_schema,
                        })
                    } else {
                        t.clone()
                    }
                })
                .collect();
            body["tools"] = serde_json::json!(normalized_tools);
        }

        Ok(body)
    }

    fn parse_stream_line(&self, line: &str) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        let line = line.trim();
        if !line.starts_with("data:") {
            return Ok(Vec::new());
        }

        let data = line[5..].trim();
        let val: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Parse(format!("Failed to parse Anthropic JSON: {}", e)))?;

        let event_type = val.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if val.get("error").is_some_and(|error| !error.is_null())
            || val.get("type").and_then(Value::as_str) == Some("error")
        {
            return Err(super::retry::stream_error(&val));
        }

        let mut events = Vec::new();

        match event_type {
            "message_start" => {
                if let Some(msg) = val.get("message") {
                    if let Some(mut usage) = self.extract_usage(msg) {
                        usage.output_tokens_final = false;
                        events.push(ProviderStreamEvent::Usage(usage));
                    }
                }
            }
            "content_block_start" => {
                let index = val.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                if let Some(block) = val.get("content_block") {
                    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    if block_type == "tool_use" {
                        let id = block
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(ToString::to_string);
                        let name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .map(ToString::to_string);
                        events.push(ProviderStreamEvent::Delta(ProviderDelta::ToolCallChunk {
                            index,
                            id,
                            name,
                            arguments: String::new(),
                        }));
                    }
                }
            }
            "content_block_delta" => {
                let index = val.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                if let Some(delta) = val.get("delta") {
                    let delta_type = delta.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    match delta_type {
                        "text_delta" => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    events.push(ProviderStreamEvent::Delta(ProviderDelta::Text(
                                        text.to_string(),
                                    )));
                                }
                            }
                        }
                        "thinking_delta" => {
                            if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                                if !thinking.is_empty() {
                                    events.push(ProviderStreamEvent::Delta(
                                        ProviderDelta::Reasoning(thinking.to_string()),
                                    ));
                                }
                            }
                        }
                        "input_json_delta" => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|p| p.as_str())
                            {
                                events.push(ProviderStreamEvent::Delta(
                                    ProviderDelta::ToolCallChunk {
                                        index,
                                        id: None,
                                        name: None,
                                        arguments: partial.to_string(),
                                    },
                                ));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = val.get("delta") {
                    if let Some(stop_reason) = delta.get("stop_reason").and_then(|s| s.as_str()) {
                        events.push(ProviderStreamEvent::StopReason(stop_reason.to_string()));
                    }
                }
                if let Some(usage) = val.get("usage") {
                    let output_tokens = usage
                        .get("output_tokens")
                        .and_then(|o| o.as_u64())
                        .unwrap_or(0);
                    events.push(ProviderStreamEvent::Usage(TokenUsage {
                        uncached_prompt_tokens: 0,
                        prompt_tokens: 0,
                        completion_tokens: output_tokens,
                        total_tokens: output_tokens,
                        output_tokens_final: usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .is_some(),
                        cache_read_input_tokens: None,
                        cache_creation_input_tokens: None,
                    }));
                }
            }
            "message_stop" => {
                events.push(ProviderStreamEvent::Done);
            }
            _ => {}
        }

        Ok(events)
    }

    fn extract_usage(&self, value: &Value) -> Option<TokenUsage> {
        let usage = value.get("usage")?;
        let input_tokens = usage
            .get("input_tokens")
            .and_then(|i| i.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("output_tokens")
            .and_then(|o| o.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|c| c.as_u64());
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(|c| c.as_u64());

        let total_prompt = input_tokens
            .saturating_add(cache_read.unwrap_or(0))
            .saturating_add(cache_creation.unwrap_or(0));

        Some(TokenUsage {
            uncached_prompt_tokens: input_tokens,
            prompt_tokens: total_prompt,
            completion_tokens: output_tokens,
            total_tokens: total_prompt.saturating_add(output_tokens),
            output_tokens_final: usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .is_some(),
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
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
                "x-api-key",
                HeaderValue::from_str(&config.api_key)
                    .map_err(|e| ProviderError::Serialization(e.to_string()))?,
            );
            headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

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
            let event_stream = process_byte_stream(byte_stream, |line| {
                AnthropicProvider.parse_stream_line(line)
            });

            Ok(event_stream)
        })
    }
}
