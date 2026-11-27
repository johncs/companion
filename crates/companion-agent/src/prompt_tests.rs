use chrono::NaiveDate;
use companion_core::AssistantPersonality;

use super::*;

fn sample_personalization() -> PersonalizationSettings {
    PersonalizationSettings {
        assistant_name: "Companion".to_string(),
        user_name: Some("Ada".to_string()),
        personality: AssistantPersonality::Friendly,
        custom_personality: None,
        user_about: None,
    }
}

#[test]
fn builds_prompt_turns_with_folders_web_search_attachments_and_mentions() {
    let dir = std::env::temp_dir().join(format!("companion_pt_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let mentioned = dir.join("README.md");
    std::fs::write(&mentioned, "hello").unwrap();

    let prefs = sample_personalization();
    let history = vec![MessageWithAttachments {
        role: MessageRole::User,
        content: format!("Check @{}", mentioned.display()),
        attachments: vec![
            StagedAttachment {
                file_name: "photo.png".to_string(),
                mime_type: "image/png".to_string(),
                file_path: "/tmp/photo.png".to_string(),
                extracted_text: None,
                byte_size: 100,
            },
            StagedAttachment {
                file_name: "notes.txt".to_string(),
                mime_type: "text/plain".to_string(),
                file_path: "/tmp/notes.txt".to_string(),
                extracted_text: Some("note body".to_string()),
                byte_size: 9,
            },
        ],
    }];

    let reg = ToolRegistry::with_builtins(std::sync::Arc::new(
        crate::web_search::WebSearchService::new().expect("web search"),
    ));
    let turns = build_prompt_turns(&PromptContext {
        chat_system_prompt: "Be helpful.",
        personalization: &prefs,
        granted_folders: &[GrantedFolder {
            path: dir.clone(),
            allow_write: true,
        }],
        web_search_active: true,
        history: &history,
        registry: &reg,
    });

    assert_eq!(turns.len(), 2);
    let sys = &turns[0].content;
    assert!(sys.contains("Current date:"));
    assert!(sys.contains("[directory, read & write]"));
    assert!(sys.contains("Web Search is enabled"));
    assert!(sys.contains("User-mentioned OS paths"));
    assert_eq!(turns[1].images, vec!["/tmp/photo.png".to_string()]);
    assert!(turns[1].content.contains("note body"));

    let empty_turns = build_prompt_turns(&PromptContext {
        chat_system_prompt: "",
        personalization: &prefs,
        granted_folders: &[],
        web_search_active: false,
        history: &[],
        registry: &reg,
    });
    assert!(
        empty_turns[0]
            .content
            .contains("No folders or files are currently granted")
    );
    assert!(!empty_turns[0].content.contains("Web Search is enabled"));

    let att_only_path = vec![MessageWithAttachments {
        role: MessageRole::User,
        content: "Summarize this attachment".to_string(),
        attachments: vec![StagedAttachment {
            file_name: "note.txt".to_string(),
            mime_type: "text/plain".to_string(),
            file_path: "/tmp/note.txt".to_string(),
            extracted_text: Some(format!("Inside attachment: @{}", dir.display())),
            byte_size: 32,
        }],
    }];
    let att_turns = build_prompt_turns(&PromptContext {
        chat_system_prompt: "",
        personalization: &prefs,
        granted_folders: &[],
        web_search_active: false,
        history: &att_only_path,
        registry: &reg,
    });
    assert!(!att_turns[0].content.contains("User-mentioned OS paths"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn attaches_granted_mentioned_images_only_and_read_file_hints_images() {
    let base = std::env::temp_dir().join(format!("companion_img_{}", uuid::Uuid::new_v4()));
    let (granted, ungranted) = (base.join("granted"), base.join("ungranted"));
    std::fs::create_dir_all(&granted).unwrap();
    std::fs::create_dir_all(&ungranted).unwrap();
    let ok_img = granted.join("car.jpg");
    let blocked_img = ungranted.join("secret.jpg");
    std::fs::write(&ok_img, b"\xff\xd8\xff\x00jpeg").unwrap();
    std::fs::write(&blocked_img, b"\xff\xd8\xff\x00jpeg").unwrap();

    let err = crate::tools::read_granted_file(&granted, "car.jpg").unwrap_err();
    assert!(err.to_string().contains("is an image file"));

    let turns = build_prompt_turns(&PromptContext {
        chat_system_prompt: "",
        personalization: &sample_personalization(),
        granted_folders: &[GrantedFolder {
            path: granted.clone(),
            allow_write: false,
        }],
        web_search_active: false,
        history: &[MessageWithAttachments {
            role: MessageRole::User,
            content: format!(
                "Compare @{} and @{}",
                ok_img.display(),
                blocked_img.display()
            ),
            attachments: Vec::new(),
        }],
        registry: &ToolRegistry::with_builtins(std::sync::Arc::new(
            crate::web_search::WebSearchService::new().unwrap(),
        )),
    });
    let canon_ok = std::fs::canonicalize(&ok_img).unwrap();
    let canon_blocked = std::fs::canonicalize(&blocked_img).unwrap();
    assert_eq!(
        turns[1].images,
        vec![canon_ok.to_string_lossy().to_string()]
    );
    assert!(
        !turns[0]
            .content
            .contains(&canon_ok.to_string_lossy().to_string())
    );
    assert!(
        turns[0]
            .content
            .contains(&canon_blocked.to_string_lossy().to_string())
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn formats_and_injects_current_local_date_deterministically() {
    let date = NaiveDate::from_ymd_opt(2026, 11, 27).unwrap();
    let formatted = format_local_date(date);
    assert_eq!(formatted, "2026-11-27 (Friday)");

    let reg = ToolRegistry::with_builtins(std::sync::Arc::new(
        crate::web_search::WebSearchService::new().unwrap(),
    ));
    let turns = build_prompt_turns_with_date(
        &PromptContext {
            chat_system_prompt: "",
            personalization: &sample_personalization(),
            granted_folders: &[],
            web_search_active: false,
            history: &[],
            registry: &reg,
        },
        &formatted,
    );
    assert!(
        turns[0]
            .content
            .contains("Current date: 2026-11-27 (Friday)")
    );
}
