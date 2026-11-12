//! The event bus: monotonic sequence numbers, a replay ring, per-client topic filters and
//! slow-client handling.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{mpsc, watch};

use crate::frame::{encode_line, EventFrame, FrameError};
use crate::protocol::Event;
use crate::types::{SubscribeResult, Topic};

/// Number of events kept for replay.
pub const DEFAULT_RING: usize = 2048;

/// One encoded line (including the trailing newline), shared between clients.
pub type Line = Arc<[u8]>;

/// Where the bus delivers lines for one connection.
///
/// The queue is bounded. When it is full the bus does not wait: it signals `kill` so the
/// connection is closed, and the daemon is never blocked by a client that stopped reading.
#[derive(Debug, Clone)]
pub struct Sink {
    /// Bounded queue drained by the connection's writer.
    pub tx: mpsc::Sender<Line>,
    /// Set to `true` to make the connection close.
    pub kill: Arc<watch::Sender<bool>>,
}

impl Sink {
    fn offer(&self, line: Line) -> bool {
        match self.tx.try_send(line) {
            Ok(()) => true,
            Err(_) => {
                self.kill.send_replace(true);
                false
            }
        }
    }
}

struct Stored {
    seq: u64,
    topic: Topic,
    line: Line,
}

struct Sub {
    topics: Option<HashSet<Topic>>,
    sink: Sink,
}

impl Sub {
    fn wants(&self, topic: Topic) -> bool {
        self.topics.as_ref().map_or(true, |t| t.contains(&topic))
    }
}

struct Inner {
    next_seq: u64,
    ring: VecDeque<Stored>,
    subs: HashMap<u64, Sub>,
}

/// Publishes events to subscribed connections and keeps a bounded replay ring.
pub struct EventBus {
    run_id: String,
    capacity: usize,
    inner: Mutex<Inner>,
}

impl EventBus {
    /// A bus for daemon run `run_id` with the default 2048-entry ring.
    pub fn new(run_id: impl Into<String>) -> Self {
        Self::with_capacity(run_id, DEFAULT_RING)
    }

