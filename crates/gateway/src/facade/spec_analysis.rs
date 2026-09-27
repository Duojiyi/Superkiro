//! Kiro's spec "Analyze Requirements": the JSON-RPC tool `spec_disambiguation`, which Kiro
//! calls through InvokeMCPStream (the IDE at `POST /mcp/stream`, its agent by the target
//! `KiroRuntimeService.InvokeMCPStream`) and reads back as an event stream of JSON-RPC
//! messages, each a `message` event: progress notifications, one partial result per
//! question, then the call's result or error. Both of Kiro's readers take that stream.
//!
//! The gateway answers it with one model call on the group's fast model (`simple-task`),
//! made and billed as a conversation turn: the model reads the requirements document and
//! names the acceptance criteria that leave a decision open, each as a question with
//! answers to choose from. Kiro shows the questions, applies those it marks as settled by
//! the document, and hands the customer's answers to the agent to write into the
//! requirements. Before, the call met the 404 fallback and the analysis failed every time.

use super::{error_response, BoxFuture, FacadeHandler, Response};
use crate::auth::AuthClaims;
use axum::{
    body::{to_bytes, Body, Bytes},
    http::{header, HeaderValue, Method, Request, StatusCode},
};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};

/// The largest call read: the requirements document in its JSON-RPC envelope.
const MAX_CALL_BYTES: usize = 2 * 1024 * 1024;

/// The most questions one analysis asks, and answers one question offers.
const MAX_QUESTIONS: usize = 10;
const MAX_ANSWERS: usize = 4;

/// How often an analysis still waiting for the model tells Kiro it is there, with a
/// notification Kiro ignores.
const KEEPALIVE: Duration = Duration::from_secs(10);

/// JSON-RPC error codes: an unknown method or tool, bad arguments, and a failed analysis.
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const ANALYSIS_FAILED: i64 = -32000;

/// What the model is asked to do with the document.
const INSTRUCTIONS: &str = r#"You review a requirements document for a software feature before it is designed. Each requirement has a heading "Requirement N" and numbered acceptance criteria, usually in EARS form ("WHEN ... THE system SHALL ...").

Find the acceptance criteria that leave a decision open: a limit, a default, an error or edge case, an order of events, a permission or a rule that two careful engineers would build differently. Skip wording and style, and anything the document already decides.

For each, ask one question that settles it, with two to four answers, and give each answer's consequence for the system in one sentence. When one answer is clearly what the document intends, set "autoResolvable" to true and give that answer as "recommendedAnswer". Otherwise set "autoResolvable" to false and give the answer you would recommend as "recommendedAnswer", or "" if none stands out.

Name each criterion "N.M": N is the requirement's number from its heading, M the criterion's number in its list. A question about a whole requirement is named "N".

Write the questions, answers and consequences in the language of the document. Ask at most 10 questions, the most consequential first. If nothing is left open, ask none.

Reply with one JSON object and nothing else, with no code fence:
{"questions":[{"requirementId":"2.3","question":"...","answers":[{"answer":"...","consequence":"..."}],"autoResolvable":false,"recommendedAnswer":"..."}]}"#;

/// Kiro's requirements analysis, answered with a turn of the conversation handler.
pub struct RequirementsAnalysis {
    conversation: Arc<dyn FacadeHandler>,
}

impl RequirementsAnalysis {
    /// Analyses made as turns of `conversation`, the `/generateAssistantResponse` handler.
    pub fn new(conversation: Arc<dyn FacadeHandler>) -> Self {
        Self { conversation }
    }
}

impl FacadeHandler for RequirementsAnalysis {
    fn method(&self) -> Method {
        Method::POST
    }

