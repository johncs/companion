use std::path::{Path, PathBuf};

use super::*;

#[test]
fn derive_query_strips_extension_and_quantization() {
    assert_eq!(
        derive_hf_search_query("gemma-4-e4b-it-q4_k_m.gguf"),
        "gemma-4-e4b-it"
    );
    assert_eq!(
        derive_hf_search_query("Qwen3-VL-8B-Instruct-UD-Q4_K_XL.gguf"),
        "Qwen3-VL-8B-Instruct"
    );
    assert_eq!(
        derive_hf_search_query("Llama-3.2-11B-Vision-Instruct.Q4_K_M.gguf"),
        "Llama-3.2-11B-Vision-Instruct"
    );
}

#[test]
fn score_prefers_matching_parameter_size() {
    let query = "gemma-4-e4b-it";
    let exact = score_repo_match("unsloth/gemma-4-e4b-it-GGUF", query);
    let wrong_size = score_repo_match("unsloth/gemma-4-27b-it-GGUF", query);
    assert!(exact > wrong_size);
}

#[test]
fn select_best_mmproj_prefers_bf16_or_f16_over_f32() {
    let files = vec![
        HfGgufFile {
            filename: "gemma-4-e4b-it-Q4_K_M.gguf".to_string(),
            size_bytes: 2_500_000_000,
        },
        HfGgufFile {
            filename: "mmproj-F32.gguf".to_string(),
            size_bytes: 1_700_000_000,
        },
        HfGgufFile {
            filename: "mmproj-BF16.gguf".to_string(),
            size_bytes: 850_000_000,
        },
    ];
    let best = select_best_mmproj(&files).expect("expected mmproj");
    assert_eq!(best.filename, "mmproj-BF16.gguf");
}

#[test]
fn select_starter_gguf_prefers_q4_k_m_and_pairs_mmproj() {
    let files = vec![
        HfGgufFile {
            filename: "mtp-gemma-4-E2B-it-Q4_K_M.gguf".to_string(),
            size_bytes: 500_000_000,
        },
        HfGgufFile {
            filename: "gemma-4-E2B-it-Q8_0.gguf".to_string(),
            size_bytes: 5_000_000_000,
        },
        HfGgufFile {
            filename: "gemma-4-E2B-it-Q4_K_M.gguf".to_string(),
            size_bytes: 3_100_000_000,
        },
        HfGgufFile {
            filename: "mmproj-gemma-4-E2B-it-Q8_0.gguf".to_string(),
            size_bytes: 450_000_000,
        },
    ];
    let (model, mmproj) = select_starter_gguf(&files).expect("expected starter pair");
    assert_eq!(model.filename, "gemma-4-E2B-it-Q4_K_M.gguf");
    assert_eq!(
        mmproj.map(|m| m.filename).as_deref(),
        Some("mmproj-gemma-4-E2B-it-Q8_0.gguf")
    );
}

#[test]
fn part_file_path_appends_part_without_clobbering_non_gguf_extensions() {
    assert_eq!(
        part_file_path(Path::new("/tmp/model.gguf")),
        PathBuf::from("/tmp/model.gguf.part")
    );
    assert_eq!(
        part_file_path(Path::new("/tmp/model.bin")),
        PathBuf::from("/tmp/model.bin.part")
    );
    assert_eq!(
        part_file_path(Path::new("/tmp/weights")),
        PathBuf::from("/tmp/weights.part")
    );
}

#[test]
fn hf_client_parses_mock_search_and_tree_responses() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]);
            let body = if req.contains("/api/models/org/repo/tree/main") {
                r#"[{"type":"file","path":"m-Q4_K_M.gguf","size":123},{"type":"file","path":"README.md","size":10}]"#
            } else {
                r#"[{"id":"org/repo","downloads":42,"likes":7}]"#
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        }
    });

    let client = HfClient::with_base_url(format!("http://{addr}"));
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let repos = client.search_gguf_repos("gemma", 5).await.unwrap();
            assert_eq!(repos.len(), 1);
            assert_eq!(repos[0].id, "org/repo");
            assert_eq!(repos[0].downloads, 42);

            let files = client.list_repo_gguf_files("org/repo").await.unwrap();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].filename, "m-Q4_K_M.gguf");
            assert_eq!(files[0].size_bytes, 123);
        });
    server.join().unwrap();
}

fn spawn_once_http(response: Vec<u8>) -> String {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(&response);
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn download_gguf_rejects_truncated_eof_and_succeeds_on_exact_200() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let dir = std::env::temp_dir().join(format!("companion_hf_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();

    let trunc_url = spawn_once_http(
        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n1234".to_vec(),
    );
    let client = HfClient::with_base_url(trunc_url);
    let dest = dir.join("model.gguf");
    let res = client
        .download_gguf_file(
            "org/repo",
            "model.gguf",
            &dest,
            Arc::new(AtomicBool::new(false)),
            |_, _| {},
        )
        .await;
    assert!(res.is_err());
    assert!(!dest.exists());

    let ok_url = spawn_once_http(
        b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nGGUF".to_vec(),
    );
    let ok_client = HfClient::with_base_url(ok_url);
    let dest_ok = dir.join("ok.gguf");
    ok_client
        .download_gguf_file(
            "org/repo",
            "ok.gguf",
            &dest_ok,
            Arc::new(AtomicBool::new(false)),
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest_ok).unwrap(), b"GGUF");
    let _ = std::fs::remove_dir_all(&dir);
}
