//! Opt-in, disclosed local file templates. No provider request or fabricated token usage.
use super::{error_response, Response};
use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};
use billing::engine::{
    BillingEngine, BillingError, ResponseTemplateReceipt, ResponseTemplateRule,
    ResponseTemplateVariant,
};
use kiro_wire::{
    encoder::encode_event,
    events::{AssistantResponseEvent, MetadataEvent, TokenUsage, ToolUseEvent},
    requests::{conversation::GenerateAssistantResponseRequest, tool::Tool},
};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};

/// Only the current user turn can match; historical prompts and tool output cannot retrigger it.
fn matching_variant<'a>(
    rules: &'a [ResponseTemplateRule],
    prompt: &str,
    model: &str,
) -> Option<(&'a ResponseTemplateRule, &'a ResponseTemplateVariant)> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return None;
    }
    rules.iter().filter(|rule| rule.enabled).find_map(|rule| {
        let matched = match rule.match_mode.as_str() {
            "exact" => prompt == rule.match_text.trim(),
            "contains" => prompt.contains(rule.match_text.trim()),
            _ => false,
        };
        if !matched {
            return None;
        }
        rule.variants
            .iter()
            .find(|v| v.model_id == model)
            .map(|v| (rule, v))
    })
}

/// Refuse schemas whose constraints we cannot check. Supporting a known tool name alone
/// is insufficient: versions of a client can use different required arguments.
fn string_fits(schema: &Value, value: &str) -> bool {
    let Some(s) = schema.as_object() else {
        return false;
    };
    if s.get("type").and_then(Value::as_str) != Some("string") {
        return false;
    }
    if s.keys().any(|k| {
        !matches!(
            k.as_str(),
            "type"
                | "description"
                | "title"
                | "default"
                | "examples"
                | "enum"
                | "const"
                | "minLength"
                | "maxLength"
        )
    }) {
        return false;
    }
    let length = value.chars().count() as u64;
    for (key, minimum) in [("minLength", true), ("maxLength", false)] {
        if let Some(bound) = s.get(key) {
            let Some(bound) = bound.as_u64() else {
                return false;
            };
            if (minimum && length < bound) || (!minimum && length > bound) {
                return false;
            }
        }
    }
    if s.get("const").is_some_and(|v| v.as_str() != Some(value)) {
        return false;
    }
    if let Some(choices) = s.get("enum") {
        if !choices
            .as_array()
            .is_some_and(|xs| xs.iter().any(|v| v.as_str() == Some(value)))
        {
            return false;
        }
    }
    true
}

fn tool_keys(name: &str) -> Option<(&'static str, &'static str)> {
    match name {
        "fsWrite" => Some(("path", "text")),
        "Write" => Some(("file_path", "content")),
        "write_file" | "writeFile" => Some(("path", "content")),
        _ => None,
    }
}

fn file_tool(tools: &[Tool], path: &str, content: &str) -> Option<(String, Value)> {
    tools.iter().find_map(|tool| {
        let (path_key, content_key) = tool_keys(tool.name())?;
        let raw = tool.input_schema();
        let schema = raw.get("json").unwrap_or(raw).as_object()?;
        if schema.get("type")?.as_str()? != "object"
            || schema.keys().any(|k| {
                !matches!(
                    k.as_str(),
                    "type"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "description"
                        | "title"
                        | "$schema"
                )
            })
        {
            return None;
        }
        let properties = schema.get("properties")?.as_object()?;
        if !string_fits(properties.get(path_key)?, path)
            || !string_fits(properties.get(content_key)?, content)
        {
            return None;
        }
        if let Some(required) = schema.get("required") {
            if required.as_array()?.iter().any(|name| {
                !name
                    .as_str()
                    .is_some_and(|name| name == path_key || name == content_key)
            }) {
                return None;
            }
        }
        Some((
            tool.name().to_owned(),
            json!({path_key: path, content_key: content}),
        ))
    })
}

