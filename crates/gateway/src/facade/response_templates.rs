//! Opt-in, disclosed local file templates. No provider request or fabricated token usage.
use super::{error_response, Response};
use axum::body::{Body, Bytes};
use axum::{
    http::{header, StatusCode},
    response::IntoResponse,
};
use billing::engine::{
    BillingEngine, BillingError, ResponseTemplateReceipt, ResponseTemplateRule,
    ResponseTemplateVariant,
};
use futures_util::StreamExt;
use kiro_wire::{
    encoder::encode_event,
    events::{AssistantResponseEvent, MetadataEvent, TokenUsage, ToolUseEvent},
    requests::{conversation::GenerateAssistantResponseRequest, tool::Tool},
};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone)]
struct DeliveryCommitted(Arc<AtomicBool>);

/// The HTTP headers are not completion. Keep both guards alive until the body
/// completes or is cancelled. A debit has its own durable replay record.
pub(super) fn protect_response(
    response: Response,
    guard: crate::idempotency::IdempotencyGuard,
    capacity: crate::guardrail::CapacityPermit,
    model: String,
) -> Response {
    if !response.status().is_success() {
        return response;
    }
    let committed = response.extensions().get::<DeliveryCommitted>().cloned();
    let (parts, body) = response.into_parts();
    let state = (
        body.into_data_stream(),
        Some(guard),
        capacity,
        model,
        committed,
    );
    let stream = futures_util::stream::unfold(
        state,
        |(mut body, mut guard, capacity, model, committed)| async move {
            match body.next().await {
                Some(chunk) => Some((chunk, (body, guard, capacity, model, committed))),
                None => {
                    if committed
                        .as_ref()
                        .is_none_or(|c| c.0.load(Ordering::Acquire))
                    {
                        if let Some(g) = guard.take() {
                            g.commit(crate::idempotency::CompletedInvocation {
                                completed_at: std::time::Instant::now(),
                                model_id: model,
                                total_input_tokens: 0,
                                total_output_tokens: 0,
                            });
                        }
                    }
                    None
                }
            }
        },
    );
    Response::from_parts(parts, Body::from_stream(stream))
}

fn render_message(text: &str, path: &str, price: i64) -> String {
    // Only named text substitutions, never expressions or recursive templates.
    text.split("{file_path}")
        .map(|s| s.replace("{price}", &format!("{:.6}", price as f64 / 1_000_000.0)))
        .collect::<Vec<_>>()
        .join(path)
}
fn random_delay(min: u32, max: u32) -> Result<u32, ring::error::Unspecified> {
    let width = u64::from(max - min) + 1;
    let limit = (u64::from(u32::MAX) + 1) / width * width;
    loop {
        let mut b = [0u8; 4];
        SystemRandom::new().fill(&mut b)?;
        let n = u64::from(u32::from_le_bytes(b));
        if n < limit {
            return Ok(min + (n % width) as u32);
        }
    }
}

fn choose_index(selected: Option<u8>, count: usize) -> Result<usize, ring::error::Unspecified> {
    debug_assert!(count > 0);
    if let Some(index) = selected.map(usize::from) {
        return (index < count)
            .then_some(index)
            .ok_or(ring::error::Unspecified);
    }
    if count == 1 {
        Ok(0)
    } else {
        random_delay(0, (count - 1) as u32).map(|index| index as usize)
    }
}

fn selected_content(
    variant: &ResponseTemplateVariant,
) -> Result<(&str, &str), ring::error::Unspecified> {
    let index = choose_index(
        variant.content_alternative_index,
        1 + variant.content_alternatives.len(),
    )?;
    if index == 0 {
        Ok((&variant.file_path, &variant.content))
    } else {
        let alternative = &variant.content_alternatives[index - 1];
        Ok((&alternative.file_path, &alternative.content))
    }
}

fn selected_message(
    message: &billing::engine::ResponseTemplateMessage,
) -> Result<(u8, &str), ring::error::Unspecified> {
    let index = choose_index(message.selected_index, 1 + message.alternatives.len())?;
    if index == 0 {
        Ok((0, &message.text))
    } else {
        Ok((index as u8, &message.alternatives[index - 1]))
    }
}

