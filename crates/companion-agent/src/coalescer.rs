//! Throttled UI stream coalescer for token bursts and incremental Markdown parsing.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::parser::strip_tool_calls;
use companion_core::markdown::{
    CodeHighlightCache, MarkdownBlock, parse_markdown_blocks_streaming,
};
use companion_core::provider::ChatEvent;

const STREAM_UI_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

/// Coalesced UI update emitted for a single inference step.
#[derive(Debug, Clone)]
pub enum StreamStepUpdate {
    Status(String),
    Snapshot {
        combined: String,
        blocks: Option<Vec<MarkdownBlock>>,
        reasoning: Option<String>,
    },
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        decode_ms: Option<u64>,
    },
    Notice(String),
    Error(String),
    Done {
        step_answer: String,
        full_reasoning: String,
    },
}

/// Join multi-step answer segments with a blank line when continuing after a tool call.
pub fn combine_answer(prefix: &str, step_answer: &str) -> String {
    match (prefix.is_empty(), step_answer.is_empty()) {
        (true, _) => step_answer.to_string(),
        (_, true) => prefix.to_string(),
        (false, false) => format!("{prefix}\n\n{step_answer}"),
    }
}

/// Pure, timer-free state machine that buffers token bursts and produces [`StreamStepUpdate`]s.
pub struct StreamCoalescerState {
    full_answer_prefix: String,
    full_reasoning: String,
    step_answer: String,
    last_combined: String,
    code_cache: CodeHighlightCache,
    dirty: bool,
}

pub enum CoalescerAction {
    Continue,
    Forward(StreamStepUpdate),
    Finish,
}

impl StreamCoalescerState {
    pub fn new(full_answer_prefix: String, full_reasoning: String) -> Self {
        Self {
            full_answer_prefix,
            full_reasoning,
            step_answer: String::new(),
            last_combined: String::new(),
            code_cache: CodeHighlightCache::new(),
            dirty: false,
        }
    }

    pub fn push_event(&mut self, ev: ChatEvent) -> CoalescerAction {
        match ev {
            ChatEvent::Status(status) => {
                if self.full_answer_prefix.is_empty() && self.full_reasoning.is_empty() {
                    CoalescerAction::Forward(StreamStepUpdate::Status(status))
                } else {
                    CoalescerAction::Continue
                }
            }
            ChatEvent::Token(t) => {
                self.step_answer.push_str(&t);
                self.dirty = true;
                CoalescerAction::Continue
            }
            ChatEvent::ReasoningToken(r) => {
                self.full_reasoning.push_str(&r);
                self.dirty = true;
                CoalescerAction::Continue
            }
            ChatEvent::Usage {
                prompt_tokens,
                completion_tokens,
                decode_ms,
            } => CoalescerAction::Forward(StreamStepUpdate::Usage {
                prompt_tokens,
                completion_tokens,
                decode_ms,
            }),
            ChatEvent::Notice(msg) => CoalescerAction::Forward(StreamStepUpdate::Notice(msg)),
            ChatEvent::Error(err) => CoalescerAction::Forward(StreamStepUpdate::Error(err)),
            ChatEvent::Done => CoalescerAction::Finish,
        }
    }

    pub fn flush(&mut self) -> StreamStepUpdate {
        let visible_step = strip_tool_calls(&self.step_answer);
        let combined = combine_answer(&self.full_answer_prefix, &visible_step);
        let blocks = if combined != self.last_combined {
            self.last_combined = combined.clone();
            Some(parse_markdown_blocks_streaming(
                &combined,
                &mut self.code_cache,
            ))
        } else {
            None
        };
        let reasoning = (!self.full_reasoning.is_empty()).then(|| self.full_reasoning.clone());
        self.dirty = false;
        StreamStepUpdate::Snapshot {
            combined,
            blocks,
            reasoning,
        }
    }

    pub fn finish(mut self) -> (Option<StreamStepUpdate>, StreamStepUpdate) {
        let trailing = if self.dirty { Some(self.flush()) } else { None };
        let done = StreamStepUpdate::Done {
            step_answer: self.step_answer,
            full_reasoning: self.full_reasoning,
        };
        (trailing, done)
    }
}

/// Coalesce token bursts and parse streaming Markdown on Tokio with the default 80 ms flush interval.
pub fn spawn_stream_coalescer(
    raw_rx: mpsc::UnboundedReceiver<ChatEvent>,
    full_answer_prefix: String,
    full_reasoning: String,
) -> mpsc::UnboundedReceiver<StreamStepUpdate> {
    spawn_stream_coalescer_with_interval(
        raw_rx,
        full_answer_prefix,
        full_reasoning,
        STREAM_UI_FLUSH_INTERVAL,
    )
}

