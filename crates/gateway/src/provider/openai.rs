//! OpenAI-compatible model provider implementation (Spec §15.2).

use super::{
    endpoint_with_suffix, process_byte_stream, sanitize_upstream_error_body, BoxFuture, BoxStream,
    ChatRequest, ModelProvider, ProviderConfig, ProviderDelta, ProviderError, ProviderStreamEvent,
    TokenUsage,
};
use futures_util::TryStreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde_json::Value;

pub struct OpenAiProvider;

/// The longest description an OpenAI function takes.
const MAX_FUNCTION_DESCRIPTION_LEN: usize = 1024;

/// What a failed tool call's result opens with, as the model reads it.
const TOOL_ERROR_MARKER: &str = "[工具调用失败 / tool call failed]";

/// The tools as OpenAI functions, and the documentation of those whose description is
/// longer than a function takes (several of Kiro's are). That documentation goes into the
/// system prompt, with a pointer left in the function; Anthropic takes descriptions whole,
/// so only this request moves them.
fn function_tools(tools: &[Value]) -> (Vec<Value>, Option<String>) {
    let mut relocated = Vec::new();
    let functions = tools
        .iter()
        .map(|t| {
            let spec = if let Some(s) = t.get("toolSpecification") {
                s
            } else {
                t
            };
            let name = spec.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let mut desc = spec
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            if desc.len() > MAX_FUNCTION_DESCRIPTION_LEN {
                relocated.push(format!("### Tool: {name}\n{desc}"));
                desc = format!("Documentation for {name} is provided in the system prompt.");
            }
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
    let docs = (!relocated.is_empty()).then(|| {
        format!(
            "\n\n## Extended Tool Documentation\n{}",
            relocated.join("\n\n")
        )
    });
    (functions, docs)
}

/// Appends `text` to the request's system prompt, or starts one with it.
fn append_to_system_prompt(messages: &mut Vec<Value>, text: &str) {
    match messages.iter_mut().find(|m| m["role"] == "system") {
        Some(system) => match &mut system["content"] {
            Value::String(prompt) => prompt.push_str(text),
            Value::Array(parts) => {
                parts.push(serde_json::json!({"type": "text", "text": text.trim_start()}))
            }
            content => *content = Value::String(text.trim_start().to_string()),
        },
        None => messages.insert(
            0,
            serde_json::json!({"role": "system", "content": text.trim_start()}),
        ),
    }
}

impl ModelProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn endpoint_url(&self, base_url: &str) -> String {
        endpoint_with_suffix(base_url, "/v1", "/chat/completions")
    }

    fn translate_request(&self, req: &ChatRequest) -> Result<Value, ProviderError> {
        let (tools, tool_docs) = function_tools(&req.tools);
        let mut messages: Vec<Value> = req
            .messages
            .iter()
            .enumerate()
            // After tool results the model continues on its own. Kiro's turn that carries
            // them has no text of its own, and sent as an empty user message some models
            // answered it ("your message seems to be empty").
            .filter(|(position, m)| {
                let empty = match &m.content {
                    Value::String(text) => text.is_empty(),
                    Value::Array(parts) => parts.is_empty(),
                    Value::Null => true,
                    _ => false,
                };
                let after_tool = position
                    .checked_sub(1)
                    .and_then(|previous| req.messages.get(previous))
                    .is_some_and(|previous| previous.role == "tool");
                !(m.role == "user" && empty && after_tool)
            })
            .map(|(_, m)| {
                let mut obj = serde_json::json!({
                    "role": m.role,
                });
                obj["content"] = match (&m.content, m.is_error) {
                    // This format has no error flag for a tool result: a failed call would
                    // read as a success unless its text said otherwise.
                    (Value::String(text), Some(true)) if m.role == "tool" => {
                        Value::String(format!("{TOOL_ERROR_MARKER}\n{text}"))
                    }
                    (content, _) => content.clone(),
                };
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
        if let Some(docs) = tool_docs {
            append_to_system_prompt(&mut messages, &docs);
        }

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
        // Reasoning models take no temperature and count their output, reasoning included,
        // in max_completion_tokens; a model the family table does not know is sent no
        // sampling parameters.
        let family = super::family::family(&req.model);
        if let Some(temp) = req
            .temperature
            .filter(|_| family.sampling && req.reasoning_effort.is_none())
        {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max_tokens) = req.max_tokens {
            let field = if family.max_completion_tokens() {
                "max_completion_tokens"
            } else {
                "max_tokens"
            };
            body[field] = serde_json::json!(max_tokens);
        }
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
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
                // The opening chunk names the role: the model has begun.
                if first.pointer("/delta/role").is_some() {
                    events.push(ProviderStreamEvent::Started);
                }
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
            prompt_final: false,
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
            // long-lived streaming body timeout: longer for a model that reasons first.
            let resp = tokio::time::timeout(
                super::retry::UpstreamLimits::for_request(request)
                    .headers
                    .min(config.timeout),
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