/// Only the current user turn can match; historical prompts and tool output cannot retrigger it.
fn matching_variant<'a>(
    rules: &'a [ResponseTemplateRule],
    prompt: &str,
    model: &str,
) -> Option<(&'a ResponseTemplateRule, &'a ResponseTemplateVariant)> {
    rules.iter().find_map(|rule| {
        if !billing::engine::preview_template_match(rule, prompt, model).matched {
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
        "fsWrite" | "fs_write" => Some(("path", "text")),
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
        BillingError::ConcurrencyLimitExceeded { .. } => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "ThrottlingException",
            "模板请求超过卡并发限制，本次未执行且未扣费。",
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
        &receipt
            .delivery
            .as_ref()
            .map(|d| render_message(&d.replay, &receipt.file_path, receipt.price_microcredits))
            .unwrap_or_else(|| {
                "[固定模板服务恢复；本次请求已扣费，重放原文件写入指令，不重复扣费]".into()
            }),
        Some((&receipt.tool_name, &receipt.tool_use_id, &input)),
        0,
    )
    .ok()?;
    Some(stream(bytes))
}

/// None preserves the ordinary model pipeline. A matched but unsupported tool request
/// returns a clear, free refusal instead of silently charging a model fallback.
pub(super) async fn respond(
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
        let mut text = String::new();
        for (result, receipt) in &receipts {
            if let Some(d) = &receipt.delivery {
                let message = match result.status.as_deref() {
                    Some("success") => &d.success,
                    Some("error") => &d.failure,
                    _ => &d.unknown,
                };
                text.push_str(&render_message(
                    message,
                    &receipt.file_path,
                    receipt.price_microcredits,
                ));
                text.push('\n');
            } else {
                text.push_str("[固定模板回执；本轮不调用上游、不再扣费]\n");
                match result.status.as_deref() {
                    Some("success") => text.push_str(&format!("客户端报告已写入 {}。{}\n", receipt.file_path, receipt.completion)),
                    Some("error") => text.push_str(&format!("客户端报告写入 {} 失败。请检查客户端工具返回的错误；不会自动重试或再次扣费。\n", receipt.file_path)),
                    _ => text.push_str(&format!("已收到 {} 的工具回执，但未确认写入成功。请检查客户端文件。\n", receipt.file_path)),
                }
            }
        }
        if !user.content.trim().is_empty() || !user.images.is_empty() || !user.documents.is_empty()
        {
            let receipt = &receipts[0].1;
            text.push_str(&receipt.delivery.as_ref().map(|d| render_message(&d.continuation, &receipt.file_path, receipt.price_microcredits)).unwrap_or_else(|| "本轮仅确认模板工具回执，附带的新指令或附件未执行；如需继续处理，请另发一条消息。\n".into()));
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
    let (file_path, content) = match selected_content(variant) {
        Ok(content) if !content.1.trim().is_empty() => content,
        Ok(_) => {
            return Some(error_response(
                StatusCode::BAD_REQUEST,
                "ValidationException",
                "命中的历史模板内容为空，请联系管理员修正；本次未调用上游或扣费。",
            ));
        }
        Err(_) => return Some(encoding_error()),
    };
    let tools = context.map(|ctx| ctx.tools.as_slice()).unwrap_or_default();
    let Some((tool_name, input)) = file_tool(tools, file_path, content) else {
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
    let legacy_text = format!(
        "[固定模板服务，非模型实时生成；不调用上游，token 用量为 0；本次写入指令收费 {:.6} 积分，回执不重复收费]\n{}\n准备向客户端发送 {} 的文件写入指令，尚未确认文件已创建。",
        variant.price_microcredits as f64 / 1_000_000.0,
        variant.preamble,
        file_path
    );
    let text = variant
        .delivery
        .as_ref()
        .map(|d| render_message(&d.dispatch, file_path, variant.price_microcredits))
        .unwrap_or(legacy_text);
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
    // This process-local slot shares the card limit with ordinary reservations.
    // Keep it alive through the final debit; cancellation releases it without billing.
    let _wait_slot = match billing.begin_response_template_wait(
        &config.revision,
        &rule.id,
        model,
        card_id,
        invocation_id,
        crate::now_secs(),
    ) {
        Ok(slot) => slot,
        Err(error) => return Some(charge_error(error)),
    };
    if let Some(delivery) = &variant.delivery {
        let write_at = match random_delay(delivery.write_min_ms, delivery.write_max_ms) {
            Ok(at) => at,
            Err(_) => return Some(encoding_error()),
        };
        let mut events = Vec::new();
        let mut message_selections = Vec::with_capacity(delivery.messages.len());
        for m in &delivery.messages {
            let (message_index, message) = match selected_message(m) {
                Ok(message) => message,
                Err(_) => return Some(encoding_error()),
            };
            message_selections.push(message_index);
            let text = render_message(message, file_path, variant.price_microcredits);
            let event =
                match encode_event("assistantResponseEvent", &AssistantResponseEvent::new(text)) {
                    Ok(b) => b,
                    Err(_) => return Some(encoding_error()),
                };
            let message_at = match random_delay(m.at_ms, m.at_max_ms.unwrap_or(m.at_ms)) {
                Ok(at) => at,
                Err(_) => return Some(encoding_error()),
            };
            events.push((message_at, event, false));
        }
        // Independent protocol keepalives also cover empty/sparse display timelines.
        // Snapshot the validated runtime interval for this request. These bounded,
        // non-billable events use the same lazy body and cancellation guards as the
        // file dispatch; no background task can outlive a disconnected client.
        let keepalive_ms = billing.runtime_settings_config().settings.keepalive_secs * 1_000;
        let mut at = keepalive_ms;
        while at < u64::from(write_at) {
            events.push((at as u32, kiro_wire::encoder::encode_keepalive(), false));
            at += keepalive_ms;
        }
        events.push((write_at, bytes, true));
        events.sort_by_key(|(at, _, _)| *at);
        let receipt = ResponseTemplateReceipt {
            tool_use_id: tool_id,
            invocation_id: invocation_id.into(),
            card_id: card_id.into(),
            conversation_id: state.conversation_id.clone(),
            model_id: model.into(),
            file_path: file_path.to_owned(),
            completion: variant.completion.clone(),
            tool_name: tool_name.clone(),
            path_key: tool_keys(&tool_name).unwrap().0.into(),
            content_key: tool_keys(&tool_name).unwrap().1.into(),
            content: content.to_owned(),
            price_microcredits: variant.price_microcredits,
            delivery: variant.delivery.clone(),
            message_selections,
            created_at_secs: 0,
        };
        let committed = DeliveryCommitted(Arc::new(AtomicBool::new(false)));
        let body_state = (
            events.into_iter(),
            tokio::time::Instant::now(),
            billing.clone(),
            config.revision.clone(),
            rule.id.clone(),
            receipt,
            _wait_slot,
            committed.clone(),
        );
        let body = futures_util::stream::unfold(
            body_state,
            |(mut events, start, billing, revision, rule, mut receipt, slot, committed)| async move {
                let (at, mut bytes, debit) = events.next()?;
                tokio::time::sleep_until(start + std::time::Duration::from_millis(at as u64)).await;
                if debit {
                    let now = crate::now_secs();
                    receipt.created_at_secs = now;
                    match billing.charge_response_template(
                        &revision,
                        &rule,
                        &receipt.model_id,
                        receipt.clone(),
                        now,
                    ) {
                        Ok(entry) => {
                            committed.0.store(true, Ordering::Release);
                            billing.record_trace(billing::observability::RequestTrace {
                                id: format!("template-{}", receipt.invocation_id),
                                card_id: receipt.card_id.clone(),
                                ts: now,
                                invocation_id: receipt.invocation_id.clone(),
                                exposed_model: receipt.model_id.clone(),
                                status: billing::observability::TraceStatus::Success,
                                provider_id: Some(entry.provider_id),
                                credits_charged: entry.credits_charged,
                                ..Default::default()
                            });
                        }
                        Err(_) => {
                            bytes = kiro_wire::encoder::encode_exception(
                                "ValidationException",
                                "模板配置、卡状态或余额已变化，未下发文件且未扣费。请重新请求。",
                            );
                        }
                    }
                }
                Some((
                    Ok::<Bytes, std::convert::Infallible>(Bytes::from(bytes)),
                    (
                        events, start, billing, revision, rule, receipt, slot, committed,
                    ),
                ))
            },
        );
        let mut response = (
            [
                (header::CONTENT_TYPE, "application/vnd.amazon.eventstream"),
                (header::CACHE_CONTROL, "no-store"),
                (
                    axum::http::HeaderName::from_static("x-accel-buffering"),
                    "no",
                ),
            ],
            Body::from_stream(body),
        )
            .into_response();
        response.extensions_mut().insert(committed);
        return Some(response);
    }
    // Waiting is cancellable and precedes the durable debit. Invalid tool schemas never wait.
    if variant.delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(variant.delay_ms as u64)).await;
    }
    let now = crate::now_secs();
    let receipt = ResponseTemplateReceipt {
        tool_use_id: tool_id,
        invocation_id: invocation_id.into(),
        card_id: card_id.into(),
        conversation_id: state.conversation_id.clone(),
        model_id: model.into(),
        file_path: file_path.to_owned(),
        completion: variant.completion.clone(),
        tool_name: tool_name.clone(),
        path_key: tool_keys(&tool_name).expect("validated file tool").0.into(),
        content_key: tool_keys(&tool_name).expect("validated file tool").1.into(),
        content: content.to_owned(),
        price_microcredits: variant.price_microcredits,
        delivery: None,
        message_selections: vec![],
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
            delay_ms: 0,
            delivery: None,
            model_id: model.into(),
            file_path: "index.html".into(),
            content: format!("<svg>{model}</svg>"),
            content_alternatives: vec![],
            content_alternative_index: None,
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
            intent: None,
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
    fn current_kiro_snake_case_writer_preserves_text_for_replay() {
        let tool = Tool::Direct(ToolSpecification::new("fs_write", "Write a file", schema()));
        let (name, input) = file_tool(&[tool], "index.html", "<svg>中文</svg>").unwrap();
        assert_eq!(name, "fs_write");
        assert_eq!(input, json!({"path":"index.html","text":"<svg>中文</svg>"}));
        assert_eq!(tool_keys(&name), Some(("path", "text")));
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
