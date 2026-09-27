//! Anthropic-compatible model provider implementation (Spec §15.2).

use super::family::{ModelFamily, Reasoning};
use super::{
    endpoint_with_suffix, process_byte_stream, sanitize_upstream_error_body, BoxFuture, BoxStream,
    ChatRequest, ModelProvider, ProviderConfig, ProviderDelta, ProviderError, ProviderOptions,
    ProviderStreamEvent, TokenUsage,
};
use futures_util::TryStreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::Value;

pub struct AnthropicProvider;

/// Prompt-cache breakpoints, at most the four allowed, placed by the request's shape alone
/// so that the next agent step finds the same prefix: after the tools, after the system
/// prompt, and at the end of the last two user turns (this one, which the next step
/// extends, and the one before, where this step's prefix was cached).
fn add_cache_breakpoints(body: &mut Value) {
    let ephemeral = || serde_json::json!({"type": "ephemeral"});
    if let Some(last) = body
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|tools| tools.last_mut())
    {
        last["cache_control"] = ephemeral();
    }
    if let Some(system) = body.get_mut("system") {
        if let Some(text) = system.as_str().filter(|text| !text.is_empty()) {
            *system =
                serde_json::json!([{"type": "text", "text": text, "cache_control": ephemeral()}]);
        }
    }
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages
        .iter_mut()
        .rev()
        .filter(|message| message["role"] == "user")
        .take(2)
    {
        let Some(block) = message
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .and_then(|blocks| blocks.last_mut())
        else {
            continue;
        };
        // An empty text block cannot carry a breakpoint.
        if block["type"] != "text" || block["text"].as_str().is_some_and(|text| !text.is_empty()) {
            block["cache_control"] = ephemeral();
        }
    }
}

fn adaptive_thinking(summarized: bool) -> Value {
    if summarized {
        serde_json::json!({"type": "adaptive", "display": "summarized"})
    } else {
        serde_json::json!({"type": "adaptive"})
    }
}