    /// A bus with a custom replay ring size (at least 1).
    pub fn with_capacity(run_id: impl Into<String>, capacity: usize) -> Self {
        Self {
            run_id: run_id.into(),
            capacity: capacity.max(1),
            inner: Mutex::new(Inner {
                next_seq: 1,
                ring: VecDeque::new(),
                subs: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Identifier of this daemon run; returned by `hello`. Changes when the daemon restarts.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The newest assigned sequence number (0 when nothing was published).
    pub fn last_seq(&self) -> u64 {
        self.lock().next_seq - 1
    }

    /// Whether any connection is subscribed to `topic` (e.g. to skip producing `voice.level`).
    pub fn has_subscribers(&self, topic: Topic) -> bool {
        self.lock().subs.values().any(|s| s.wants(topic))
    }

    /// Assigns the next sequence number, stores the event in the ring and offers it to every
    /// matching subscriber. Returns the sequence number, or an error (and consumes no number)
    /// when the event cannot be encoded within the line limit.
    pub fn publish(&self, event: &Event) -> Result<u64, FrameError> {
        let mut inner = self.lock();
        let seq = inner.next_seq;
        let line: Line = encode_line(&EventFrame::new(seq, event)?)?.into();
        inner.next_seq += 1;
        let topic = event.topic();
        if inner.ring.len() == self.capacity {
            inner.ring.pop_front();
        }
        inner.ring.push_back(Stored {
            seq,
            topic,
            line: line.clone(),
        });
        inner
            .subs
            .retain(|_, sub| !sub.wants(topic) || sub.sink.offer(line.clone()));
        Ok(seq)
    }

    /// Registers (or replaces) the subscription of connection `conn`.
    ///
    /// Under one lock this queues the reply line built by `reply`, then the replayed events
    /// with `seq > after_seq` that match the topics, then switches the connection to live
    /// delivery, so nothing is lost or duplicated and the reply precedes every event. On a gap
    /// (`after_seq` older than the ring, or newer than the daemon has ever issued) nothing is
    /// replayed: the client must re-fetch state.
    pub fn attach(
        &self,
        conn: u64,
        sink: Sink,
        after_seq: Option<u64>,
        topics: Option<Vec<Topic>>,
        reply: impl FnOnce(SubscribeResult) -> Line,
    ) -> SubscribeResult {
        let mut inner = self.lock();
        let last = inner.next_seq - 1;
        let gap = match after_seq {
            None => false,
            Some(a) if a > last => true,
            Some(a) if a == last => false,
            Some(a) => inner.ring.front().map_or(true, |f| f.seq > a + 1),
        };
        let result = SubscribeResult { seq: last, gap };
        inner.subs.remove(&conn);
        let sub = Sub {
            topics: topics.map(|t| t.into_iter().collect()),
            sink,
        };
        let mut ok = sub.sink.offer(reply(result));
        if let (true, Some(a), false) = (ok, after_seq, gap) {
            for stored in inner
                .ring
                .iter()
                .filter(|s| s.seq > a && sub.wants(s.topic))
            {
                if !sub.sink.offer(stored.line.clone()) {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            inner.subs.insert(conn, sub);
        }
        result
    }

    /// Removes the subscription of connection `conn`, if any.
    pub fn detach(&self, conn: u64) {
        self.lock().subs.remove(&conn);
    }

    /// Number of subscribed connections.
    pub fn subscriber_count(&self) -> usize {
        self.lock().subs.len()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("run_id", &self.run_id)
            .field("last_seq", &self.last_seq())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Empty, NotifyCountEvent};

    fn sink(cap: usize) -> (Sink, mpsc::Receiver<Line>) {
        let (tx, rx) = mpsc::channel(cap);
        (
            Sink {
                tx,
                kill: Arc::new(watch::channel(false).0),
            },
            rx,
        )
    }

    fn chat_ev() -> Event {
        Event::ChatListChanged(Empty {})
    }

    fn count(n: u32) -> Event {
        Event::NotifyCount(NotifyCountEvent { unseen: n })
    }

    fn seqs(rx: &mut mpsc::Receiver<Line>) -> Vec<u64> {
        let mut out = Vec::new();
        while let Ok(l) = rx.try_recv() {
            let v: serde_json::Value = serde_json::from_slice(&l).unwrap();
            out.push(v.get("seq").and_then(|s| s.as_u64()).unwrap_or(0));
        }
        out
    }

    fn reply_line(r: SubscribeResult) -> Line {
        let mut v = serde_json::to_vec(&serde_json::json!({"gap": r.gap, "last": r.seq})).unwrap();
        v.push(b'\n');
        v.into()
    }

    #[test]
    fn sequence_numbers_are_monotonic() {
        let bus = EventBus::new("r");
        assert_eq!(bus.last_seq(), 0);
        assert_eq!(bus.publish(&chat_ev()).unwrap(), 1);
        assert_eq!(bus.publish(&chat_ev()).unwrap(), 2);
        assert_eq!(bus.last_seq(), 2);
    }

    #[test]
    fn replay_resumes_after_seq_in_order() {
        let bus = EventBus::new("r");
        for _ in 0..5 {
            bus.publish(&chat_ev()).unwrap();
        }
        let (s, mut rx) = sink(16);
        let r = bus.attach(1, s, Some(2), None, reply_line);
        assert_eq!(r, SubscribeResult { seq: 5, gap: false });
        bus.publish(&chat_ev()).unwrap();
        let got = seqs(&mut rx);
        // first line is the reply (no seq key -> 0), then 3,4,5 replayed, then live 6
        assert_eq!(got, vec![0, 3, 4, 5, 6]);
    }

    #[test]
    fn no_after_seq_means_live_only() {
        let bus = EventBus::new("r");
        bus.publish(&chat_ev()).unwrap();
        let (s, mut rx) = sink(16);
        bus.attach(1, s, None, None, reply_line);
        bus.publish(&chat_ev()).unwrap();
        assert_eq!(seqs(&mut rx), vec![0, 2]);
    }

    #[test]
    fn gap_when_ring_overflowed_and_no_replay() {
        let bus = EventBus::with_capacity("r", 3);
        for _ in 0..10 {
            bus.publish(&chat_ev()).unwrap();
        }
        let (s, mut rx) = sink(16);
        let r = bus.attach(1, s, Some(2), None, reply_line);
        assert!(r.gap);
        assert_eq!(seqs(&mut rx), vec![0]);
        // exactly at the edge: ring holds 8,9,10 so after_seq 7 is gapless
        let (s, mut rx) = sink(16);
        let r = bus.attach(2, s, Some(7), None, reply_line);
        assert!(!r.gap);
        assert_eq!(seqs(&mut rx), vec![0, 8, 9, 10]);
        let (s, _rx) = sink(16);
        assert!(bus.attach(3, s, Some(6), None, reply_line).gap);
    }

    #[test]
    fn after_seq_from_the_future_is_a_gap() {
        let bus = EventBus::new("r");
        bus.publish(&chat_ev()).unwrap();
        let (s, _rx) = sink(4);
        assert!(bus.attach(1, s, Some(99), None, reply_line).gap);
    }

    #[test]
    fn caught_up_client_has_no_gap() {
        let bus = EventBus::new("r");
        let (s, mut rx) = sink(4);
        let r = bus.attach(1, s, Some(0), None, reply_line);
        assert_eq!(r, SubscribeResult { seq: 0, gap: false });
        assert_eq!(seqs(&mut rx), vec![0]);
    }

    #[test]
    fn topic_filters_apply_to_replay_and_live() {
        let bus = EventBus::new("r");
        bus.publish(&chat_ev()).unwrap(); // chat
        bus.publish(&count(1)).unwrap(); // notify
        let (s, mut rx) = sink(16);
        bus.attach(1, s, Some(0), Some(vec![Topic::Notify]), reply_line);
        bus.publish(&chat_ev()).unwrap();
        bus.publish(&count(2)).unwrap();
        assert_eq!(seqs(&mut rx), vec![0, 2, 4]);
        assert!(bus.has_subscribers(Topic::Notify));
        assert!(!bus.has_subscribers(Topic::Voice));
    }

    #[test]
    fn empty_topic_list_receives_nothing() {
        let bus = EventBus::new("r");
        let (s, mut rx) = sink(16);
        bus.attach(1, s, None, Some(vec![]), reply_line);
        bus.publish(&chat_ev()).unwrap();
        assert_eq!(seqs(&mut rx), vec![0]);
    }

    #[test]
    fn unknown_topics_are_ignored() {
        let bus = EventBus::new("r");
        let (s, mut rx) = sink(16);
        bus.attach(
            1,
            s,
            None,
            Some(vec![Topic::Unknown, Topic::Chat]),
            reply_line,
        );
        bus.publish(&chat_ev()).unwrap();
        assert_eq!(seqs(&mut rx), vec![0, 1]);
    }

    #[test]
    fn resubscribe_replaces_filters() {
        let bus = EventBus::new("r");
        let (s, mut rx) = sink(16);
        bus.attach(1, s.clone(), None, Some(vec![Topic::Chat]), reply_line);
        bus.attach(1, s, None, Some(vec![Topic::Notify]), reply_line);
        assert_eq!(bus.subscriber_count(), 1);
        bus.publish(&chat_ev()).unwrap();
        bus.publish(&count(1)).unwrap();
        assert_eq!(seqs(&mut rx), vec![0, 0, 2]);
    }

    #[test]
    fn slow_client_is_killed_and_does_not_block() {
        let bus = EventBus::new("r");
        let (s, _rx) = sink(2);
        let mut kill = s.kill.subscribe();
        bus.attach(1, s, None, None, reply_line); // 1 queued
        bus.publish(&chat_ev()).unwrap(); // 2 queued (full)
        assert!(!*kill.borrow_and_update());
        bus.publish(&chat_ev()).unwrap(); // overflow
        assert!(*kill.borrow_and_update());
        assert_eq!(bus.subscriber_count(), 0);
        // the daemon keeps going
        assert_eq!(bus.publish(&chat_ev()).unwrap(), 3);
    }

    #[test]
    fn closed_receiver_is_dropped() {
        let bus = EventBus::new("r");
        let (s, rx) = sink(4);
        bus.attach(1, s, None, None, reply_line);
        drop(rx);
        bus.publish(&chat_ev()).unwrap();
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn replay_overflowing_the_queue_kills_the_client() {
        let bus = EventBus::new("r");
        for _ in 0..10 {
            bus.publish(&chat_ev()).unwrap();
        }
        let (s, _rx) = sink(3);
        let kill = s.kill.clone();
        bus.attach(1, s, Some(0), None, reply_line);
        assert!(*kill.borrow());
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn oversized_event_is_rejected_without_consuming_a_seq() {
        let bus = EventBus::new("r");
        let big = Event::Unknown {
            event: "x.big".into(),
            data: serde_json::Value::String("a".repeat(crate::frame::MAX_LINE)),
        };
        assert!(bus.publish(&big).is_err());
        assert_eq!(bus.last_seq(), 0);
    }
}
