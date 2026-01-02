//! End-to-end integration tests for the upload flow.
//!
//! These tests require a running worker at localhost:8787.
//! Run with: cargo test --test e2e -- --ignored

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use flate2::read::GzDecoder;
use std::io::Read;

/// Test the full encrypt -> upload -> fetch -> decrypt roundtrip
#[test]
#[ignore] // Requires running worker
fn test_e2e_roundtrip() {
    let worker_url = std::env::var("WORKER_URL")
        .unwrap_or_else(|_| "http://localhost:8787".to_string());

    // Create test HTML content
    let test_html = r#"<!DOCTYPE html>
<html>
<head><title>Test</title></head>
<body>
<h1>E2E Test Content</h1>
<p>This is a test transcript with unique marker: AGENTEXPORT_E2E_TEST_MARKER_12345</p>
</body>
</html>"#;

    // Encrypt using the same logic as the CLI
    let encrypted = encrypt_html(test_html).expect("encryption failed");

    println!("Encrypted blob size: {} bytes", encrypted.blob.len());
    println!("Key (base64url): {}", encrypted.key_b64);

    // Upload to worker
    let upload_url = format!("{}/upload", worker_url);
    let response = ureq::post(&upload_url)
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&encrypted.blob)
        .expect("upload failed");

    assert!(response.status() < 400, "Upload failed with status {}", response.status());

    let upload_response: serde_json::Value = response.into_json().expect("parse response");
    let id = upload_response["id"].as_str().expect("missing id");

    println!("Uploaded with ID: {}", id);

    // Fetch blob back
    let blob_url = format!("{}/blob/{}", worker_url, id);
    let response = ureq::get(&blob_url)
        .call()
        .expect("fetch blob failed");

    assert_eq!(response.status(), 200);

    let mut fetched_blob = Vec::new();
    response.into_reader().read_to_end(&mut fetched_blob).expect("read blob");

    println!("Fetched blob size: {} bytes", fetched_blob.len());
    assert_eq!(fetched_blob, encrypted.blob, "Fetched blob doesn't match uploaded");

    // Decrypt
    let decrypted = decrypt_blob(&fetched_blob, &encrypted.key_b64)
        .expect("decryption failed");

    println!("Decrypted size: {} bytes", decrypted.len());

    // Verify content
    assert!(
        decrypted.contains("AGENTEXPORT_E2E_TEST_MARKER_12345"),
        "Decrypted content doesn't contain test marker"
    );

    println!("\n✓ E2E roundtrip test PASSED!");
}

/// Test that viewer page is served
#[test]
#[ignore]
fn test_viewer_page_served() {
    let worker_url = std::env::var("WORKER_URL")
        .unwrap_or_else(|_| "http://localhost:8787".to_string());

    // First upload something
    let test_html = "<html><body>test</body></html>";
    let encrypted = encrypt_html(test_html).unwrap();

    let response = ureq::post(&format!("{}/upload", worker_url))
        .set("Content-Type", "application/octet-stream")
        .send_bytes(&encrypted.blob)
        .unwrap();

    let upload_response: serde_json::Value = response.into_json().unwrap();
    let id = upload_response["id"].as_str().unwrap();

    // Fetch viewer page
    let viewer_url = format!("{}/v/{}", worker_url, id);
    let response = ureq::get(&viewer_url).call().unwrap();

    assert_eq!(response.status(), 200);

    let html = response.into_string().unwrap();
    assert!(html.contains("<!DOCTYPE html>"));
    assert!(html.contains("Decrypting transcript"));
    assert!(html.contains("crypto.subtle.decrypt"));

    println!("✓ Viewer page test PASSED!");
}

/// Test 404 for non-existent blob
#[test]
#[ignore]
fn test_blob_not_found() {
    let worker_url = std::env::var("WORKER_URL")
        .unwrap_or_else(|_| "http://localhost:8787".to_string());

    let response = ureq::get(&format!("{}/blob/0000000000000000", worker_url))
        .call();

    match response {
        Err(ureq::Error::Status(404, _)) => println!("✓ 404 test PASSED!"),
        other => panic!("Expected 404, got {:?}", other),
    }
}

// Helper: encrypt HTML (mirrors src/crypto.rs)
struct EncryptionResult {
    blob: Vec<u8>,
    key_b64: String,
}

fn encrypt_html(html: &str) -> Result<EncryptionResult, Box<dyn std::error::Error>> {
    use flate2::{write::GzEncoder, Compression};
    use rand::RngCore;
    use std::io::Write;

    // Compress
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(html.as_bytes())?;
    let compressed = encoder.finish()?;

    // Generate key and IV
    let mut key_bytes = [0u8; 32];
    let mut iv_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    rand::thread_rng().fill_bytes(&mut iv_bytes);

    // Encrypt
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)?;
    let nonce = Nonce::from_slice(&iv_bytes);
    let ciphertext = cipher.encrypt(nonce, compressed.as_slice())
        .map_err(|e| format!("encryption failed: {}", e))?;

    // Combine IV + ciphertext
    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&iv_bytes);
    blob.extend_from_slice(&ciphertext);

    let key_b64 = URL_SAFE_NO_PAD.encode(key_bytes);

    Ok(EncryptionResult { blob, key_b64 })
}

// Helper: decrypt blob
fn decrypt_blob(blob: &[u8], key_b64: &str) -> Result<String, Box<dyn std::error::Error>> {
    let key_bytes = URL_SAFE_NO_PAD.decode(key_b64)?;

    let iv = &blob[..12];
    let ciphertext = &blob[12..];

    let cipher = Aes256Gcm::new_from_slice(&key_bytes)?;
    let nonce = Nonce::from_slice(iv);
    let compressed = cipher.decrypt(nonce, ciphertext)
        .map_err(|e| format!("decryption failed: {}", e))?;

    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut html = String::new();
    decoder.read_to_string(&mut html)?;

    Ok(html)
}
