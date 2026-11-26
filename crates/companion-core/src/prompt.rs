//! Pure system prompt template rendering and personalization composition.

use crate::AssistantPersonality;

pub const CORE_SYSTEM_RULES: &str = "Core rules: Be accurate, concise, and honest. If you are unsure or lack information, say so directly instead of guessing. Never fabricate file contents, command outputs, or tool results. Reply in the user's language.";
pub const DEFAULT_SYSTEM_PROMPT: &str = "";

/// Return the default per-chat system prompt template.
pub fn default_system_prompt() -> &'static str {
    DEFAULT_SYSTEM_PROMPT
}

/// Expand `{assistant_name}` and `{user_name}` placeholders (and legacy `"You are Companion,"` defaults).
pub fn render_system_prompt(
    template: &str,
    assistant_name: &str,
    user_name: Option<&str>,
) -> String {
    let mut out = template
        .replace("{assistant_name}", assistant_name)
        .replace("{user_name}", user_name.unwrap_or("the user"));
    if assistant_name != "Companion" && out.starts_with("You are Companion,") {
        out = out.replacen(
            "You are Companion,",
            &format!("You are {assistant_name},"),
            1,
        );
    }
    out
}

/// Compose the full system prompt with core rules, personalized character/name, optional user context,
/// and per-chat instructions.
pub fn compose_personalized_system_prompt(
    chat_prompt: &str,
    assistant_name: &str,
    user_name: Option<&str>,
    personality: AssistantPersonality,
    custom_personality: Option<&str>,
    user_about: Option<&str>,
) -> String {
    let effective_assistant = match assistant_name.trim() {
        "" => "Companion",
        s => s,
    };
    let clean_user = user_name.map(str::trim).filter(|s| !s.is_empty());
    let mut layers = vec![
        CORE_SYSTEM_RULES.to_string(),
        format!("Your name is {effective_assistant}."),
    ];
    if let Some(uname) = clean_user {
        layers.push(format!("The user's name is {uname}."));
    }
    layers.push(personality.resolve_guidance(effective_assistant, clean_user, custom_personality));
    if let Some(about) = user_about.map(str::trim).filter(|s| !s.is_empty()) {
        let who = clean_user.unwrap_or("the user");
        layers.push(format!("About {who}: {about}"));
    }

    let rendered_chat = render_system_prompt(chat_prompt, effective_assistant, user_name);
    let trimmed_chat = rendered_chat.trim();
    if !trimmed_chat.is_empty() {
        layers.push(trimmed_chat.to_string());
    }

    layers.join("\n\n")
}
