//! Streaming stop-sequence detector that holds back a minimal trailing window
//! so stop markers split across token pieces are caught cleanly.

/// Default stop sequences emitted by common local chat templates (ChatML, Gemma, Llama 3, Phi).
pub const DEFAULT_STOP_SEQUENCES: [&str; 5] = [
    "<|im_end|>",
    "<end_of_turn>",
    "<turn|>",
    "<|eot_id|>",
    "<|end|>",
];

/// Result of pushing a decoded text piece into [`StopSequenceFilter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutput {
    /// Safe text ready to forward downstream (may be empty while buffering a potential suffix).
    Emit(String),
    /// A stop sequence was matched; `emit` contains any text preceding the stop marker.
    Stopped { emit: String },
}

/// Incremental stop-sequence filter over streamed UTF-8 token pieces.
pub struct StopSequenceFilter {
    sequences: Vec<&'static str>,
    max_hold: usize,
    tail: String,
}

impl Default for StopSequenceFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl StopSequenceFilter {
    /// Create a filter using [`DEFAULT_STOP_SEQUENCES`].
    pub fn new() -> Self {
        Self::with_sequences(DEFAULT_STOP_SEQUENCES.to_vec())
    }

    /// Create a filter matching the given stop sequences.
    pub fn with_sequences(sequences: Vec<&'static str>) -> Self {
        let max_hold = sequences.iter().map(|s| s.len()).max().unwrap_or(0);
        Self {
            sequences,
            max_hold,
            tail: String::new(),
        }
    }

    /// Feed a newly decoded piece and return either safe text to emit or a stop signal.
    pub fn push(&mut self, piece: &str) -> StopOutput {
        if piece.is_empty() {
            return StopOutput::Emit(String::new());
        }

        self.tail.push_str(piece);
        if let Some(stop_idx) = self
            .sequences
            .iter()
            .filter_map(|s| self.tail.find(s))
            .min()
        {
            let emit = self.tail[..stop_idx].to_string();
            self.tail.clear();
            return StopOutput::Stopped { emit };
        }

        if self.tail.len() > self.max_hold {
            let mut flush_end = self.tail.len() - self.max_hold;
            while flush_end > 0 && !self.tail.is_char_boundary(flush_end) {
                flush_end -= 1;
            }
            if flush_end > 0 {
                let safe: String = self.tail.drain(..flush_end).collect();
                return StopOutput::Emit(safe);
            }
        }

        StopOutput::Emit(String::new())
    }

    /// Flush any remaining buffered text at end-of-stream (trimming trailing whitespace).
    pub fn finish(&mut self) -> String {
        if self.tail.is_empty() {
            return String::new();
        }
        let trimmed = self.tail.trim_end().to_string();
        self.tail.clear();
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_stop_sequence_split_across_chunks() {
        let mut filter = StopSequenceFilter::new();
        let mut emitted = String::new();

        for piece in ["Hello ", "world!<|im", "_end|>", " extra"] {
            match filter.push(piece) {
                StopOutput::Emit(s) => emitted.push_str(&s),
                StopOutput::Stopped { emit } => {
                    emitted.push_str(&emit);
                    assert_eq!(emitted, "Hello world!");
                    assert_eq!(filter.finish(), "");
                    return;
                }
            }
        }
        panic!("expected stop sequence to trigger");
    }

    #[test]
    fn earliest_stop_sequence_wins() {
        let mut filter = StopSequenceFilter::new();
        assert_eq!(
            filter.push("done<|end|>after<|im_end|>"),
            StopOutput::Stopped {
                emit: "done".to_string()
            }
        );
    }

    #[test]
    fn preserves_utf8_boundaries_while_flushing() {
        let mut filter = StopSequenceFilter::with_sequences(vec!["<|stop|>"]);
        let mut emitted = String::new();

        for piece in ["🦀🦀🦀", " café ", "done"] {
            match filter.push(piece) {
                StopOutput::Emit(s) => emitted.push_str(&s),
                StopOutput::Stopped { .. } => panic!("unexpected stop"),
            }
        }
        emitted.push_str(&filter.finish());
        assert_eq!(emitted, "🦀🦀🦀 café done");
    }

    #[test]
    fn finish_trims_trailing_whitespace_without_stop() {
        let mut filter = StopSequenceFilter::new();
        let mut emitted = String::new();
        if let StopOutput::Emit(s) = filter.push("Hi   \n") {
            emitted.push_str(&s);
        }
        emitted.push_str(&filter.finish());
        assert_eq!(emitted, "Hi");
    }

    #[test]
    fn partial_stop_prefix_at_eof_is_flushed_and_empty_push_is_noop() {
        let mut filter = StopSequenceFilter::new();
        assert_eq!(filter.push(""), StopOutput::Emit(String::new()));
        let mut out = String::new();
        if let StopOutput::Emit(s) = filter.push("Hello<|im_") {
            out.push_str(&s);
        }
        out.push_str(&filter.finish());
        assert_eq!(out, "Hello<|im_");
    }

    #[test]
    fn empty_stop_list_emits_immediately() {
        let mut filter = StopSequenceFilter::with_sequences(Vec::new());
        assert_eq!(
            filter.push("instant"),
            StopOutput::Emit("instant".to_string())
        );
        assert_eq!(filter.finish(), "");
    }
}
