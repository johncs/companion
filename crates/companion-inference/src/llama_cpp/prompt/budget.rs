//! Context-budget turn trimming so long chats stay within `n_ctx`.

use companion_core::provider::{ChatRole, ChatTurn};

/// Pin system turns and the latest non-system turn, then keep as many recent turns as fit
/// inside `n_ctx` minus a reply reserve so long chats continue seamlessly without overflowing.
pub(in crate::llama_cpp) fn trim_turns_to_budget(
    turns: &[ChatTurn],
    n_ctx: u32,
    count_tokens: impl Fn(&str) -> usize,
) -> Vec<ChatTurn> {
    const TURN_FRAMING_OVERHEAD: usize = 8;
    let ctx_budget = n_ctx.max(512);
    let reply_reserve = (ctx_budget / 4).clamp(256, 2048);
    let max_prompt_tokens = (ctx_budget - reply_reserve) as usize;

    let mut system_turns = Vec::new();
    let mut dialog_turns = Vec::new();
    let mut used_tokens = 0usize;

    for turn in turns {
        if matches!(turn.role, ChatRole::System) {
            used_tokens += count_tokens(&turn.content) + TURN_FRAMING_OVERHEAD;
            system_turns.push(turn.clone());
        } else {
            dialog_turns.push(turn);
        }
    }

    let Some((&latest, earlier)) = dialog_turns.split_last() else {
        return system_turns;
    };

    used_tokens += count_tokens(&latest.content) + TURN_FRAMING_OVERHEAD;
    let mut kept_rev = vec![latest.clone()];

    for &turn in earlier.iter().rev() {
        let cost = count_tokens(&turn.content) + TURN_FRAMING_OVERHEAD;
        if used_tokens + cost > max_prompt_tokens {
            break;
        }
        used_tokens += cost;
        kept_rev.push(turn.clone());
    }

    kept_rev.reverse();
    system_turns.extend(kept_rev);
    system_turns
}
