//! Bookkeeping for resuming an event subscription across reconnects.

/// Remembers the daemon `run_id` and the last event `seq` a client has processed, so a
/// reconnecting client only asks for replay when the daemon is the same run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumeState {
    run_id: Option<String>,
    last_seq: Option<u64>,
}

impl ResumeState {
    /// A fresh state with nothing processed yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `after_seq` to send to a daemon with the given `run_id`: the last processed `seq`,
    /// or `None` when the run changed (sequence numbers restarted) or nothing was seen.
    pub fn after_seq_for(&self, run_id: &str) -> Option<u64> {
        match &self.run_id {
            Some(r) if r == run_id => self.last_seq,
            _ => None,
        }
    }

    /// Records that event `seq` of daemon run `run_id` was processed.
    pub fn observe(&mut self, run_id: &str, seq: u64) {
        if self.run_id.as_deref() != Some(run_id) {
            self.run_id = Some(run_id.to_owned());
            self.last_seq = None;
        }
        self.last_seq = Some(self.last_seq.map_or(seq, |s| s.max(seq)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_only_within_a_run() {
        let mut r = ResumeState::new();
        assert_eq!(r.after_seq_for("a"), None);
        r.observe("a", 5);
        r.observe("a", 3);
        assert_eq!(r.after_seq_for("a"), Some(5));
        assert_eq!(r.after_seq_for("b"), None);
        r.observe("b", 1);
        assert_eq!(r.after_seq_for("b"), Some(1));
        assert_eq!(r.after_seq_for("a"), None);
    }
}
