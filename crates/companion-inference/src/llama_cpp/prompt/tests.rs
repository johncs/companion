use super::budget::trim_turns_to_budget;
use super::templates::{
    ChatTemplateFamily, detect_template_family, format_chatml_fallback, format_gemma4_prompt,
    post_process_template_output, prepare_turns,
};
use companion_core::ThinkingLevel;
use companion_core::provider::{ChatRole, ChatTurn};

fn turn(role: ChatRole, content: &str) -> ChatTurn {
    ChatTurn {
        role,
        content: content.to_string(),
        images: Vec::new(),
    }
}

#[test]
fn detects_chat_template_families() {
    assert_eq!(
        detect_template_family("<|turn>user\n{{ content }}<turn|>"),
        ChatTemplateFamily::Gemma4
    );
    assert_eq!(
        detect_template_family("{% if enable_thinking %}<think>{% endif %}"),
        ChatTemplateFamily::QwenThinking
    );
    assert_eq!(
        detect_template_family("<|im_start|>user\n{{ content }}<|im_end|>"),
        ChatTemplateFamily::Generic
    );
}

#[test]
fn prepares_vision_markers_and_non_vision_notes() {
    let mut msg = turn(ChatRole::User, "Describe this");
    msg.images = vec!["/tmp/a.png".to_string(), "/tmp/b.png".to_string()];

    let with_vision = prepare_turns(
        &[msg.clone()],
        ThinkingLevel::Default,
        true,
        "<__media__>",
        ChatTemplateFamily::Generic,
    );
    assert_eq!(with_vision[0].1, "<__media__>\n<__media__>\nDescribe this");

    let without_vision = prepare_turns(
        &[msg],
        ThinkingLevel::Default,
        false,
        "<__media__>",
        ChatTemplateFamily::Generic,
    );
    assert_eq!(
        without_vision[0].1,
        "[Attached image: /tmp/a.png]\n[Attached image: /tmp/b.png]\nDescribe this"
    );
}

#[test]
fn formats_gemma4_with_and_without_thinking() {
    let msgs = vec![
        turn(ChatRole::System, "Be helpful."),
        turn(ChatRole::User, "Hi"),
        turn(ChatRole::Assistant, "Hello"),
    ];
    let prepared = prepare_turns(
        &msgs,
        ThinkingLevel::Medium,
        false,
        "<__media__>",
        ChatTemplateFamily::Gemma4,
    );
    let (prompt, close_tag) = format_gemma4_prompt(&prepared, ThinkingLevel::Medium);
    assert_eq!(close_tag, Some("<channel|>"));
    assert!(prompt.contains("<|turn>system\n<|think|>\nReasoning effort: medium (balanced)."));
    assert!(prompt.contains("<|turn>model\nHello<turn|>\n"));
    assert!(prompt.ends_with("<|turn>model\n<|channel>thought\n"));

    let prepared_off = prepare_turns(
        &msgs,
        ThinkingLevel::Off,
        false,
        "<__media__>",
        ChatTemplateFamily::Gemma4,
    );
    let (prompt_off, close_off) = format_gemma4_prompt(&prepared_off, ThinkingLevel::Off);
    assert_eq!(close_off, None);
    assert!(prompt_off.ends_with("<|turn>model\n"));
}

#[test]
fn applies_qwen_think_and_no_think_flags() {
    let msgs = vec![turn(ChatRole::User, "Solve 2+2")];
    let on = prepare_turns(
        &msgs,
        ThinkingLevel::High,
        false,
        "<__media__>",
        ChatTemplateFamily::QwenThinking,
    );
    assert_eq!(on[0].0, "system");
    assert!(on[1].1.ends_with(" /think"));

    let off = prepare_turns(
        &msgs,
        ThinkingLevel::Off,
        false,
        "<__media__>",
        ChatTemplateFamily::QwenThinking,
    );
    assert_eq!(off.len(), 1);
    assert_eq!(off[0].1, "Solve 2+2 /no_think");

    let (processed, close) = post_process_template_output(
        "<|im_start|>assistant\n<think>\n\n</think>\n\n".to_string(),
        ChatTemplateFamily::QwenThinking,
        ThinkingLevel::High,
    );
    assert_eq!(close, Some("</think>"));
    assert_eq!(processed, "<|im_start|>assistant\n<think>\n");
}

#[test]
fn formats_chatml_fallback_with_explicit_thinking() {
    let msgs = vec![turn(ChatRole::User, "Ping")];
    let prepared = prepare_turns(
        &msgs,
        ThinkingLevel::Low,
        false,
        "<__media__>",
        ChatTemplateFamily::Generic,
    );
    let (prompt, close) = format_chatml_fallback(&prepared, ThinkingLevel::Low);
    assert_eq!(close, Some("</think>"));
    assert!(prompt.contains("<|im_start|>system\nReasoning effort: low (brief)."));
    assert!(prompt.ends_with("<|im_start|>assistant\n<think>\n"));
}

#[test]
fn trims_oldest_turns_while_preserving_system_and_latest_user() {
    let turns = vec![
        turn(ChatRole::System, &"s".repeat(40)),
        turn(ChatRole::User, &"1".repeat(200)),
        turn(ChatRole::Assistant, &"2".repeat(200)),
        turn(ChatRole::User, &"3".repeat(100)),
        turn(ChatRole::Assistant, &"4".repeat(100)),
        turn(ChatRole::User, &"5".repeat(80)),
    ];
    let kept = trim_turns_to_budget(&turns, 512, |s| s.len());
    assert_eq!(kept.len(), 3);
    assert!(matches!(kept[0].role, ChatRole::System));
    assert!(kept[1].content.starts_with('4'));
    assert!(kept[2].content.starts_with('5'));
}

#[test]
fn keeps_latest_turn_even_when_over_budget() {
    let turns = vec![
        turn(ChatRole::System, "sys"),
        turn(ChatRole::User, "old"),
        turn(ChatRole::User, &"x".repeat(2000)),
    ];
    let kept = trim_turns_to_budget(&turns, 512, |s| s.len());
    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].content, "sys");
    assert_eq!(kept[1].content.len(), 2000);
}

#[test]
fn prepare_turns_merges_existing_system_prompt_idempotently() {
    let generic_msgs = vec![
        turn(ChatRole::System, "You are helpful."),
        turn(ChatRole::User, "Hi"),
    ];
    let generic = prepare_turns(
        &generic_msgs,
        ThinkingLevel::Max,
        false,
        "<__media__>",
        ChatTemplateFamily::Generic,
    );
    assert_eq!(generic.len(), 2);
    assert!(
        generic[0]
            .1
            .starts_with("You are helpful.\n\nReasoning effort: maximum")
    );

    let gemma_msgs = vec![
        turn(ChatRole::System, "<|think|>\nAlready set."),
        turn(ChatRole::User, "Hi"),
    ];
    let gemma = prepare_turns(
        &gemma_msgs,
        ThinkingLevel::Low,
        false,
        "<__media__>",
        ChatTemplateFamily::Gemma4,
    );
    assert_eq!(gemma[0].1, "<|think|>\nAlready set.");
}
