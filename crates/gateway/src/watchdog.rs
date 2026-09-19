//! Upstream three-tier watchdog (TTFB, Inter-chunk Idle, Hard Timeout).
//!
//! Spec §8 (Operations & Reliability: Upstream Watchdog).
//! Prevents hanging connections on upstream LLM stalls while allowing long streaming conversations.

use futures_util::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq, Clone)]
pub enum WatchdogError {
    #[error(
        "Upstream TTFB timeout ({timeout:?}): no initial byte/chunk received within threshold"
    )]
    TtfbTimeout { timeout: Duration },

    #[error("Upstream stream idle timeout ({timeout:?}): chunk interval exceeded threshold")]
    IdleTimeout { timeout: Duration },

    #[error("Upstream hard timeout ({timeout:?}): total request duration cap exceeded")]
    HardTimeout { timeout: Duration },
}

/// Three-tier timeout configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogConfig {
    /// Maximum time to wait for the first response byte/token from upstream.
    pub ttfb_timeout: Duration,
    /// Maximum time allowed between consecutive streaming chunks.
    pub idle_timeout: Duration,
    /// Maximum global hard ceiling for the entire request lifecycle.
    pub hard_timeout: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            ttfb_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(90),
            hard_timeout: Duration::from_secs(600),
        }
    }
}

/// Stream wrapper that enforces TTFB, idle, and hard timeouts.
pub struct WatchdogStream<S> {
    inner: S,
    config: WatchdogConfig,
    started_at: Instant,
    last_chunk_at: Option<Instant>,
    has_received_first_chunk: bool,
    sleep: Pin<Box<tokio::time::Sleep>>,
}

impl<S> WatchdogStream<S> {
    pub fn new(inner: S, config: WatchdogConfig) -> Self {
        let deadline = std::time::Instant::now() + config.ttfb_timeout.min(config.hard_timeout);
        Self {
            inner,
            config,
            started_at: Instant::now(),
            last_chunk_at: None,
            has_received_first_chunk: false,
            sleep: Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(
                deadline,
            ))),
        }
    }
}

impl<S, T, E> Stream for WatchdogStream<S>
where
    S: Stream<Item = Result<T, E>> + Unpin,
    E: From<WatchdogError>,
{
    type Item = Result<T, E>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let now = Instant::now();

        // 1. Check Hard Timeout (global cap)
        if now.duration_since(self.started_at) > self.config.hard_timeout {
            return Poll::Ready(Some(Err(WatchdogError::HardTimeout {
                timeout: self.config.hard_timeout,
            }
            .into())));
        }

        // 2. Check TTFB vs Idle Timeout
        if !self.has_received_first_chunk {
            if now.duration_since(self.started_at) > self.config.ttfb_timeout {
                return Poll::Ready(Some(Err(WatchdogError::TtfbTimeout {
                    timeout: self.config.ttfb_timeout,
                }
                .into())));
            }
        } else if let Some(last_at) = self.last_chunk_at {
            if now.duration_since(last_at) > self.config.idle_timeout {
                return Poll::Ready(Some(Err(WatchdogError::IdleTimeout {
                    timeout: self.config.idle_timeout,
                }
                .into())));
            }
        }

        // 3. Poll underlying upstream stream
        match Pin::new(&mut self.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                self.has_received_first_chunk = true;
                let n = Instant::now();
                self.last_chunk_at = Some(n);

                let next_deadline =
                    (n + self.config.idle_timeout).min(self.started_at + self.config.hard_timeout);
                self.sleep
                    .as_mut()
                    .reset(tokio::time::Instant::from_std(next_deadline));

                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => {
                use std::future::Future;
                match self.sleep.as_mut().poll(cx) {
                    Poll::Ready(_) => {
                        let now = Instant::now();
                        if now.duration_since(self.started_at) >= self.config.hard_timeout {
                            Poll::Ready(Some(Err(WatchdogError::HardTimeout {
                                timeout: self.config.hard_timeout,
                            }
                            .into())))
                        } else if !self.has_received_first_chunk {
                            Poll::Ready(Some(Err(WatchdogError::TtfbTimeout {
                                timeout: self.config.ttfb_timeout,
                            }
                            .into())))
                        } else {
                            Poll::Ready(Some(Err(WatchdogError::IdleTimeout {
                                timeout: self.config.idle_timeout,
                            }
                            .into())))
                        }
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }
}
