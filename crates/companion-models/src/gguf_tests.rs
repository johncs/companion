use std::io::Cursor;

use super::*;

fn push_str(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
    buf.extend_from_slice(s.as_bytes());
}

fn sample_gguf_bytes(arch: &str, ctx: u32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"GGUF");
    buf.extend_from_slice(&3u32.to_le_bytes());
    buf.extend_from_slice(&0u64.to_le_bytes());
    buf.extend_from_slice(&3u64.to_le_bytes());

    push_str(&mut buf, "general.architecture");
    buf.extend_from_slice(&8u32.to_le_bytes());
    push_str(&mut buf, arch);

    push_str(&mut buf, &format!("{arch}.context_length"));
    buf.extend_from_slice(&4u32.to_le_bytes());
    buf.extend_from_slice(&ctx.to_le_bytes());

    push_str(&mut buf, "tokenizer.chat_template");
    buf.extend_from_slice(&8u32.to_le_bytes());
    push_str(
        &mut buf,
        "{% if enable_thinking %}<|channel>thought{% endif %}",
    );
    buf
}

#[test]
fn parses_gguf_context_length_and_chat_template() {
    let buf = sample_gguf_bytes("gemma4", 131072);
    let meta = parse_gguf_reader(&mut Cursor::new(buf)).expect("valid GGUF header");
    assert_eq!(meta.max_context, Some(131072));
    assert!(meta.supports_thinking);
    assert!(!meta.is_clip);
}

#[test]
fn detects_clip_projector_architecture() {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"GGUF");
    buf.extend_from_slice(&3u32.to_le_bytes());
    buf.extend_from_slice(&0u64.to_le_bytes());
    buf.extend_from_slice(&1u64.to_le_bytes());

    push_str(&mut buf, "general.architecture");
    buf.extend_from_slice(&8u32.to_le_bytes());
    push_str(&mut buf, "clip");

    let meta = parse_gguf_reader(&mut Cursor::new(buf)).expect("valid GGUF header");
    assert!(meta.is_clip);
}

#[test]
fn metadata_cache_invalidates_when_file_changes() {
    let path = std::env::temp_dir().join(format!(
        "companion_gguf_cache_{}.gguf",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&path, sample_gguf_bytes("llama", 8192)).unwrap();

    let cache = GgufMetadataCache::new();
    assert_eq!(cache.inspect(&path).max_context, Some(8192));

    // Rewrite with a different size & context length so signature changes deterministically.
    std::fs::write(&path, sample_gguf_bytes("qwen25vl", 32768)).unwrap();
    let updated = cache.inspect(&path);
    assert_eq!(updated.max_context, Some(32768));
    assert!(updated.supports_vision);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn builds_friendly_context_presets_capped_to_max() {
    assert_eq!(
        context_presets_up_to(131072),
        vec![2048, 4096, 8192, 16384, 32768, 65536, 131072]
    );
    assert_eq!(
        context_presets_up_to(40960),
        vec![2048, 4096, 8192, 16384, 32768, 40960]
    );
    assert_eq!(format_context_label(131072), "128K (131,072)");
    assert_eq!(format_context_label(1048576), "1M (1,048,576)");
    assert_eq!(format_short_context(10485760), "10M");
}

#[test]
fn detects_vision_and_audio_capabilities_from_templates() {
    assert!(template_supports_vision("<start_of_image>describe"));
    assert!(template_supports_vision("type == 'image_url'"));
    assert!(!template_supports_vision("You are a helpful assistant."));

    assert!(template_supports_audio("<start_of_audio>transcribe"));
    assert!(template_supports_audio("<|audio|>"));
    assert!(!template_supports_audio("You are a helpful assistant."));
}