    fn path(&self) -> &'static str {
        "/mcp/stream"
    }

    fn handle<'a>(&'a self, req: Request<Body>) -> BoxFuture<'a, Response> {
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let Ok(bytes) = to_bytes(body, MAX_CALL_BYTES).await else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "ValidationException",
                    "需求文档过大，无法分析：请拆分后再试。",
                );
            };
            let call: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let id = call.get("id").cloned().unwrap_or(Value::Null);
            if call["method"] != "tools/call" || call["params"]["name"] != "spec_disambiguation" {
                return events(vec![rpc_error(
                    &id,
                    METHOD_NOT_FOUND,
                    "本服务只提供需求分析（spec_disambiguation），不提供这个 MCP 调用。",
                )]);
            }
            let arguments = &call["params"]["arguments"];
            let requirements = arguments["requirementsText"].as_str().unwrap_or_default();
            if requirements.trim().is_empty() {
                return events(vec![rpc_error(
                    &id,
                    INVALID_PARAMS,
                    "需求文档为空，无法分析。",
                )]);
            }

            let answer = self
                .conversation
                .handle(analysis_turn(
                    &id,
                    arguments["conversationId"].as_str(),
                    requirements,
                    parts.extensions.get::<AuthClaims>(),
                ))
                .await;
            // Refused before the model was called (no credits, too many requests): the
            // refusal as the conversation gives it, which Kiro shows as the analysis's.
            if !answer.status().is_success() {
                return answer;
            }
            let request_id = answer.headers().get("x-amzn-requestid").cloned();
            let (frames, receiver) = tokio::sync::mpsc::channel::<Bytes>(16);
            tokio::spawn(analyse(answer.into_body(), id, frames));
            let body = futures_util::stream::unfold(receiver, |mut receiver| async move {
                let frame = receiver.recv().await?;
                Some((Ok::<_, std::convert::Infallible>(frame), receiver))
            });
            let mut response = event_stream(Body::from_stream(body));
            if let Some(request_id) = request_id {
                response
                    .headers_mut()
                    .insert("x-amzn-requestid", request_id);
            }
            response
        })
    }
}

/// The conversation turn that asks the fast model for the analysis: named after Kiro's call
/// ID, so that a call Kiro sends again is not run and billed twice.
fn analysis_turn(
    id: &Value,
    conversation_id: Option<&str>,
    requirements: &str,
    claims: Option<&AuthClaims>,
) -> Request<Body> {
    let conversation_id = conversation_id
        .filter(|id| !id.trim().is_empty() && id.chars().count() <= 256)
        .map_or_else(
            || format!("spec-analysis-{}", crate::now_secs()),
            str::to_string,
        );
    let turn = json!({
        "systemPrompt": INSTRUCTIONS,
        "conversationState": {
            "conversationId": conversation_id,
            "history": [],
            "currentMessage": {"userInputMessage": {
                "content": format!("<requirements>\n{requirements}\n</requirements>"),
                "modelId": super::models::SIMPLE_TASK_MODEL,
            }}
        }
    });
    let mut request = Request::builder()
        .method(Method::POST)
        .uri("/generateAssistantResponse")
        .header(header::CONTENT_TYPE, "application/json");
    let invocation = id
        .as_str()
        .filter(|id| {
            (1..=100).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        })
        .map(|id| format!("spec-analysis-{id}"));
    if let Some(invocation) = invocation {
        request = request.header("amz-sdk-invocation-id", invocation);
    }
    let mut request = request
        .body(Body::from(turn.to_string()))
        .expect("a request built from valid parts");
    if let Some(claims) = claims {
        request.extensions_mut().insert(claims.clone());
    }
    request
}

