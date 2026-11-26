//! Pure UTF-8 decoding, stop-sequence filtering, and reasoning-tag parsing pipeline.

use encoding_rs::{CoderResult, Decoder, UTF_8};

use crate::stop_filter::{StopOutput, StopSequenceFilter};
use crate::think_parser::{ParsedChunk, ThinkStreamParser};

/// Output produced by feeding a single token's raw bytes into [`TokenStreamPipeline`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineStep {
    pub chunks: Vec<ParsedChunk>,
    pub stopped: bool,
}

/// Composes `encoding_rs::Decoder` + [`StopSequenceFilter`] + [`ThinkStreamParser`]
/// so the streaming token pipeline can be tested without `llama.cpp` or GTK.
pub struct TokenStreamPipeline {
    decoder: Decoder,
    stop_filter: StopSequenceFilter,
    think_parser: ThinkStreamParser,
    stopped: bool,
}

impl TokenStreamPipeline {
    /// Create a pipeline with default stop sequences and optional primed `initial_close_tag`.
    pub fn new(initial_close_tag: Option<&'static str>) -> Self {
        Self::with_stop_sequences(StopSequenceFilter::new(), initial_close_tag)
    }

    /// Create a pipeline with a custom [`StopSequenceFilter`].
    pub fn with_stop_sequences(
        stop_filter: StopSequenceFilter,
        initial_close_tag: Option<&'static str>,
    ) -> Self {
        let think_parser = match initial_close_tag {
            Some(tag) => ThinkStreamParser::starting_in_think(tag),
            None => ThinkStreamParser::new(),
        };
        Self {
            decoder: UTF_8.new_decoder(),
            stop_filter,
            think_parser,
            stopped: false,
        }
    }

    /// Decode a raw token byte slice (handling `OutputFull` without dropping UTF-8 bytes)
    /// and route it through stop-sequence and reasoning-tag filters.
    pub fn push_bytes(&mut self, bytes: &[u8]) -> PipelineStep {
        if self.stopped {
            return PipelineStep {
                chunks: Vec::new(),
                stopped: true,
            };
        }
        let piece = self.decode_chunk(bytes, false);
        self.push_decoded(&piece)
    }

    /// Flush any pending UTF-8 bytes, stop-filter tail, and reasoning-tag buffer at EOF.
    pub fn finish(&mut self) -> Vec<ParsedChunk> {
        let mut out = Vec::new();
        if !self.stopped {
            let tail = self.decode_chunk(&[], true);
            if !tail.is_empty() {
                let step = self.push_decoded(&tail);
                out.extend(step.chunks);
            }
            if !self.stopped {
                let remaining = self.stop_filter.finish();
                if !remaining.is_empty() {
                    out.extend(self.think_parser.push(&remaining));
                }
            }
        }
        out.extend(self.think_parser.finish());
        out
    }

    fn decode_chunk(&mut self, mut input: &[u8], last: bool) -> String {
        let cap = self
            .decoder
            .max_utf8_buffer_length(input.len())
            .unwrap_or_else(|| input.len().saturating_mul(3).saturating_add(4));
        let mut out = String::with_capacity(cap.max(4));
        let mut scratch = String::with_capacity(cap.max(4));
        loop {
            scratch.clear();
            let (result, read, _) = self.decoder.decode_to_string(input, &mut scratch, last);
            out.push_str(&scratch);
            input = &input[read..];
            match result {
                CoderResult::InputEmpty => break,
                CoderResult::OutputFull => {
                    let needed = self
                        .decoder
                        .max_utf8_buffer_length(input.len())
                        .unwrap_or(16)
                        .max(16);
                    if scratch.capacity() < needed {
                        scratch.reserve(needed - scratch.capacity());
                    }
                }
            }
        }
        out
    }

    fn push_decoded(&mut self, piece: &str) -> PipelineStep {
        match self.stop_filter.push(piece) {
            StopOutput::Emit(safe) if !safe.is_empty() => PipelineStep {
                chunks: self.think_parser.push(&safe),
                stopped: false,
            },
            StopOutput::Emit(_) => PipelineStep {
                chunks: Vec::new(),
                stopped: false,
            },
            StopOutput::Stopped { emit } => {
                self.stopped = true;
                let chunks = if emit.is_empty() {
                    Vec::new()
                } else {
                    self.think_parser.push(&emit)
                };
                PipelineStep {
                    chunks,
                    stopped: true,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_all(chunks: Vec<ParsedChunk>, reasoning: &mut String, answer: &mut String) {
        for c in chunks {
            match c {
                ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                ParsedChunk::Answer(a) => answer.push_str(&a),
            }
        }
    }

    #[test]
    fn decodes_split_multibyte_utf8_and_invalid_bytes_without_drops() {
        let mut pipe = TokenStreamPipeline::new(None);
        let mut reasoning = String::new();
        let mut answer = String::new();

        // 4-byte emoji "🦀" + 3-byte CJK "中" + invalid UTF-8 byte 0xFF + normal text.
        let mut bytes = Vec::new();
        bytes.extend_from_slice("🦀中".as_bytes());
        bytes.push(0xFF);
        bytes.extend_from_slice(b"ok");

        for b in bytes.chunks(1) {
            let step = pipe.push_bytes(b);
            assert!(!step.stopped);
            collect_all(step.chunks, &mut reasoning, &mut answer);
        }
        collect_all(pipe.finish(), &mut reasoning, &mut answer);

        assert_eq!(answer, "🦀中\u{FFFD}ok");
        assert!(reasoning.is_empty());
    }

    #[test]
    fn flushes_incomplete_utf8_trailing_sequence_on_eof() {
        let mut pipe = TokenStreamPipeline::new(None);
        let mut reasoning = String::new();
        let mut answer = String::new();

        // First 2 bytes of a 4-byte emoji, followed by EOF.
        let step = pipe.push_bytes(&"🦀".as_bytes()[..2]);
        collect_all(step.chunks, &mut reasoning, &mut answer);
        collect_all(pipe.finish(), &mut reasoning, &mut answer);

        assert_eq!(answer, "\u{FFFD}");
    }

    #[test]
    fn handles_split_gemma4_channel_thought_newline_boundary() {
        let mut pipe = TokenStreamPipeline::new(None);
        let mut reasoning = String::new();
        let mut answer = String::new();

        for tok in [
            "<|channel>thought",
            "\nstep 1",
            "<channel|>Final answer<|end|>",
        ] {
            let step = pipe.push_bytes(tok.as_bytes());
            collect_all(step.chunks, &mut reasoning, &mut answer);
            if step.stopped {
                break;
            }
        }
        collect_all(pipe.finish(), &mut reasoning, &mut answer);

        assert_eq!(reasoning, "step 1");
        assert_eq!(answer, "Final answer");
    }
}
