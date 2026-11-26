//! Streaming reasoning-tag parser (`<think>`, `<thought>`, Gemma 4 `<|channel>thought`).

const TAG_PAIRS: &[(&str, &str)] = &[
    ("<think>", "</think>"),
    ("<thought>", "</thought>"),
    ("<|thought|>", "<|/thought|>"),
    ("<|channel>thought\n", "<channel|>"),
    ("<|channel>thought", "<channel|>"),
];

/// Incremental parser that separates reasoning blocks from visible answer text,
/// handling tags split across arbitrary token boundaries.
#[derive(Debug, Default)]
pub struct ThinkStreamParser {
    active_close_tag: Option<&'static str>,
    buffer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedChunk {
    Answer(String),
    Reasoning(String),
}

impl ThinkStreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a parser that is already inside a reasoning block (when the prompt primed the open tag).
    pub fn starting_in_think(close_tag: &'static str) -> Self {
        Self {
            active_close_tag: Some(close_tag),
            buffer: String::new(),
        }
    }

    /// Feed a newly decoded token chunk and emit any complete answer/reasoning segments.
    pub fn push(&mut self, token: &str) -> Vec<ParsedChunk> {
        self.buffer.push_str(token);
        let mut out = Vec::new();

        loop {
            if let Some(close_tag) = self.active_close_tag {
                if let Some(end_idx) = self.buffer.find(close_tag) {
                    let reasoning = self.buffer[..end_idx].to_string();
                    if !reasoning.is_empty() {
                        out.push(ParsedChunk::Reasoning(reasoning));
                    }
                    self.buffer.drain(..end_idx + close_tag.len());
                    self.active_close_tag = None;
                } else {
                    // Keep trailing chars that could be a partial close tag prefix.
                    let safe = safe_prefix_len(&self.buffer, &[close_tag]);
                    if safe > 0 {
                        let reasoning = self.buffer[..safe].to_string();
                        self.buffer.drain(..safe);
                        out.push(ParsedChunk::Reasoning(reasoning));
                    }
                    break;
                }
            } else if let Some((start_idx, open_tag, close_tag)) = find_earliest_open(&self.buffer)
            {
                let answer = self.buffer[..start_idx].to_string();
                if !answer.is_empty() {
                    out.push(ParsedChunk::Answer(answer));
                }
                self.buffer.drain(..start_idx + open_tag.len());
                self.active_close_tag = Some(close_tag);
            } else {
                let open_tags: Vec<&str> = TAG_PAIRS.iter().map(|(o, _)| *o).collect();
                let safe = safe_prefix_len(&self.buffer, &open_tags);
                if safe > 0 {
                    let answer = self.buffer[..safe].to_string();
                    self.buffer.drain(..safe);
                    out.push(ParsedChunk::Answer(answer));
                }
                break;
            }
        }

        out
    }

    /// Flush any remaining buffered characters at end-of-stream.
    pub fn finish(&mut self) -> Vec<ParsedChunk> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let rest = std::mem::take(&mut self.buffer);
        if self.active_close_tag.take().is_some() {
            vec![ParsedChunk::Reasoning(rest)]
        } else {
            vec![ParsedChunk::Answer(rest)]
        }
    }
}

fn find_earliest_open(buf: &str) -> Option<(usize, &'static str, &'static str)> {
    let mut best: Option<(usize, &'static str, &'static str)> = None;
    for &(open, close) in TAG_PAIRS {
        if let Some(idx) = buf.find(open) {
            let replace = match best {
                None => true,
                Some((best_idx, best_open, _)) => {
                    idx < best_idx || (idx == best_idx && open.len() > best_open.len())
                }
            };
            if replace {
                best = Some((idx, open, close));
            }
        }
    }
    best
}

/// Return how many leading bytes of `buf` cannot possibly be the start of any `tags`.
fn safe_prefix_len(buf: &str, tags: &[&str]) -> usize {
    let mut earliest = buf.len();
    for &tag in tags {
        let max_tail = (tag.len().saturating_sub(1)).min(buf.len());
        for tail_len in (1..=max_tail).rev() {
            let start = buf.len() - tail_len;
            if buf.is_char_boundary(start) && tag.starts_with(&buf[start..]) {
                earliest = earliest.min(start);
                break;
            }
        }
    }
    earliest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_split_think_tags_across_tokens() {
        let mut p = ThinkStreamParser::new();
        let mut reasoning = String::new();
        let mut answer = String::new();

        for tok in ["<thi", "nk>let's ", "plan</th", "ink>Hello ", "world!"] {
            for chunk in p.push(tok) {
                match chunk {
                    ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                    ParsedChunk::Answer(a) => answer.push_str(&a),
                }
            }
        }
        for chunk in p.finish() {
            match chunk {
                ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                ParsedChunk::Answer(a) => answer.push_str(&a),
            }
        }

        assert_eq!(reasoning, "let's plan");
        assert_eq!(answer, "Hello world!");
    }

    #[test]
    fn parses_gemma4_channel_thought_tags() {
        let mut p = ThinkStreamParser::new();
        let mut reasoning = String::new();
        let mut answer = String::new();

        for tok in [
            "<|chan",
            "nel>thought\n",
            "check facts",
            "<chan",
            "nel|>Final answer.",
        ] {
            for chunk in p.push(tok) {
                match chunk {
                    ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                    ParsedChunk::Answer(a) => answer.push_str(&a),
                }
            }
        }
        for chunk in p.finish() {
            match chunk {
                ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                ParsedChunk::Answer(a) => answer.push_str(&a),
            }
        }

        assert_eq!(reasoning, "check facts");
        assert_eq!(answer, "Final answer.");
    }

    #[test]
    fn handles_unclosed_tags_and_multibyte_utf8() {
        let mut p = ThinkStreamParser::new();
        let mut reasoning = String::new();
        let mut answer = String::new();

        for tok in ["🦀<think>σκέψη ", "σε εξέλιξη"] {
            for chunk in p.push(tok) {
                match chunk {
                    ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                    ParsedChunk::Answer(a) => answer.push_str(&a),
                }
            }
        }
        for chunk in p.finish() {
            match chunk {
                ParsedChunk::Reasoning(r) => reasoning.push_str(&r),
                ParsedChunk::Answer(a) => answer.push_str(&a),
            }
        }

        assert_eq!(answer, "🦀");
        assert_eq!(reasoning, "σκέψη σε εξέλιξη");
    }
}