/// Reads the model's answer, and sends Kiro the analysis made of it: progress while it
/// waits, then each question, then the result, or the error that stopped it.
async fn analyse(answer: Body, id: Value, frames: tokio::sync::mpsc::Sender<Bytes>) {
    let send = |message: Value| {
        let frames = frames.clone();
        async move { frames.send(Bytes::from(frame(&message))).await.is_ok() }
    };
    if !send(progress("正在分析需求文档中的验收标准…")).await {
        return;
    }
    let messages = match model_text(answer, &frames).await {
        Err(None) => return,
        Err(Some(message)) => vec![rpc_error(&id, ANALYSIS_FAILED, &message)],
        Ok(text) => match questions(&text) {
            Err(message) => vec![rpc_error(&id, ANALYSIS_FAILED, message)],
            Ok(questions) => {
                let mut messages = vec![progress(&if questions.is_empty() {
                    "分析完成：没有发现需要确认的地方。".to_string()
                } else {
                    format!("分析完成：有 {} 处需要确认。", questions.len())
                })];
                let count = questions.len();
                messages.extend(questions.into_iter().map(partial_result));
                messages.push(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": format!("{count} clarifying questions")}],
                        "isError": false,
                    }
                }));
                messages
            }
        },
    };
    for message in messages {
        if !send(message).await {
            return;
        }
    }
}

/// The text of the model's answer, read to its end. The error is the one to show, or none
/// when Kiro has gone.
async fn model_text(
    answer: Body,
    frames: &tokio::sync::mpsc::Sender<Bytes>,
) -> Result<String, Option<String>> {
    use futures_util::StreamExt;
    let cut_short = || Some("模型的回答中断了，请重新分析。".to_string());
    let mut chunks = answer.into_data_stream();
    let mut decoder = kiro_wire::EventStreamDecoder::new();
    let mut text = String::new();
    let mut keepalive = tokio::time::interval(KEEPALIVE);
    keepalive.tick().await;
    loop {
        tokio::select! {
            chunk = chunks.next() => {
                let Some(chunk) = chunk else {
                    return Ok(text);
                };
                let chunk = chunk.map_err(|_| cut_short())?;
                decoder.feed(&chunk).map_err(|_| cut_short())?;
                while let Some(frame) = decoder.decode().map_err(|_| cut_short())? {
                    if frame.exception_type().is_some() {
                        let failure: Value = frame.payload_as_json().unwrap_or(Value::Null);
                        return Err(Some(format!(
                            "模型调用失败：{}",
                            failure["message"].as_str().unwrap_or("未知错误")
                        )));
                    }
                    if frame.event_type() == Some("assistantResponseEvent") {
                        if let Ok(event) = frame.payload_as_json::<kiro_wire::AssistantResponseEvent>() {
                            text.push_str(&event.content);
                        }
                    }
                }
            }
            // Kiro closed the stream: the answer is dropped, which stops the model.
            _ = frames.closed() => return Err(None),
            _ = keepalive.tick() => {
                let alive = json!({"jsonrpc": "2.0", "method": "notifications/keepalive"});
                if frames.send(Bytes::from(frame(&alive))).await.is_err() {
                    return Err(None);
                }
            }
        }
    }
}

/// One question as the model gave it, checked.
#[derive(Debug, Clone, PartialEq)]
struct Question {
    requirement_id: String,
    question: String,
    answers: Vec<(String, Option<String>)>,
    auto_resolvable: bool,
    recommended: Option<String>,
}

/// The questions in the model's answer: its JSON object, wherever it is in the text.
/// Questions without an ID, a question or an answer to choose are left out; one the
/// document settles needs no answers.
fn questions(text: &str) -> Result<Vec<Question>, &'static str> {
    const UNREADABLE: &str = "模型的分析结果无法读取，请重新分析。";
    let start = text.find('{').ok_or(UNREADABLE)?;
    let end = text
        .rfind('}')
        .filter(|end| *end > start)
        .ok_or(UNREADABLE)?;
    let value: Value = serde_json::from_str(&text[start..=end]).map_err(|_| UNREADABLE)?;
    let items = value["questions"].as_array().ok_or(UNREADABLE)?;
    let string = |value: &Value| -> Option<String> {
        match value {
            Value::String(text) => Some(text.trim().to_string()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        }
        .filter(|text| !text.is_empty())
    };
    Ok(items
        .iter()
        .filter_map(|item| {
            let answers: Vec<(String, Option<String>)> = item["answers"]
                .as_array()
                .map(|answers| {
                    answers
                        .iter()
                        .filter_map(|answer| match answer {
                            Value::Object(_) => {
                                Some((string(&answer["answer"])?, string(&answer["consequence"])))
                            }
                            other => Some((string(other)?, None)),
                        })
                        .take(MAX_ANSWERS)
                        .collect()
                })
                .unwrap_or_default();
            let recommended = string(&item["recommendedAnswer"]);
            let auto_resolvable = item["autoResolvable"] == true && recommended.is_some();
            (auto_resolvable || !answers.is_empty()).then_some(())?;
            Some(Question {
                requirement_id: string(&item["requirementId"])?,
                question: string(&item["question"])?,
                answers,
                auto_resolvable,
                recommended,
            })
        })
        .take(MAX_QUESTIONS)
        .collect())
}

