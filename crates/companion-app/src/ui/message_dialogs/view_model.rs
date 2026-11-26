//! Pure, GTK-free formatting helpers for the Message Info dialog.

use crate::core::types::{ExecutionDevice, ToolCallStatus};
use crate::i18n::{gettext, tr_f};
use crate::storage::ToolCallRecord;
use crate::ui::message_object::MessageEntry;

/// Pure view-model formatting for [`MessageEntry`] metadata and tool call summaries.
pub(super) struct MessageInfoViewModel;

impl MessageInfoViewModel {
    pub(super) fn device_label(device: Option<ExecutionDevice>) -> String {
        match device {
            Some(ExecutionDevice::Gpu) => gettext("GPU (Vulkan)"),
            Some(ExecutionDevice::Cpu) => gettext("CPU"),
            None => "—".to_string(),
        }
    }

    pub(super) fn duration_text(entry: &MessageEntry) -> String {
        if entry.is_streaming {
            gettext("Generating…")
        } else if let Some(ms) = entry.generation_ms {
            format!("{:.2} s", (ms as f64) / 1000.0)
        } else {
            "—".to_string()
        }
    }

    pub(super) fn speed_text(entry: &MessageEntry) -> String {
        if entry.is_streaming {
            gettext("Generating…")
        } else if let Some(ms) = entry.generation_ms
            && ms > 0
            && entry.token_count > 0
        {
            let tps = (entry.token_count as f64) / ((ms as f64) / 1000.0);
            tr_f("{speed} tokens/s", &[("speed", &format!("{tps:.1}"))])
        } else {
            "—".to_string()
        }
    }

    pub(super) fn tokens_text(entry: &MessageEntry) -> String {
        if entry.is_streaming {
            gettext("Generating…")
        } else if entry.token_count > 0 {
            entry.token_count.to_string()
        } else {
            "—".to_string()
        }
    }

    pub(super) fn summarize_tool_call(tc: &ToolCallRecord) -> String {
        let raw_target = serde_json::from_str::<serde_json::Value>(&tc.arguments_json)
            .ok()
            .and_then(|v| {
                ["path", "subdir", "query", "command"]
                    .iter()
                    .find_map(|key| v.get(*key).and_then(|x| x.as_str()).map(str::to_string))
            })
            .unwrap_or_else(|| {
                tc.arguments_json
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            });
        let target = if raw_target.trim() == "." {
            gettext("Granted root (.)")
        } else {
            raw_target
        };

        if tc.status != ToolCallStatus::Ok
            && let Some(err) = tc
                .output
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            return format!("{target} — {err}");
        }
        target
    }
}

#[cfg(test)]
mod tests {
    use super::MessageInfoViewModel;
    use crate::core::types::{ExecutionDevice, ToolCallStatus};
    use crate::storage::ToolCallRecord;
    use crate::ui::message_object::MessageEntry;

    #[test]
    fn formats_metrics_and_tool_call_summaries() {
        let streaming = MessageEntry {
            is_streaming: true,
            ..Default::default()
        };
        assert_eq!(
            MessageInfoViewModel::duration_text(&streaming),
            "Generating…"
        );
        assert_eq!(MessageInfoViewModel::speed_text(&streaming), "Generating…");
        assert_eq!(MessageInfoViewModel::tokens_text(&streaming), "Generating…");

        let done = MessageEntry {
            execution_device: Some(ExecutionDevice::Gpu),
            token_count: 50,
            generation_ms: Some(2000),
            ..Default::default()
        };
        assert_eq!(
            MessageInfoViewModel::device_label(done.execution_device),
            "GPU (Vulkan)"
        );
        assert_eq!(MessageInfoViewModel::duration_text(&done), "2.00 s");
        assert_eq!(MessageInfoViewModel::speed_text(&done), "25.0 tokens/s");
        assert_eq!(MessageInfoViewModel::tokens_text(&done), "50");

        let tc = ToolCallRecord {
            id: "1".into(),
            message_id: "m1".into(),
            tool_name: "list_files".into(),
            arguments_json: r#"{"subdir":"."}"#.into(),
            status: ToolCallStatus::Denied,
            output: Some("User denied".into()),
            permission_decision: crate::core::types::PermissionDecision::Deny,
            created_at: 0,
        };
        assert_eq!(
            MessageInfoViewModel::summarize_tool_call(&tc),
            "Granted root (.) — User denied"
        );
    }
}
