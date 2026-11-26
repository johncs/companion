//! Context-budget turn trimming so long chats stay within `n_ctx`.

use companion_core::provider::{ChatRole, ChatTurn};

const TURN_FRAMING_OVERHEAD: usize = 8;
const TOKENS_PER_IMAGE: usize = 256;

fn turn_token_cost(turn: &ChatTurn, count_tokens: &impl Fn(&str) -> usize) -> usize {
    count_tokens(&turn.content)
        .saturating_add(turn.images.len().saturating_mul(TOKENS_PER_IMAGE))
        .saturating_add(TURN_FRAMING_OVERHEAD)
}

/// Pin system turns and the latest non-system turn, then keep as many recent turns as fit
/// inside `n_ctx` minus a reply reserve so long chats continue seamlessly without overflowing.
pub(in crate::llama_cpp) fn trim_turns_to_budget(
    turns: &[ChatTurn],
    n_ctx: u32,
    count_tokens: impl Fn(&str) -> usize,
) -> Vec<ChatTurn> {
    let ctx_budget = n_ctx.max(512);
    let reply_reserve = (ctx_budget / 4).clamp(256, 2048);
    let max_prompt_tokens = (ctx_budget - reply_reserve) as usize;

    let (mut system_turns, dialog_turns, mut used_tokens) =
        partition_system_turns(turns, &count_tokens);

    let Some((&latest, earlier)) = dialog_turns.split_last() else {
        return system_turns;
    };

    used_tokens = used_tokens.saturating_add(turn_token_cost(latest, &count_tokens));
    let mut kept_rev = vec![latest.clone()];

    for &turn in earlier.iter().rev() {
        let cost = turn_token_cost(turn, &count_tokens);
        if used_tokens.saturating_add(cost) > max_prompt_tokens {
            break;
        }
        used_tokens = used_tokens.saturating_add(cost);
        kept_rev.push(turn.clone());
    }

    drop_leading_assistant_turns(&mut kept_rev);
    kept_rev.reverse();
    system_turns.extend(kept_rev);
    system_turns
}

fn partition_system_turns<'a>(
    turns: &'a [ChatTurn],
    count_tokens: &impl Fn(&str) -> usize,
) -> (Vec<ChatTurn>, Vec<&'a ChatTurn>, usize) {
    let mut system_turns = Vec::new();
    let mut dialog_turns = Vec::new();
    let mut used_tokens = 0usize;
    for turn in turns {
        if matches!(turn.role, ChatRole::System) {
            used_tokens = used_tokens.saturating_add(turn_token_cost(turn, count_tokens));
            system_turns.push(turn.clone());
        } else {
            dialog_turns.push(turn);
        }
    }
    (system_turns, dialog_turns, used_tokens)
}

fn drop_leading_assistant_turns(kept_rev: &mut Vec<ChatTurn>) {
    // Drop orphaned leading Assistant turns after trimming so strict Jinja templates don't reject them.
    while kept_rev.len() > 1
        && kept_rev
            .last()
            .is_some_and(|t| matches!(t.role, ChatRole::Assistant))
    {
        kept_rev.pop();
    }
}