/// A question as Kiro reads it: a partial result of `CLARIFYING_QUESTIONS`.
fn partial_result(question: Question) -> Value {
    let default = question
        .recommended
        .as_ref()
        .filter(|recommended| {
            question
                .answers
                .iter()
                .any(|(answer, _)| answer == *recommended)
        })
        .map(|answer| json!({"answer": answer}));
    let mut details = json!({
        "question": question.question,
        "answerChoices": question
            .answers
            .iter()
            .map(|(answer, consequence)| match consequence {
                Some(consequence) => json!({"answer": answer, "consequence": consequence}),
                None => json!({"answer": answer}),
            })
            .collect::<Vec<_>>(),
        "autoResolvable": question.auto_resolvable,
    });
    if let Some(recommended) = question.recommended {
        details["recommendedAnswer"] = json!(recommended);
    }
    if let Some(default) = default {
        details["defaultChoice"] = default;
    }
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/partial_result",
        "params": {
            "responseType": "CLARIFYING_QUESTIONS",
            "requirementId": question.requirement_id,
            "question": details,
        }
    })
}

fn progress(message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {"status": "IN_PROGRESS", "message": message}
    })
}

/// The error Kiro shows as "Analysis error: <message> (code: <code>)".
fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// One JSON-RPC message as the `message` event Kiro reads.
fn frame(message: &Value) -> Vec<u8> {
    kiro_wire::encode_event("message", message).unwrap_or_default()
}

/// A stream of `messages`, all known at once.
fn events(messages: Vec<Value>) -> Response {
    let bytes: Vec<u8> = messages.iter().flat_map(frame).collect();
    event_stream(Body::from(bytes))
}

/// An event stream: Kiro's agent reads a body as frames only when it is labelled so.
fn event_stream(body: Body) -> Response {
    let mut response = Response::new(body);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.amazon.eventstream"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_questions_are_read_wherever_its_json_is() {
        let text = "Here you go:\n```json\n{\"questions\":[\
            {\"requirementId\":\"1.2\",\"question\":\"How many attempts?\",\
             \"answers\":[{\"answer\":\"3\",\"consequence\":\"Locked after 3\"},\"5\"],\
             \"autoResolvable\":false,\"recommendedAnswer\":\"3\"},\
            {\"requirementId\":2,\"question\":\"Which format?\",\"answers\":[],\
             \"autoResolvable\":true,\"recommendedAnswer\":\"ISO 8601\"},\
            {\"requirementId\":\"3\",\"question\":\"No answers\",\"answers\":[]},\
            {\"question\":\"No ID\",\"answers\":[\"a\"]}]}\n```";
        let read = questions(text).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].requirement_id, "1.2");
        assert_eq!(
            read[0].answers,
            vec![
                ("3".to_string(), Some("Locked after 3".to_string())),
                ("5".to_string(), None)
            ]
        );
        assert_eq!(read[1].requirement_id, "2");
        assert!(read[1].auto_resolvable);

        let result = partial_result(read[0].clone());
        assert_eq!(result["params"]["question"]["defaultChoice"]["answer"], "3");
        assert!(questions("I could not find any.").is_err());
        assert_eq!(questions("{\"questions\":[]}").unwrap(), Vec::new());
    }
}
