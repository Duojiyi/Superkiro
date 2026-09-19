use futures_util::StreamExt;
use gateway::watchdog::{WatchdogConfig, WatchdogError, WatchdogStream};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;

#[derive(Debug, PartialEq, Eq)]
enum MockStreamError {
    #[allow(dead_code)]
    Upstream(String),
    Watchdog(WatchdogError),
}

impl From<WatchdogError> for MockStreamError {
    fn from(err: WatchdogError) -> Self {
        MockStreamError::Watchdog(err)
    }
}

// Convert unbounded Receiver to a Stream
struct ReceiverStream<T> {
    rx: mpsc::UnboundedReceiver<T>,
}

impl<T> futures_util::Stream for ReceiverStream<T> {
    type Item = T;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

#[tokio::test]
async fn test_watchdog_happy_path() {
    let config = WatchdogConfig {
        ttfb_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_millis(500),
        hard_timeout: Duration::from_millis(2000),
    };

    let (tx, rx) = mpsc::unbounded_channel::<Result<String, MockStreamError>>();
    tx.send(Ok("token_1".to_string())).unwrap();
    tx.send(Ok("token_2".to_string())).unwrap();
    tx.send(Ok("token_3".to_string())).unwrap();
    drop(tx);

    let stream = ReceiverStream { rx };
    let mut watchdog_stream = WatchdogStream::new(stream, config);

    let mut collected = Vec::new();
    while let Some(res) = watchdog_stream.next().await {
        collected.push(res.unwrap());
    }

    assert_eq!(collected, vec!["token_1", "token_2", "token_3"]);
}

#[tokio::test]
async fn test_watchdog_ttfb_timeout() {
    let config = WatchdogConfig {
        ttfb_timeout: Duration::from_millis(50),
        idle_timeout: Duration::from_millis(500),
        hard_timeout: Duration::from_millis(2000),
    };

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        sleep(Duration::from_millis(150)).await;
        let _ = tx.send(Ok("late_token".to_string()));
    });

    let stream = ReceiverStream { rx };
    let mut watchdog_stream = WatchdogStream::new(stream, config);
    let first = watchdog_stream.next().await;

    assert!(matches!(
        first,
        Some(Err(MockStreamError::Watchdog(
            WatchdogError::TtfbTimeout { .. }
        )))
    ));
}

#[tokio::test]
async fn test_watchdog_idle_timeout_between_chunks() {
    let config = WatchdogConfig {
        ttfb_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_millis(50),
        hard_timeout: Duration::from_millis(2000),
    };

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let _ = tx.send(Ok("first_token".to_string()));
        // Idle delay between chunks
        sleep(Duration::from_millis(150)).await;
        let _ = tx.send(Ok("second_token".to_string()));
    });

    let stream = ReceiverStream { rx };
    let mut watchdog_stream = WatchdogStream::new(stream, config);

    // 1st token arrives fine
    let first = watchdog_stream.next().await;
    assert_eq!(first.unwrap().unwrap(), "first_token");

    // 2nd poll triggers idle timeout!
    let second = watchdog_stream.next().await;
    assert!(matches!(
        second,
        Some(Err(MockStreamError::Watchdog(
            WatchdogError::IdleTimeout { .. }
        )))
    ));
}

#[tokio::test]
async fn test_watchdog_hard_timeout_cap() {
    let config = WatchdogConfig {
        ttfb_timeout: Duration::from_millis(500),
        idle_timeout: Duration::from_millis(500),
        hard_timeout: Duration::from_millis(80),
    };

    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        for i in 0..10 {
            sleep(Duration::from_millis(30)).await;
            if tx.send(Ok(format!("tok_{i}"))).is_err() {
                break;
            }
        }
    });

    let stream = ReceiverStream { rx };
    let mut watchdog_stream = WatchdogStream::new(stream, config);

    let mut hit_hard_timeout = false;
    while let Some(res) = watchdog_stream.next().await {
        if let Err(MockStreamError::Watchdog(WatchdogError::HardTimeout { .. })) = res {
            hit_hard_timeout = true;
            break;
        }
    }

    assert!(hit_hard_timeout);
}