fn frames(
    conversation: &str,
    model: &str,
    text: &str,
    tool: Option<(&str, &str, &Value)>,
    price: i64,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut result = encode_event(
        "messageMetadataEvent",
        &json!({"conversationId":conversation}),
    )?;
    result.extend(encode_event(
        "assistantResponseEvent",
        &AssistantResponseEvent::new(text).with_model_id(model),
    )?);
    if let Some((name, id, input)) = tool {
        result.extend(encode_event(
            "toolUseEvent",
            &ToolUseEvent::new(name, id, serde_json::to_string(input)?, true),
        )?);
    }
    result.extend(encode_event(
        "meteringEvent",
        &json!({"usage":price as f64 / 1_000_000.0,"unit":"credit","unitPlural":"credits"}),
    )?);
    result.extend(encode_event(
        "metadataEvent",
        &MetadataEvent::new(
            TokenUsage::default(),
            Some(
                if tool.is_some() {
                    "tool_use"
                } else {
                    "end_turn"
                }
                .into(),
            ),
        ),
    )?);
    Ok(result)
}

fn stream(bytes: Vec<u8>) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")],
        bytes,
    )
        .into_response()
}
fn encoding_error() -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerException",
        "模板响应编码失败，本次未扣费。",
    )
}
fn charge_error(error: BillingError) -> Response {
    match error {
        BillingError::Persistence(_) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "InternalServerException",
            "模板扣费未能持久保存，本次未发送写入指令。",
        ),
        BillingError::DuplicateInvocation(_) => error_response(
            StatusCode::CONFLICT,
            "InvocationAlreadyCompletedException",
            "该请求已处理，不会重复扣费或重新发送写入指令。",
        ),
        _ => error_response(
            StatusCode::BAD_REQUEST,
            "ValidationException",
            &format!("模板请求未执行且未扣费：{error}"),
        ),
    }
}

pub(super) fn replay_receipt(
    billing: &BillingEngine,
    request: &GenerateAssistantResponseRequest,
    card_id: &str,
    invocation_id: &str,
    model: &str,
    now: u64,
) -> Option<Response> {
    let receipt = billing.response_template_receipt_for_invocation(card_id, invocation_id, now)?;
    let state = &request.conversation_state;
    let user = &state.current_message.user_input_message;
    let permitted = billing.get_card(card_id).is_some_and(|card| {
        card.check_active(now).is_ok()
            && billing.get_group(&card.group_id).is_some()
            && billing
                .list_models_for_group(&card.group_id, false)
                .iter()
                .any(|m| !m.retired && m.matches_model(model))
    });
    let matching_tool = user.user_input_message_context.as_ref().and_then(|ctx| {
        let tools: Vec<_> = ctx
            .tools
            .iter()
            .filter(|t| t.name() == receipt.tool_name)
            .cloned()
            .collect();
        file_tool(&tools, &receipt.file_path, &receipt.content)
    });
    if !permitted
        || !billing.persistence_ready()
        || receipt.conversation_id != state.conversation_id
        || receipt.model_id != model
        || matching_tool.is_none()
        || !user.images.is_empty()
        || !user.documents.is_empty()
    {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "ValidationException",
            "原模板请求无法在当前卡状态、会话、模型或工具条件下恢复；本轮未扣费。",
        ));
    }
    let (path_key, content_key) = (receipt.path_key.as_str(), receipt.content_key.as_str());
    let input = json!({path_key: receipt.file_path, content_key: receipt.content});
    let bytes = frames(
        &receipt.conversation_id,
        &receipt.model_id,
        "[固定模板服务恢复；本次请求已扣费，重放原文件写入指令，不重复扣费]",
        Some((&receipt.tool_name, &receipt.tool_use_id, &input)),
        0,
    )
    .ok()?;
    Some(stream(bytes))
}

