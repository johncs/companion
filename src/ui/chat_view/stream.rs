//! Streaming token coalescing, Markdown incremental parsing, and active run handle.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::agent::parser::strip_tool_calls;
use crate::core::markdown::{CodeHighlightCache, MarkdownBlock, parse_markdown_blocks_streaming};
use crate::core::runtime;
use crate::inference::provider::ChatEvent;
use crate::ui::message_object::MessageObject;

const STREAM_UI_FLUSH_INTERVAL: Duration = Duration::from_millis(80);

pub(super) struct ActiveChatRun {
    pub(super) stream_obj: MessageObject,
    pub(super) cancel_flag: Arc<AtomicBool>,
}

pub(super) enum StreamStepUpdate {
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

pub(super) fn combine_answer(prefix: &str, step_visible: &str) -> String {
    if prefix.is_empty() {
        step_visible.to_string()
    } else if step_visible.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}\n\n{step_visible}")
    }
}

/// Coalesce token bursts and parse streaming Markdown on Tokio so the GTK main thread stays responsive.
pub(super) fn spawn_stream_coalescer(
    mut raw_rx: mpsc::UnboundedReceiver<ChatEvent>,
    full_answer_prefix: String,
    mut full_reasoning: String,
) -> mpsc::UnboundedReceiver<StreamStepUpdate> {
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    runtime().spawn(async move {
        let mut step_answer = String::new();
        let mut last_combined = String::new();
        let mut code_cache = CodeHighlightCache::new();
        let mut last_flush = Instant::now();
        let mut dirty = false;

        let flush_snapshot = |step_answer: &str,
                              full_reasoning: &str,
                              last_combined: &mut String,
                              code_cache: &mut CodeHighlightCache| {
            let visible_step = strip_tool_calls(step_answer);
            let combined = combine_answer(&full_answer_prefix, &visible_step);
            let blocks = if combined != *last_combined {
                *last_combined = combined.clone();
                Some(parse_markdown_blocks_streaming(&combined, code_cache))
            } else {
                None
            };
            let reasoning = (!full_reasoning.is_empty()).then(|| full_reasoning.to_string());
            let _ = ui_tx.send(StreamStepUpdate::Snapshot {
                combined,
                blocks,
                reasoning,
            });
        };

        loop {
            let ev_opt = if dirty {
                let deadline = tokio::time::Instant::from_std(last_flush + STREAM_UI_FLUSH_INTERVAL);
                tokio::select! {
                    biased;
                    ev = raw_rx.recv() => Some(ev),
                    _ = tokio::time::sleep_until(deadline) => None,
                }
            } else {
                Some(raw_rx.recv().await)
            };

            match ev_opt {
                Some(None) => {
                    let _ = ui_tx.send(StreamStepUpdate::Done {
                        step_answer,
                        full_reasoning,
                    });
                    break;
                }
                Some(Some(first_ev)) => {
                    let mut done = false;
                    let mut pending = Some(first_ev);
                    while let Some(ev) = pending.take().or_else(|| raw_rx.try_recv().ok()) {
                        match ev {
                            ChatEvent::Status(status) => {
                                if full_answer_prefix.is_empty() && full_reasoning.is_empty() {
                                    let _ = ui_tx.send(StreamStepUpdate::Status(status));
                                }
                            }
                            ChatEvent::Token(t) => {
                                step_answer.push_str(&t);
                                dirty = true;
                            }
                            ChatEvent::ReasoningToken(r) => {
                                full_reasoning.push_str(&r);
                                dirty = true;
                            }
                            ChatEvent::Usage {
                                prompt_tokens,
                                completion_tokens,
                                decode_ms,
                            } => {
                                let _ = ui_tx.send(StreamStepUpdate::Usage {
                                    prompt_tokens,
                                    completion_tokens,
                                    decode_ms,
                                });
                            }
                            ChatEvent::Notice(msg) => {
                                let _ = ui_tx.send(StreamStepUpdate::Notice(msg));
                            }
                            ChatEvent::Error(err) => {
                                let _ = ui_tx.send(StreamStepUpdate::Error(err));
                            }
                            ChatEvent::Done => {
                                done = true;
                                break;
                            }
                        }
                    }

                    if done {
                        let _ = ui_tx.send(StreamStepUpdate::Done {
                            step_answer,
                            full_reasoning,
                        });
                        break;
                    }

                    if dirty && last_flush.elapsed() >= STREAM_UI_FLUSH_INTERVAL {
                        flush_snapshot(
                            &step_answer,
                            &full_reasoning,
                            &mut last_combined,
                            &mut code_cache,
                        );
                        dirty = false;
                        last_flush = Instant::now();
                    }
                }
                None => {
                    if dirty {
                        flush_snapshot(
                            &step_answer,
                            &full_reasoning,
                            &mut last_combined,
                            &mut code_cache,
                        );
                        dirty = false;
                        last_flush = Instant::now();
                    }
                }
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
}
