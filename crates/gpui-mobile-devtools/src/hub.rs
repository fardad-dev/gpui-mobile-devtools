//! The event hub: numbers every event, keeps recent ones for viewers that
//! connect later, and fans events out to connected viewers.
//!
//! Publishing never blocks on a viewer: each viewer has a bounded queue, and
//! a viewer that falls that far behind is disconnected (it reconnects and
//! gets the buffer again).

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};

/// Queue length per viewer before it counts as stuck.
const VIEWER_QUEUE: usize = 8192;

pub(crate) type Message = Arc<str>;

struct Inner {
    next_seq: u64,
    buffer: VecDeque<Message>,
    buffered_bytes: usize,
    viewers: Vec<SyncSender<Message>>,
}

pub(crate) struct Hub {
    limit_bytes: usize,
    inner: Mutex<Inner>,
}

static HUB: OnceLock<Hub> = OnceLock::new();

pub(crate) fn init(limit_bytes: usize) {
    let _ = HUB.set(Hub {
        limit_bytes,
        inner: Mutex::new(Inner {
            next_seq: 1,
            buffer: VecDeque::new(),
            buffered_bytes: 0,
            viewers: Vec::new(),
        }),
    });
}

pub(crate) fn get() -> Option<&'static Hub> {
    HUB.get()
}

impl Hub {
    /// Publishes `json`, a serialized JSON object, after giving it the next
    /// sequence number as its first field.
    pub(crate) fn publish(&self, json: &str) {
        debug_assert!(json.starts_with('{'));
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let seq = inner.next_seq;
        inner.next_seq += 1;
        let body = &json[1..];
        let message: Message = if body.trim_start().starts_with('}') {
            format!("{{\"seq\":{seq}{body}").into()
        } else {
            format!("{{\"seq\":{seq},{body}").into()
        };

        inner.buffered_bytes += message.len();
        inner.buffer.push_back(message.clone());
        while inner.buffered_bytes > self.limit_bytes && inner.buffer.len() > 1 {
            if let Some(old) = inner.buffer.pop_front() {
                inner.buffered_bytes -= old.len();
            }
        }

        inner
            .viewers
            .retain(|viewer| match viewer.try_send(message.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
            });
    }

    /// Registers a viewer. Returns the buffered history and a receiver for
    /// everything published after it, with nothing lost or repeated between
    /// the two.
    pub(crate) fn subscribe(&self) -> (Vec<Message>, Receiver<Message>) {
        let (sender, receiver) = mpsc::sync_channel(VIEWER_QUEUE);
        let mut inner = self.inner.lock().unwrap_or_else(|poison| poison.into_inner());
        let history = inner.buffer.iter().cloned().collect();
        inner.viewers.push(sender);
        (history, receiver)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(limit: usize) -> Hub {
        Hub {
            limit_bytes: limit,
            inner: Mutex::new(Inner {
                next_seq: 1,
                buffer: VecDeque::new(),
                buffered_bytes: 0,
                viewers: Vec::new(),
            }),
        }
    }

    #[test]
    fn numbers_events_and_replays_history() {
        let hub = hub(1 << 20);
        hub.publish(r#"{"type":"a"}"#);
        hub.publish("{}");
        let (history, receiver) = hub.subscribe();
        assert_eq!(&*history[0], r#"{"seq":1,"type":"a"}"#);
        assert_eq!(&*history[1], r#"{"seq":2}"#);
        hub.publish(r#"{"type":"b"}"#);
        assert_eq!(&*receiver.try_recv().unwrap(), r#"{"seq":3,"type":"b"}"#);
    }

    #[test]
    fn drops_oldest_events_past_the_limit() {
        let hub = hub(40);
        for _ in 0..10 {
            hub.publish(r#"{"type":"event"}"#);
        }
        let (history, _) = hub.subscribe();
        assert_eq!(history.len(), 1);
        assert!(history[0].starts_with(r#"{"seq":10,"#));
    }

    #[test]
    fn forgets_viewers_that_disconnected() {
        let hub = hub(1 << 20);
        let (_, receiver) = hub.subscribe();
        drop(receiver);
        hub.publish("{}");
        assert!(hub.inner.lock().unwrap().viewers.is_empty());
    }
}
