//! Core domain logic: Markdown rendering, thinking levels, paths, and shared types.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod i18n;
pub mod ids;
pub mod model_params;
pub mod model_types;
pub mod paths;
pub mod personality;
pub mod prompt;
pub mod provider;
pub mod thinking;
pub mod types;

pub use personality::AssistantPersonality;
pub use thinking::ThinkingLevel;

/// Process `items` in parallel across CPU cores (up to 8 workers) when `items.len() >= min_parallel`.
pub fn parallel_chunks<T, R, F>(items: &[T], min_parallel: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&[T]) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() < min_parallel {
        return vec![f(items)];
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8)
        .min(items.len());
    let chunk_size = items.len().div_ceil(workers).max(1);

    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(workers);
        for chunk in items.chunks(chunk_size) {
            handles.push(s.spawn(|| f(chunk)));
        }
        handles
            .into_iter()
            .map(|h| match h.join() {
                Ok(v) => v,
                Err(payload) => std::panic::resume_unwind(payload),
            })
            .collect()
    })
}

/// Substitute `{key}` placeholders in `template` in a single left-to-right pass so
/// replacement values containing `{other_key}` are never re-expanded.
pub fn render_placeholders(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        if let Some(close) = after_open.find('}') {
            let key = &after_open[..close];
            if let Some((_, val)) = vars.iter().find(|(k, _)| *k == key) {
                out.push_str(val);
                rest = &after_open[close + 1..];
                continue;
            }
        }
        out.push('{');
        rest = after_open;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_placeholders_is_single_pass_and_parallel_chunks_propagates_panics() {
        let out = render_placeholders(
            "Hello {user_name}, I am {assistant_name}.",
            &[
                ("user_name", "{assistant_name}"),
                ("assistant_name", "Emma"),
            ],
        );
        assert_eq!(out, "Hello {assistant_name}, I am Emma.");

        let rendered = crate::prompt::compose_personalized_system_prompt(
            "Hi {user_name}",
            &crate::prompt::PersonalizationProfile {
                assistant_name: "Emma",
                user_name: Some("   "),
                personality: AssistantPersonality::Friendly,
                custom_personality: None,
                user_about: None,
            },
        );
        assert!(rendered.contains("Hi the user"));

        let res = std::panic::catch_unwind(|| {
            parallel_chunks(&[1, 2, 3, 4], 1, |chunk| {
                if chunk.contains(&3) {
                    panic!("worker boom");
                }
                chunk.len()
            })
        });
        assert!(res.is_err());
    }
}
