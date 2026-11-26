//! Pure, unit-testable view-model for active AI model detail labels and Vision action state.

use std::path::Path;

use crate::i18n::{gettext, tr_f};
use crate::models::format_bytes;
use crate::models::gguf::format_context_label;

use super::inventory::InstalledModelInfo;

pub(super) struct ActiveModelViewModel {
    pub(super) file_sub: String,
    pub(super) file_tooltip: Option<String>,
    pub(super) size_sub: String,
    pub(super) quant_sub: String,
    pub(super) ctx_sub: String,
    pub(super) speed_sub: String,
    pub(super) text_sub: String,
    pub(super) thinking_sub: String,
    pub(super) vision_sub: String,
    pub(super) vision_action_label: Option<String>,
    pub(super) audio_sub: String,
}

impl ActiveModelViewModel {
    pub(super) fn from_info(info: &InstalledModelInfo) -> Self {
        let file_name = info
            .record
            .file_path
            .as_deref()
            .map(|p| {
                Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(p)
            })
            .unwrap_or("—");
        let file_sub = if info.file_size.is_some() {
            file_name.to_string()
        } else {
            format!("{file_name} ({})", gettext("Missing file"))
        };
        let size_sub = info
            .file_size
            .map(format_bytes)
            .unwrap_or_else(|| gettext("Missing file"));
        let quant_sub = info
            .record
            .quantization
            .as_deref()
            .unwrap_or("—")
            .to_string();
        let ctx_sub = info
            .gguf
            .max_context
            .map(format_context_label)
            .unwrap_or_else(|| "—".to_string());
        let speed_sub = if let Some(tps) = info.record.avg_tokens_per_sec {
            tr_f("{tps} tok/s", &[("tps", &format!("{tps:.1}"))])
        } else {
            gettext("Unavailable")
        };
        let thinking_supported = info.record.supports_thinking || info.gguf.supports_thinking;
        let thinking_sub = if thinking_supported {
            gettext("Active")
        } else {
            gettext("Unavailable")
        };
        let (vision_sub, vision_action_label) = resolve_vision_state(info);
        let audio_supported =
            info.gguf.supports_audio || info.mmproj_gguf.as_ref().is_some_and(|m| m.supports_audio);
        let audio_sub = if audio_supported {
            gettext("Available (Coming soon)")
        } else {
            gettext("Unavailable")
        };

        Self {
            file_sub,
            file_tooltip: info.record.file_path.clone(),
            size_sub,
            quant_sub,
            ctx_sub,
            speed_sub,
            text_sub: gettext("Active"),
            thinking_sub,
            vision_sub,
            vision_action_label,
            audio_sub,
        }
    }
}

fn resolve_vision_state(info: &InstalledModelInfo) -> (String, Option<String>) {
    let action_label = || {
        if info.record.hf_repo.is_some() {
            gettext("Install Vision")
        } else {
            gettext("Enable Vision")
        }
    };
    if info.record.mmproj_path.is_some() {
        if info.mmproj_exists {
            if info.mmproj_gguf.as_ref().is_some_and(|m| !m.is_clip) {
                (gettext("Invalid projector file"), Some(action_label()))
            } else {
                (gettext("Active"), None)
            }
        } else {
            (gettext("Missing file"), Some(action_label()))
        }
    } else if info.gguf.supports_vision {
        let sub = if info.record.hf_repo.is_some() {
            gettext("Available (Requires mmproj)")
        } else {
            gettext("Available (Requires external mmproj file)")
        };
        (sub, Some(action_label()))
    } else {
        (gettext("Unavailable"), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model_params::ModelParams;
    use crate::models::gguf::GgufMetadata;
    use crate::storage::ModelRecord;

    fn sample_info() -> InstalledModelInfo {
        InstalledModelInfo {
            record: ModelRecord {
                id: "m1".to_string(),
                display_name: "Gemma 4".to_string(),
                provider_type: "llama_cpp".to_string(),
                file_path: Some("/tmp/gemma4.gguf".to_string()),
                mmproj_path: None,
                hf_repo: Some("org/gemma".to_string()),
                quantization: Some("Q4_K_M".to_string()),
                supports_thinking: true,
                avg_tokens_per_sec: Some(42.5),
                default_params: ModelParams::default(),
            },
            file_size: Some(2_048),
            gguf: GgufMetadata {
                supports_vision: true,
                max_context: Some(8192),
                ..Default::default()
            },
            mmproj_exists: false,
            mmproj_gguf: None,
        }
    }

    #[test]
    fn formats_active_model_labels_and_vision_prompt() {
        let vm = ActiveModelViewModel::from_info(&sample_info());
        assert_eq!(vm.file_sub, "gemma4.gguf");
        assert_eq!(vm.quant_sub, "Q4_K_M");
        assert!(vm.speed_sub.contains("42.5"));
        assert_eq!(vm.thinking_sub, "Active");
        assert_eq!(vm.vision_action_label.as_deref(), Some("Install Vision"));
        assert_eq!(vm.audio_sub, "Unavailable");
    }

    #[test]
    fn reports_missing_file_and_active_vision_projector() {
        let mut info = sample_info();
        info.file_size = None;
        info.record.mmproj_path = Some("/tmp/mmproj.gguf".to_string());
        info.mmproj_exists = true;
        info.mmproj_gguf = Some(GgufMetadata {
            is_clip: true,
            supports_audio: true,
            ..Default::default()
        });
        let vm = ActiveModelViewModel::from_info(&info);
        assert!(vm.file_sub.contains("Missing file"));
        assert_eq!(vm.vision_sub, "Active");
        assert!(vm.vision_action_label.is_none());
        assert!(vm.audio_sub.contains("Coming soon"));
    }
}