/// Coalesce token bursts with an explicit `flush_interval` (useful for deterministic tests).
pub fn spawn_stream_coalescer_with_interval(
    mut raw_rx: mpsc::UnboundedReceiver<ChatEvent>,
    full_answer_prefix: String,
    full_reasoning: String,
    flush_interval: Duration,
) -> mpsc::UnboundedReceiver<StreamStepUpdate> {
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut state = StreamCoalescerState::new(full_answer_prefix, full_reasoning);
        let mut last_flush = Instant::now();

        loop {
            let ev_opt = if state.dirty {
                let deadline = tokio::time::Instant::from_std(last_flush + flush_interval);
                tokio::select! {
                    biased;
                    ev = raw_rx.recv() => Some(ev),
                    _ = tokio::time::sleep_until(deadline) => None,
                }
            } else {
                Some(raw_rx.recv().await)
            };

            let mut finished = false;
            match ev_opt {
                Some(None) => finished = true,
                Some(Some(first_ev)) => {
                    let mut pending = Some(first_ev);
                    while let Some(ev) = pending.take().or_else(|| raw_rx.try_recv().ok()) {
                        match state.push_event(ev) {
                            CoalescerAction::Continue => {}
                            CoalescerAction::Forward(update) => {
                                let _ = ui_tx.send(update);
                            }
                            CoalescerAction::Finish => {
                                finished = true;
                                break;
                            }
                        }
                    }
                }
                None => {}
            }

            if finished {
                let (trailing, done) = state.finish();
                if let Some(snap) = trailing {
                    let _ = ui_tx.send(snap);
                }
                let _ = ui_tx.send(done);
                break;
            }
            if state.dirty && last_flush.elapsed() >= flush_interval {
                let _ = ui_tx.send(state.flush());
                last_flush = Instant::now();
            }
        }
    });
    ui_rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_answer_joins_non_empty_parts() {
        assert_eq!(combine_answer("", "step1"), "step1");
        assert_eq!(combine_answer("prefix", ""), "prefix");
        assert_eq!(combine_answer("prefix", "step2"), "prefix\n\nstep2");
    }

    #[test]
    fn state_machine_strips_tool_calls_and_flushes_on_finish() {
        let mut state = StreamCoalescerState::new(String::new(), String::new());
        assert!(matches!(
            state.push_event(ChatEvent::Token("Hello ".into())),
            CoalescerAction::Continue
        ));
        assert!(matches!(
            state.push_event(ChatEvent::Token(
                "<tool_call>{\"name\":\"list_files\"}</tool_call>".into()
            )),
            CoalescerAction::Continue
        ));
        assert!(matches!(
            state.push_event(ChatEvent::Done),
            CoalescerAction::Finish
        ));

        let (trailing, done) = state.finish();
        assert!(matches!(
            trailing,
            Some(StreamStepUpdate::Snapshot { ref combined, .. }) if combined == "Hello"
        ));
        assert!(matches!(
            done,
            StreamStepUpdate::Done { ref step_answer, .. } if step_answer.contains("<tool_call>")
        ));
    }

    #[tokio::test]
    async fn flushes_dirty_trailing_tokens_before_done() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut out = spawn_stream_coalescer_with_interval(
            rx,
            String::new(),
            String::new(),
            Duration::from_secs(60),
        );

        let _ = tx.send(ChatEvent::Token("final-tail".into()));
        let _ = tx.send(ChatEvent::Done);

        let mut snapshots = Vec::new();
        while let Some(ev) = out.recv().await {
            match ev {
                StreamStepUpdate::Snapshot { combined, .. } => snapshots.push(combined),
                StreamStepUpdate::Done { .. } => break,
                _ => {}
            }
        }
        assert_eq!(snapshots.last().map(String::as_str), Some("final-tail"));
    }

    #[tokio::test]
    async fn timer_flushes_buffered_tokens_after_interval() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut out =
            spawn_stream_coalescer_with_interval(rx, String::new(), String::new(), Duration::ZERO);

        let _ = tx.send(ChatEvent::Token("hello ".into()));
        let ev = out.recv().await.expect("timed flush snapshot");
        assert!(matches!(
            ev,
            StreamStepUpdate::Snapshot { ref combined, .. } if combined == "hello"
        ));
        drop(tx);
    }

    #[test]
    fn status_events_are_suppressed_on_continuation_steps() {
        let mut state = StreamCoalescerState::new("Earlier answer".into(), String::new());
        assert!(matches!(
            state.push_event(ChatEvent::Status("Generating…".into())),
            CoalescerAction::Continue
        ));

        let mut fresh = StreamCoalescerState::new(String::new(), String::new());
        assert!(matches!(
            fresh.push_event(ChatEvent::Status("Generating…".into())),
            CoalescerAction::Forward(StreamStepUpdate::Status(ref s)) if s == "Generating…"
        ));
    }
}
