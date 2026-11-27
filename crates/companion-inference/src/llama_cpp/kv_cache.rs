//! KV-cache prefix reuse across multi-turn text inference.

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::token::LlamaToken;
use llama_cpp_2::{LlamaStateSeqFlags, SeqState};

use super::LlamaCppProvider;

pub(crate) struct KvPrefixCache {
    pub(super) ctx_size: u32,
    pub(super) tokens: Vec<LlamaToken>,
    pub(super) seq_state: SeqState,
}

impl LlamaCppProvider {
    pub(super) fn take_kv_cache(&self) -> Option<KvPrefixCache> {
        self.loaded
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().and_then(|l| l.kv_cache.take()))
    }

    pub(super) fn store_kv_cache(&self, cache: KvPrefixCache) {
        if let Ok(mut guard) = self.loaded.lock()
            && let Some(loaded) = guard.as_mut()
        {
            loaded.kv_cache = Some(cache);
        }
    }
}

/// Returns how many leading tokens of `next` can be reused from `cached`,
/// always leaving at least 1 trailing token so the final token is decoded with logits.
pub(super) fn compute_reusable_prefix<T: PartialEq>(cached: &[T], next: &[T]) -> usize {
    let shared = cached
        .iter()
        .zip(next.iter())
        .take_while(|(a, b)| a == b)
        .count();
    if shared == next.len() {
        shared.saturating_sub(1)
    } else {
        shared
    }
}

pub(super) fn restore_kv_prefix(
    ctx: &mut LlamaContext<'_>,
    cached: Option<KvPrefixCache>,
    ctx_size: u32,
    next_tokens: &[LlamaToken],
) -> usize {
    let Some(cached) = cached else {
        return 0;
    };
    if cached.ctx_size != ctx_size {
        return 0;
    }
    let prefix_len = compute_reusable_prefix(&cached.tokens, next_tokens);
    if prefix_len == 0 || ctx.state_seq_set(&cached.seq_state, 0).is_err() {
        ctx.clear_kv_cache();
        return 0;
    }
    if prefix_len < cached.tokens.len() {
        let p0 = u32::try_from(prefix_len).unwrap_or(u32::MAX);
        if !matches!(ctx.clear_kv_cache_seq(Some(0), Some(p0), None), Ok(true)) {
            ctx.clear_kv_cache();
            return 0;
        }
    }
    prefix_len
}

pub(super) fn save_kv_prefix(
    provider: &LlamaCppProvider,
    ctx: &LlamaContext<'_>,
    ctx_size: u32,
    evaluated_tokens: Vec<LlamaToken>,
) {
    if evaluated_tokens.is_empty() {
        return;
    }
    if let Ok(seq_state) = ctx.state_seq_get(0, LlamaStateSeqFlags::default()) {
        provider.store_kv_cache(KvPrefixCache {
            ctx_size,
            tokens: evaluated_tokens,
            seq_state,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::compute_reusable_prefix;

    #[test]
    fn computes_reusable_prefix_for_multi_turn_and_diverged_prompts() {
        assert_eq!(compute_reusable_prefix(&[1, 2, 3], &[4, 5]), 0);
        assert_eq!(compute_reusable_prefix(&[1, 2, 3], &[1, 2, 3, 4, 5]), 3);
        assert_eq!(compute_reusable_prefix(&[1, 2, 3, 4], &[1, 2, 9, 10]), 2);
        // Identical prompt leaves at least 1 trailing token for logits sampling.
        assert_eq!(compute_reusable_prefix(&[1, 2, 3], &[1, 2, 3]), 2);
        assert_eq!(compute_reusable_prefix::<i32>(&[1], &[]), 0);
    }
}
