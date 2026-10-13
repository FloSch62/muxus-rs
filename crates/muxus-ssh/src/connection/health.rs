//! Passive keepalive health: observe replies to the transport's own keepalives without sending
//! probes of our own. Two silent intervals mean at least one keepalive went unanswered.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Notify, watch};

/// Keepalive health of one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportHealth {
    Healthy,
    Suspect,
}

/// ssh_config keepalive settings as the client applies them: OpenSSH's ServerAliveInterval
/// default is zero (disabled), while ServerAliveCountMax defaults to three when an interval is
/// explicitly set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeepaliveOptions {
    pub interval: Duration,
    pub count_max: usize,
}

pub fn ssh_keepalive_options(
    server_alive_interval: Option<f64>,
    server_alive_count_max: Option<f64>,
) -> KeepaliveOptions {
    KeepaliveOptions {
        interval: Duration::from_secs_f64(server_alive_interval.unwrap_or(0.0).max(0.0)),
        count_max: server_alive_count_max.map(|n| n as usize).unwrap_or(3),
    }
}

/// Bytes arriving on a transport, for health observers.
#[derive(Default)]
pub struct TransportActivity {
    notify: Notify,
    /// Set once the transport hit EOF or an error.
    ended: Mutex<bool>,
}

impl TransportActivity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records incoming data.
    pub fn received(&self) {
        self.notify.notify_waiters();
    }

    fn end(&self) {
        *self.ended.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.notify.notify_waiters();
    }

    pub fn ended(&self) -> bool {
        *self.ended.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Watches for silence: `Suspect` after two keepalive intervals without data, `Healthy`
    /// again on the next byte. Returns `None` when keepalives are disabled. The task ends when the
    /// returned receiver and every clone of it are dropped.
    pub fn observe(
        self: &Arc<Self>,
        keepalive_interval: Duration,
    ) -> Option<watch::Receiver<TransportHealth>> {
        if keepalive_interval.is_zero() {
            return None;
        }
        let (tx, rx) = watch::channel(TransportHealth::Healthy);
        let activity = self.clone();
        tokio::spawn(async move {
            loop {
                let notified = activity.notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                tokio::select! {
                    _ = &mut notified => {
                        if tx.is_closed() || activity.ended() {
                            return;
                        }
                        if *tx.borrow() == TransportHealth::Suspect {
                            let _ = tx.send(TransportHealth::Healthy);
                        }
                    }
                    _ = tokio::time::sleep(keepalive_interval * 2) => {
                        if tx.is_closed() {
                            return;
                        }
                        if *tx.borrow() == TransportHealth::Healthy {
                            let _ = tx.send(TransportHealth::Suspect);
                        }
                        // Stay suspect until data arrives again.
                        let notified = activity.notify.notified();
                        tokio::pin!(notified);
                        tokio::select! {
                            _ = notified => {
                                if activity.ended() { return; }
                                let _ = tx.send(TransportHealth::Healthy);
                            }
                            _ = tx.closed() => return,
                        }
                    }
                    _ = tx.closed() => return,
                }
            }
        });
        Some(rx)
    }
}

/// A transport stream that reports incoming bytes to a [`TransportActivity`].
pub struct ObservedStream<S> {
    inner: S,
    activity: Arc<TransportActivity>,
}

impl<S> ObservedStream<S> {
    pub fn new(inner: S, activity: Arc<TransportActivity>) -> Self {
        Self { inner, activity }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for ObservedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        match &result {
            Poll::Ready(Ok(())) if buf.filled().len() > before => self.activity.received(),
            Poll::Ready(_) => self.activity.end(),
            Poll::Pending => {}
        }
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for ObservedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_openssh_compatible_defaults_without_implicit_probes() {
        assert_eq!(
            ssh_keepalive_options(None, None),
            KeepaliveOptions {
                interval: Duration::ZERO,
                count_max: 3
            }
        );
    }

    #[test]
    fn honors_explicit_server_alive_settings() {
        assert_eq!(
            ssh_keepalive_options(Some(45.0), Some(7.0)),
            KeepaliveOptions {
                interval: Duration::from_secs(45),
                count_max: 7
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn turns_suspect_after_two_silent_keepalive_intervals_and_recovers_on_input() {
        let activity = TransportActivity::new();
        let mut rx = activity.observe(Duration::from_secs(15)).unwrap();
        tokio::time::sleep(Duration::from_millis(29_999)).await;
        assert_eq!(*rx.borrow(), TransportHealth::Healthy);
        tokio::time::sleep(Duration::from_millis(2)).await;
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow_and_update(), TransportHealth::Suspect);

        activity.received();
        rx.changed().await.unwrap();
        assert_eq!(*rx.borrow_and_update(), TransportHealth::Healthy);
    }

    #[tokio::test(start_paused = true)]
    async fn does_not_monitor_when_ssh_keepalives_are_disabled() {
        let activity = TransportActivity::new();
        assert!(activity.observe(Duration::ZERO).is_none());
    }

    #[tokio::test]
    async fn observed_streams_report_incoming_bytes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut a, b) = tokio::io::duplex(64);
        let activity = TransportActivity::new();
        let mut observed = ObservedStream::new(b, activity.clone());
        let notified = activity.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        a.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        observed.read_exact(&mut buf).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .unwrap();
        drop(a);
        let mut rest = Vec::new();
        observed.read_to_end(&mut rest).await.unwrap();
        assert!(activity.ended());
    }
}