fn normalize_content_block(val: &Value) -> Value {
    // An attached PDF, as the translator writes it (OpenAI's `file` part).
    if val.get("type").and_then(Value::as_str) == Some("file") {
        let file = &val["file"];
        if let Some((media_type, data)) = file["file_data"]
            .as_str()
            .and_then(|url| url.strip_prefix("data:"))
            .and_then(|rest| rest.split_once(";base64,"))
        {
            let mut block = serde_json::json!({
                "type": "document",
                "source": {"type": "base64", "media_type": media_type, "data": data},
            });
            if let Some(name) = file["filename"].as_str().filter(|name| !name.is_empty()) {
                block["title"] = serde_json::json!(name);
            }
            return block;
        }
    }
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

impl AnthropicProvider {
    /// The request body for `req`, with the options of the provider it goes to.
    pub fn translate_request_with(
        &self,
        req: &ChatRequest,
        options: &ProviderOptions,
    ) -> Result<Value, ProviderError> {
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
                // Signed thinking goes back first and unchanged, to the model that wrote it
                // alone, and only where the operator has checked the upstream takes it.
                if let Some(thinking) = m
                    .thinking
                    .as_ref()
                    .filter(|thinking| options.replay_thinking && thinking.model == req.model)
                {
                    blocks.push(serde_json::json!({
                        "type": "thinking",
                        "thinking": thinking.text,
                        "signature": thinking.signature,
                    }));
                }
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
        // A request ending on the assistant's turn is a prefill, which current models
        // refuse. It happens when the user's turn carried nothing that could be sent.
        if messages
            .last()
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
        {
            messages.push(serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "(continue)"}]
            }));
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

        let family = super::family::family(&req.model);
        let mut thinking = false;
        match (family.reasoning, req.reasoning_effort) {
            (
                Reasoning::Adaptive {
                    xhigh, summarized, ..
                },
                Some(effort),
            ) => {
                body["thinking"] = adaptive_thinking(summarized);
                body["output_config"] =
                    serde_json::json!({"effort": ModelFamily::adaptive_effort(xhigh, effort)});
                thinking = true;
            }
            // It thinks anyway, and streams an empty text while it does unless it is asked
            // for a summary: a long silent pause, then the answer.
            (
                Reasoning::Adaptive {
                    summarized: true,
                    by_default: true,
                    ..
                },
                None,
            ) => {
                body["thinking"] = adaptive_thinking(true);
                thinking = true;
            }
            (Reasoning::Adaptive { .. }, None) | (_, None) => {}
            // Older Claude models, and any this format carries that the table does not know,
            // think within a budget that leaves a quarter of the output for the answer.
            (_, Some(effort)) => {
                use kiro_wire::requests::conversation::ReasoningEffort::*;
                let wanted = match effort {
                    Low => 1024,
                    Medium => 4096,
                    High => 8192,
                    Xhigh => 16384,
                    Max => 24576,
                };
                let budget = wanted.min(max_tokens.saturating_sub((max_tokens / 4).max(1024)));
                if budget < 1024 {
                    return Err(ProviderError::Serialization(
                        "Thinking needs a max output of at least 2048 tokens".into(),
                    ));
                }
                body["thinking"] = serde_json::json!({"type": "enabled", "budget_tokens": budget});
                thinking = true;
            }
        }
        // Thinking takes no sampling parameters, and current models take none at all.
        if let Some(temp) = req.temperature.filter(|_| family.sampling && !thinking) {
            body["temperature"] = serde_json::json!(temp);
        }

        if !req.tools.is_empty() {
            let mut normalized_tools: Vec<Value> = req
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
                        // Kiro wraps each schema as {"json": {...}}. Passed on wrapped, a model
                        // sees no parameters and guesses its own (fs_write called with
                        // file_path/content instead of path/text).
                        let input_schema = match input_schema.get("json") {
                            Some(inner) if inner.is_object() => inner.clone(),
                            _ => input_schema,
                        };
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
            // Streamed as the model writes them, a large tool input (a file being written)
            // is not held back in one silent block. Off unless the operator turns it on:
            // some relays refuse the field.
            if options.eager_tool_input {
                for tool in normalized_tools.iter_mut().filter(|tool| tool.is_object()) {
                    tool["eager_input_streaming"] = serde_json::json!(true);
                }
            }
            body["tools"] = serde_json::json!(normalized_tools);
        }

        if options.prompt_cache {
            add_cache_breakpoints(&mut body);
        }

        Ok(body)
    }
}

impl ModelProvider for AnthropicProvider {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn endpoint_url(&self, base_url: &str) -> String {
        endpoint_with_suffix(base_url, "/v1", "/messages")
    }

