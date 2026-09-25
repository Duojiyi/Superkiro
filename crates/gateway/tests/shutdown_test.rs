//! Streams still open when the gateway stops.
//!
//! Shutdown waited for every connection, and a stream may run ten minutes, so the
//! container was killed with streams still running: output already delivered, and holds
//! dropped at the next start, so that work was never billed. Streams are cut instead,
//! and each settles what it streamed. The signal is process-wide, so this is the only
//! test in this binary.

use billing::{BillingEngine, Card, ReservationEstimateParams};
use futures_util::StreamExt;
use gateway::provider::{ProviderDelta, ProviderStreamEvent, ReceiverStream};
use gateway::stream::{create_stream_guard, BillingSettler, StreamGuardConfig};
use kiro_wire::decoder::EventStreamDecoder;
use std::time::Duration;

mod support;

#[tokio::test]
async fn open_streams_are_cut_and_billed_for_what_they_streamed_when_the_gateway_stops() {
    let billing = BillingEngine::new();
    billing.upsert_rate_card_version(support::wildcard_price("default"));
    let mut card = Card::new("card-stop", "group-default", 1_000_000_000);
    card.activate(gateway::now_secs(), 86_400).unwrap();
    billing.upsert_card(card);
    billing
        .reserve(
            "card-stop",
            "inv-stop",
            &ReservationEstimateParams::new(1_000, 8_000).with_model("model"),
            gateway::now_secs(),
            660,
        )
        .unwrap();

    // The model has streamed some text and is still going.
    let (upstream, rx) = tokio::sync::mpsc::channel(16);
    upstream
        .send(Ok(ProviderStreamEvent::Delta(ProviderDelta::Text(
            "The first part of a long answer. ".repeat(20),
        ))))
        .await
        .unwrap();
    let settler = BillingSettler::new(
        billing.clone(),
        "inv-stop".into(),
        "model".into(),
        "provider".into(),
        "model".into(),
    )
    .with_estimated_input(1_000);
    let mut stream = create_stream_guard(
        ReceiverStream::new(rx),
        StreamGuardConfig::default(),
        None,
        None,
        Some(settler),
    );
    let mut decoder = EventStreamDecoder::new();
    let mut frames = Vec::new();
    while frames.is_empty() {
        decoder
            .feed(&stream.next().await.unwrap().unwrap())
            .unwrap();
        while let Some(frame) = decoder.decode().unwrap() {
            frames.push(frame);
        }
    }

    gateway::stream::cut_open_streams();
    let rest = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(chunk) = stream.next().await {
            decoder.feed(&chunk.unwrap()).unwrap();
            while let Some(frame) = decoder.decode().unwrap() {
                frames.push(frame);
            }
        }
    })
    .await;
    assert!(rest.is_ok(), "the open stream was not cut");
    let names: Vec<String> = frames
        .iter()
        .map(|frame| {
            frame
                .headers
                .get(":event-type")
                .or_else(|| frame.headers.get(":exception-type"))
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(
        names.last().map(String::as_str),
        Some("InternalServerException")
    );

    let ledger = billing.list_ledger_entries_for_card("card-stop", None);
    assert_eq!(ledger.len(), 1, "what was streamed was not billed");
    assert!(ledger[0].output_tokens > 0);
    assert_eq!(billing.get_card("card-stop").unwrap().credit_reserved, 0);
    drop(upstream);
}
