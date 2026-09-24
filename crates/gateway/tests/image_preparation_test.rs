//! Images in a conversation: a bounded number per request, decoded only before
//! translation and within a time budget, and never forwarded in a form the provider
//! would reject on this turn and every later one.
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use gateway::translate::{prepare_images, translate_kiro_to_chat_request, TranslationContext};
use kiro_wire::requests::conversation::GenerateAssistantResponseRequest;
use serde_json::{json, Value};
use std::io::Cursor;
use std::time::{Duration, Instant};

const LIMIT: usize = 20;
const BUDGET: Duration = Duration::from_secs(30);

fn png(width: u32, height: u32) -> String {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([10, 120, 200]));
    let mut out = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    BASE64.encode(out)
}

fn user(content: &str, images: &[(&str, String)]) -> Value {
    let images: Vec<Value> = images
        .iter()
        .map(|(format, bytes)| json!({"format": format, "source": {"bytes": bytes}}))
        .collect();
    json!({"userInputMessage": {"content": content, "images": images}})
}

/// One user turn per entry of `history`, each answered, then a current turn.
fn request(
    history: Vec<Vec<(&str, String)>>,
    current: Vec<(&str, String)>,
) -> GenerateAssistantResponseRequest {
    let mut turns = Vec::new();
    for (index, images) in history.iter().enumerate() {
        turns.push(user(&format!("turn {index}"), images));
        turns.push(json!({"assistantResponseMessage": {"content": "seen"}}));
    }
    serde_json::from_value(json!({
        "conversationState": {
            "conversationId": "conversation",
            "currentMessage": user("and now?", &current),
            "history": turns,
        }
    }))
    .unwrap()
}

/// Image URLs and omission notes across all user messages, in order.
fn sent(
    req: &GenerateAssistantResponseRequest,
    ctx: &mut TranslationContext,
) -> (Vec<String>, Vec<String>) {
    let chat = translate_kiro_to_chat_request(req, ctx);
    let (mut images, mut notes) = (Vec::new(), Vec::new());
    for message in chat.messages.iter().filter(|m| m.role == "user") {
        for part in message.content.as_array().into_iter().flatten() {
            match part["type"].as_str() {
                Some("image_url") => images.push(part["image_url"]["url"].as_str().unwrap().into()),
                Some("text") if part["text"].as_str().unwrap().contains("omitted") => {
                    notes.push(part["text"].as_str().unwrap().into())
                }
                _ => {}
            }
        }
    }
    (images, notes)
}

async fn prepared(req: &GenerateAssistantResponseRequest) -> TranslationContext {
    TranslationContext::new("claude-sonnet-4.5")
        .with_vision_support(true)
        .with_prepared_images(prepare_images(req, LIMIT, BUDGET).await)
}

#[tokio::test]
async fn only_the_most_recent_images_are_sent() {
    let images: Vec<String> = (0..26).map(|n| png(8 + n, 8)).collect();
    let history = images[..25]
        .iter()
        .map(|image| vec![("png", image.clone())])
        .collect();
    let req = request(history, vec![("png", images[25].clone())]);

    let (sent_images, notes) = sent(&req, &mut prepared(&req).await);

    let expected: Vec<String> = images[6..]
        .iter()
        .map(|image| format!("data:image/png;base64,{image}"))
        .collect();
    assert_eq!(sent_images, expected, "the {LIMIT} most recent, in order");
    assert_eq!(notes.len(), 6);
    assert!(notes.iter().all(|note| note.contains("not resent")));
}

#[tokio::test]
async fn an_unreadable_history_image_becomes_a_note() {
    for bad in [BASE64.encode(b"not an image at all"), "%%%".to_string()] {
        let req = request(vec![vec![("png", bad.clone())]], vec![]);
        for mut ctx in [
            prepared(&req).await,
            TranslationContext::new("claude-sonnet-4.5").with_vision_support(true),
        ] {
            let (images, notes) = sent(&req, &mut ctx);
            assert!(images.is_empty(), "forwarded an image the provider rejects");
            assert_eq!(notes.len(), 1);
            assert!(notes[0].contains("cannot be read"));
        }
    }
}

#[tokio::test]
async fn large_images_are_shrunk_before_translation_and_never_during_it() {
    let req = request(vec![vec![("png", png(3000, 60))]], vec![]);

    // Translation alone never decodes: without preparation the image is left out.
    let mut bare = TranslationContext::new("claude-sonnet-4.5").with_vision_support(true);
    let (images, notes) = sent(&req, &mut bare);
    assert!(images.is_empty());
    assert!(notes[0].contains("could not process"));

    let (images, notes) = sent(&req, &mut prepared(&req).await);
    assert!(notes.is_empty());
    let jpeg = images[0].strip_prefix("data:image/jpeg;base64,").unwrap();
    let decoded = image::load_from_memory(&BASE64.decode(jpeg).unwrap()).unwrap();
    // The long side is brought to the limit and the aspect ratio kept.
    assert!(decoded.width() <= 1568 && decoded.width() > 1500);
    assert_eq!(decoded.height(), 31);
}

#[tokio::test]
async fn a_mislabelled_image_is_sent_under_its_real_type() {
    let image = png(16, 16);
    let req = request(vec![], vec![("jpeg", image.clone())]);
    let (images, _) = sent(&req, &mut prepared(&req).await);
    assert_eq!(images, vec![format!("data:image/png;base64,{image}")]);
}

#[tokio::test]
async fn a_request_of_decompression_bombs_is_bounded_by_its_budget() {
    // Kilobytes on the wire, tens of megabytes once decoded.
    let bomb = png(4096, 2048);
    assert!(bomb.len() < 200_000);
    let history = (0..LIMIT).map(|_| vec![("png", bomb.clone())]).collect();
    let req = request(history, vec![]);

    let budget = Duration::from_millis(200);
    let started = Instant::now();
    let prepared = prepare_images(&req, LIMIT, budget).await;
    assert!(
        started.elapsed() < budget + Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );

    let mut ctx = TranslationContext::new("claude-sonnet-4.5")
        .with_vision_support(true)
        .with_prepared_images(prepared);
    let (images, notes) = sent(&req, &mut ctx);
    assert_eq!(images.len() + notes.len(), LIMIT);
    assert!(images
        .iter()
        .all(|url| url.starts_with("data:image/jpeg;base64,")));
    assert!(notes.iter().all(|note| note.contains("could not process")));
}

#[tokio::test]
async fn a_text_model_reads_the_header_and_never_the_pixels() {
    let req = request(vec![vec![("png", png(3000, 60))]], vec![("png", png(4, 2))]);
    let mut ctx = TranslationContext::new("deepseek-chat");
    let chat = translate_kiro_to_chat_request(&req, &mut ctx);
    let text: String = chat
        .messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.content.as_str().unwrap().to_string())
        .collect();
    assert!(text.contains("尺寸: 3000x60"), "{text}");
    assert!(text.contains("尺寸: 4x2"), "{text}");
    assert!(!text.contains("base64"));
}