    fn translate_request(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        self.translate_request_with(req, &super::current_provider_options())
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
                events.push(ProviderStreamEvent::Started);
                if let Some(msg) = val.get("message") {
                    if let Some(mut usage) = self.extract_usage(msg) {
                        usage.output_tokens_final = false;
                        events.push(ProviderStreamEvent::Usage(usage));
                    }
                }
            }
            "content_block_start" => {
                let index = val
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .map(|i| i as usize);
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
                    } else {
                        events.push(ProviderStreamEvent::Started);
                    }
                }
            }
            "content_block_delta" => {
                let index = val
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .map(|i| i as usize);
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
                        "signature_delta" => {
                            if let Some(signature) = delta
                                .get("signature")
                                .and_then(|s| s.as_str())
                                .filter(|s| !s.is_empty())
                            {
                                events.push(ProviderStreamEvent::Delta(
                                    ProviderDelta::ReasoningSignature(signature.to_string()),
                                ));
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
                    if let Some(details) = delta
                        .get("stop_details")
                        .or_else(|| val.get("stop_details"))
                        .filter(|details| details.is_object())
                    {
                        let field = |name: &str| details[name].as_str().map(ToString::to_string);
                        events.push(ProviderStreamEvent::Refusal {
                            category: field("category"),
                            explanation: field("explanation"),
                        });
                    }
                }
                if let Some(mut usage) = self.extract_usage(&val) {
                    // These counts are cumulative. A relay that learns the real prompt
                    // counts only when the reply ends sends an estimate at message_start
                    // and bills what it restates here, so a restatement replaces the
                    // estimate. A report of zeros is a placeholder, not a count.
                    usage.prompt_final =
                        val["usage"].get("input_tokens").is_some_and(Value::is_u64)
                            && usage.prompt_tokens > 0;
                    if !usage.prompt_final {
                        usage = TokenUsage {
                            completion_tokens: usage.completion_tokens,
                            total_tokens: usage.completion_tokens,
                            output_tokens_final: usage.output_tokens_final,
                            ..TokenUsage::default()
                        };
                    }
                    events.push(ProviderStreamEvent::Usage(usage));
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
            prompt_final: false,
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
        self.chat_stream_with(client, config, request, super::current_provider_options())
    }
}

impl AnthropicProvider {
    /// The answer to `request`, with the options of the provider it goes to. Preserved
    /// thinking binds each replayed block to the conversation it was written in: once Kiro's
    /// compaction or the gateway has rebuilt an earlier part of it differently, the upstream
    /// refuses the request (400, "Invalid `signature` in `thinking` block"), and the attempt
    /// goes once more without any thinking in its history.
    pub fn chat_stream_with<'a>(
        &'a self,
        client: &'a reqwest::Client,
        config: &'a ProviderConfig,
        request: &'a ChatRequest,
        options: ProviderOptions,
    ) -> BoxFuture<
        'a,
        Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError>,
    > {
        Box::pin(async move {
            let body = self.translate_request_with(request, &options)?;
            match self.send(client, config, request, &body).await {
                Err(ProviderError::Http(status, text))
                    if status == reqwest::StatusCode::BAD_REQUEST
                        && carries_thinking(&body)
                        && names_thinking(&text) =>
                {
                    // No body or content is logged.
                    eprintln!(
                        "upstream_thinking_refused model={} retry=without_thinking",
                        config.model
                    );
                    let options = ProviderOptions {
                        replay_thinking: false,
                        ..options
                    };
                    let body = self.translate_request_with(request, &options)?;
                    self.send(client, config, request, &body).await
                }
                result => result,
            }
        })
    }

    /// Sends `body` for `request`: the stream of the answer, or the upstream's refusal.
    async fn send(
        &self,
        client: &reqwest::Client,
        config: &ProviderConfig,
        request: &ChatRequest,
        body: &Value,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, ProviderError>>, ProviderError> {
        let url = self.endpoint_url(&config.base_url);
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&config.api_key)
                .map_err(|e| ProviderError::Serialization(e.to_string()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

        // Bound connection and response-header wait separately from the
        // long-lived streaming body timeout: longer for a model that reasons first.
        // A relay that gives no headers in time did not answer at all.
        let resp = tokio::time::timeout(
            super::retry::limits_for(request)
                .headers
                .min(config.timeout),
            client
                .post(&url)
                .headers(headers)
                .json(body)
                .timeout(config.timeout)
                .send(),
        )
        .await
        .map_err(|_| ProviderError::NoAnswer)?
        .map_err(|e| {
            if e.is_timeout() {
                ProviderError::NoAnswer
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
        Ok(process_byte_stream(byte_stream, |line| {
            AnthropicProvider.parse_stream_line(line)
        }))
    }
}

/// Whether a request body sends thinking back in its history.
fn carries_thinking(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|messages| {
        messages
            .iter()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .any(|block| {
                matches!(
                    block["type"].as_str(),
                    Some("thinking" | "redacted_thinking")
                )
            })
    })
}

/// Whether an upstream's refusal is about a thinking block or its signature. Only that is
/// read from the upstream's words.
fn names_thinking(refusal: &str) -> bool {
    let refusal = refusal.to_ascii_lowercase();
    refusal.contains("thinking") || refusal.contains("signature")
}