/// None preserves the ordinary model pipeline. A matched but unsupported tool request
/// returns a clear, free refusal instead of silently charging a model fallback.
pub(super) fn respond(
    billing: &BillingEngine,
    request: &GenerateAssistantResponseRequest,
    card_id: &str,
    invocation_id: &str,
    model: &str,
    now: u64,
) -> Option<Response> {
    let state = &request.conversation_state;
    let user = &state.current_message.user_input_message;
    let context = user.user_input_message_context.as_ref();
    if let Some(context) = context.filter(|ctx| !ctx.tool_results.is_empty()) {
        // Kiro can attach continuation text to a tool result. A known template
        // receipt must never become another billable model invocation because of it.
        let receipts: Vec<_> = context
            .tool_results
            .iter()
            .filter_map(|result| {
                billing
                    .response_template_receipt_any(
                        card_id,
                        &state.conversation_id,
                        &result.tool_use_id,
                    )
                    .map(|receipt| (result, receipt))
            })
            .collect();
        // Template tool IDs occupy a reserved namespace. Even after receipt-body
        // expiry/pruning, never turn a late or incorrectly scoped result into paid upstream work.
        if context.tool_results.iter().any(|result| {
            result.tool_use_id.starts_with("template_")
                && !receipts
                    .iter()
                    .any(|(known, _)| known.tool_use_id == result.tool_use_id)
        }) {
            return Some(error_response(
                StatusCode::BAD_REQUEST,
                "ValidationException",
                "模板回执已过期、不存在或不属于当前会话；本轮未调用上游或扣费。",
            ));
        }
        if receipts.is_empty() {
            return None;
        }
        if receipts
            .iter()
            .any(|(_, receipt)| now.saturating_sub(receipt.created_at_secs) >= 86_400)
        {
            return Some(error_response(
                StatusCode::BAD_REQUEST,
                "ValidationException",
                "模板回执已过期，本轮未调用上游或扣费；请重新发送原始请求。",
            ));
        }
        if receipts.len() != context.tool_results.len() {
            return Some(error_response(
                StatusCode::BAD_REQUEST,
                "ValidationException",
                "模板回执混有其他工具结果，本轮未调用上游或扣费。请单独发送后续请求。",
            ));
        }
        let mut text = String::from("[固定模板回执；本轮不调用上游、不再扣费]\n");
        for (result, receipt) in &receipts {
            match result.status.as_deref() {
                Some("success") => text.push_str(&format!("客户端报告已写入 {}。{}\n", receipt.file_path, receipt.completion)),
                Some("error") => text.push_str(&format!("客户端报告写入 {} 失败。请检查客户端工具返回的错误；不会自动重试或再次扣费。\n", receipt.file_path)),
                _ => text.push_str(&format!("已收到 {} 的工具回执，但未确认写入成功。请检查客户端文件。\n", receipt.file_path)),
            }
        }
        if !user.content.trim().is_empty() || !user.images.is_empty() || !user.documents.is_empty()
        {
            text.push_str("本轮仅确认模板工具回执，附带的新指令或附件未执行；如需继续处理，请另发一条消息。\n");
        }
        let receipt_model = &receipts[0].1.model_id;
        return Some(
            match frames(&state.conversation_id, receipt_model, &text, None, 0) {
                Ok(bytes) => stream(bytes),
                Err(_) => encoding_error(),
            },
        );
    }
    if !user.images.is_empty() || !user.documents.is_empty() {
        return None;
    }
    // A process restart clears in-memory idempotency, so durable receipts are the replay record.
    if billing
        .response_template_receipt_for_invocation(card_id, invocation_id, now)
        .is_some()
    {
        return replay_receipt(billing, request, card_id, invocation_id, model, now);
    }
    let config = billing.response_template_config();
    let (rule, variant) = matching_variant(&config.rules, &user.content, model)?;
    let tools = context.map(|ctx| ctx.tools.as_slice()).unwrap_or_default();
    let Some((tool_name, input)) = file_tool(tools, &variant.file_path, &variant.content) else {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            "ValidationException",
            "命中固定模板，但客户端未提供兼容的文件写入工具或参数结构，本次未扣费。",
        ));
    };
    let mut random = [0u8; 16];
    if SystemRandom::new().fill(&mut random).is_err() {
        return Some(encoding_error());
    }
    let tool_id = format!(
        "template_{}",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let text = format!("[固定模板服务，非模型实时生成；不调用上游，token 用量为 0；本次写入指令收费 {:.6} 积分，回执不重复收费]\n{}\n准备向客户端发送 {} 的文件写入指令，尚未确认文件已创建。", variant.price_microcredits as f64 / 1_000_000.0, variant.preamble, variant.file_path);
    // Encode everything before the durable debit; even a response construction error is free.
    let bytes = match frames(
        &state.conversation_id,
        model,
        &text,
        Some((&tool_name, &tool_id, &input)),
        variant.price_microcredits,
    ) {
        Ok(bytes) => bytes,
        Err(_) => return Some(encoding_error()),
    };
    let receipt = ResponseTemplateReceipt {
        tool_use_id: tool_id,
        invocation_id: invocation_id.into(),
        card_id: card_id.into(),
        conversation_id: state.conversation_id.clone(),
        model_id: model.into(),
        file_path: variant.file_path.clone(),
        completion: variant.completion.clone(),
        tool_name: tool_name.clone(),
        path_key: if tool_name == "Write" {
            "file_path"
        } else {
            "path"
        }
        .into(),
        content_key: if tool_name == "fsWrite" {
            "text"
        } else {
            "content"
        }
        .into(),
        content: variant.content.clone(),
        price_microcredits: variant.price_microcredits,
        created_at_secs: now,
    };
    let entry =
        match billing.charge_response_template(&config.revision, &rule.id, model, receipt, now) {
            Ok(entry) => entry,
            Err(error) => return Some(charge_error(error)),
        };
    billing.record_trace(billing::observability::RequestTrace {
        id: format!("template-{invocation_id}"),
        card_id: card_id.into(),
        ts: now,
        invocation_id: invocation_id.into(),
        exposed_model: model.into(),
        status: billing::observability::TraceStatus::Success,
        provider_id: Some(entry.provider_id),
        credits_charged: entry.credits_charged,
        ..Default::default()
    });
    Some(stream(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiro_wire::requests::tool::ToolSpecification;
    fn variant(model: &str) -> ResponseTemplateVariant {
        ResponseTemplateVariant {
            model_id: model.into(),
            file_path: "index.html".into(),
            content: format!("<svg>{model}</svg>"),
            preamble: String::new(),
            completion: String::new(),
            price_microcredits: 123_456,
        }
    }
    fn rule(model: &str) -> ResponseTemplateRule {
        ResponseTemplateRule {
            id: model.into(),
            name: model.into(),
            enabled: true,
            match_mode: "exact".into(),
            match_text: "create pelican".into(),
            variants: vec![variant(model)],
        }
    }
    fn tool(schema: Value) -> Tool {
        Tool::Direct(ToolSpecification::new("fsWrite", "Write a file", schema))
    }
    fn schema() -> Value {
        json!({"json":{"type":"object","properties":{"path":{"type":"string"},"text":{"type":"string"}},"required":["path","text"]}})
    }
    #[test]
    fn exact_model_and_rule_priority() {
        let mut first = rule("gpt");
        first.variants.push(variant("claude"));
        let rules = vec![first, rule("other")];
        assert_eq!(
            matching_variant(&rules, " create pelican ", "claude")
                .unwrap()
                .1
                .content,
            "<svg>claude</svg>"
        );
        assert!(matching_variant(&rules, "create pelican later", "claude").is_none());
        assert!(matching_variant(&rules, "create pelican", "unknown").is_none());
    }
    #[test]
    fn tool_schema_checked_and_arguments_escaped() {
        let content = "<svg>\"\n中文</svg>";
        let (_, input) = file_tool(&[tool(schema())], "index.html", content).unwrap();
        assert_eq!(input, json!({"path":"index.html","text":content}));
        let mut s = schema();
        s["json"]["required"] = json!(["path", "text", "root"]);
        assert!(file_tool(&[tool(s)], "index.html", content).is_none());
        let mut s = schema();
        s["json"]["properties"]["text"]["maxLength"] = json!(2);
        assert!(file_tool(&[tool(s)], "index.html", content).is_none());
        let mut s = schema();
        s["json"]["allOf"] = json!([]);
        assert!(file_tool(&[tool(s)], "index.html", content).is_none());
    }
    #[test]
    fn frames_have_real_tool_stop_and_zero_tokens() {
        let input = json!({"path":"index.html","text":"<svg/>"});
        let bytes = frames(
            "conversation",
            "gpt",
            "模板",
            Some(("fsWrite", "template_1", &input)),
            123_456,
        )
        .unwrap();
        let mut decoder = kiro_wire::EventStreamDecoder::new();
        decoder.feed(&bytes).unwrap();
        let mut payloads: Vec<Value> = Vec::new();
        while let Some(frame) = decoder.decode().unwrap() {
            payloads.push(serde_json::from_slice(&frame.payload).unwrap());
        }
        assert!(payloads
            .iter()
            .any(|p| p["name"] == "fsWrite" && p["stop"] == true));
        let last = payloads.last().unwrap();
        assert_eq!(last["stopReason"], "tool_use");
        assert_eq!(last["tokenUsage"]["outputTokens"], 0);
        assert_eq!(payloads[3]["usage"], 0.123456);
    }
}
