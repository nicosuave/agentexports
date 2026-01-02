//! End-to-end integration tests for the upload flow.
//!
//! These tests require a running worker at localhost:8787.
//! Run with: cargo test --test e2e -- --ignored

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::io::Read;

/// Test the full encrypt -> upload -> fetch -> decrypt roundtrip
#[test]
#[ignore] // Requires running worker
fn test_e2e_roundtrip() {
    let worker_url =
        std::env::var("WORKER_URL").unwrap_or_else(|_| "http://localhost:8787".to_string());

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
    let key_hash = compute_key_hash(&encrypted.key_b64);
    let response = ureq::post(&upload_url)
        .set("Content-Type", "application/octet-stream")
        .set("X-Key-Hash", &key_hash)
        .send_bytes(&encrypted.blob)
        .expect("upload failed");

    assert!(
        response.status() < 400,
        "Upload failed with status {}",
        response.status()
    );

    let upload_response: serde_json::Value = response.into_json().expect("parse response");
    let id = upload_response["id"].as_str().expect("missing id");

    println!("Uploaded with ID: {}", id);

    // Fetch blob back
    let blob_url = format!("{}/blob/{}", worker_url, id);
    let response = ureq::get(&blob_url).call().expect("fetch blob failed");

    assert_eq!(response.status(), 200);

    let mut fetched_blob = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut fetched_blob)
        .expect("read blob");

    println!("Fetched blob size: {} bytes", fetched_blob.len());
    assert_eq!(
        fetched_blob, encrypted.blob,
        "Fetched blob doesn't match uploaded"
    );

    // Decrypt
    let decrypted = decrypt_blob(&fetched_blob, &encrypted.key_b64).expect("decryption failed");

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
    let worker_url =
        std::env::var("WORKER_URL").unwrap_or_else(|_| "http://localhost:8787".to_string());

    // First upload something
    let test_html = "<html><body>test</body></html>";
    let encrypted = encrypt_html(test_html).unwrap();
    let key_hash = compute_key_hash(&encrypted.key_b64);

    let response = ureq::post(&format!("{}/upload", worker_url))
        .set("Content-Type", "application/octet-stream")
        .set("X-Key-Hash", &key_hash)
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
    assert!(html.contains("Decrypting..."));
    assert!(html.contains("crypto.subtle.decrypt"));

    println!("✓ Viewer page test PASSED!");
}

/// Test 404 for non-existent blob
#[test]
#[ignore]
fn test_blob_not_found() {
    let worker_url =
        std::env::var("WORKER_URL").unwrap_or_else(|_| "http://localhost:8787".to_string());

    // ID format: g (TTL prefix) + 16 hex chars = 17 chars total
    let response = ureq::get(&format!("{}/blob/g0000000000000000", worker_url)).call();

    match response {
        Err(ureq::Error::Status(404, _)) => println!("✓ 404 test PASSED!"),
        other => panic!("Expected 404, got {:?}", other),
    }
}

/// Test delete flow with key hash authentication
#[test]
#[ignore]
fn test_delete_with_key_hash() {
    let worker_url =
        std::env::var("WORKER_URL").unwrap_or_else(|_| "http://localhost:8787".to_string());

    // Upload a blob
    let test_html = "<html><body>delete test</body></html>";
    let encrypted = encrypt_html(test_html).unwrap();
    let key_hash = compute_key_hash(&encrypted.key_b64);

    let response = ureq::post(&format!("{}/upload", worker_url))
        .set("Content-Type", "application/octet-stream")
        .set("X-Key-Hash", &key_hash)
        .send_bytes(&encrypted.blob)
        .unwrap();

    let upload_response: serde_json::Value = response.into_json().unwrap();
    let id = upload_response["id"].as_str().unwrap();
    println!("Uploaded blob with ID: {}", id);

    // Verify blob exists
    let response = ureq::get(&format!("{}/blob/{}", worker_url, id)).call();
    assert!(response.is_ok(), "Blob should exist");

    // Try to delete with wrong key hash - should fail
    let wrong_hash = "0".repeat(64);
    let response = ureq::delete(&format!("{}/blob/{}", worker_url, id))
        .set("X-Key-Hash", &wrong_hash)
        .call();
    match response {
        Err(ureq::Error::Status(401, _)) => println!("Correctly rejected wrong key hash"),
        other => panic!("Expected 401 for wrong key hash, got {:?}", other),
    }

    // Delete with correct key hash - should succeed
    let response = ureq::delete(&format!("{}/blob/{}", worker_url, id))
        .set("X-Key-Hash", &key_hash)
        .call()
        .expect("delete should succeed");
    assert_eq!(response.status(), 204, "Delete should return 204");
    println!("Delete succeeded");

    // Verify blob is gone
    let response = ureq::get(&format!("{}/blob/{}", worker_url, id)).call();
    match response {
        Err(ureq::Error::Status(404, _)) => println!("Blob correctly deleted"),
        other => panic!("Expected 404 after delete, got {:?}", other),
    }

    println!("✓ Delete test PASSED!");
}

/// Test delete without key hash fails
#[test]
#[ignore]
fn test_delete_requires_key_hash() {
    let worker_url =
        std::env::var("WORKER_URL").unwrap_or_else(|_| "http://localhost:8787".to_string());

    // Upload a blob
    let test_html = "<html><body>auth test</body></html>";
    let encrypted = encrypt_html(test_html).unwrap();
    let key_hash = compute_key_hash(&encrypted.key_b64);

    let response = ureq::post(&format!("{}/upload", worker_url))
        .set("Content-Type", "application/octet-stream")
        .set("X-Key-Hash", &key_hash)
        .send_bytes(&encrypted.blob)
        .unwrap();

    let upload_response: serde_json::Value = response.into_json().unwrap();
    let id = upload_response["id"].as_str().unwrap();

    // Try to delete without key hash - should fail
    let response = ureq::delete(&format!("{}/blob/{}", worker_url, id)).call();
    match response {
        Err(ureq::Error::Status(401, _)) => println!("✓ Delete auth test PASSED!"),
        other => panic!("Expected 401, got {:?}", other),
    }
}

// Helper: encrypt HTML (mirrors src/crypto.rs)
struct EncryptionResult {
    blob: Vec<u8>,
    key_b64: String,
}

fn encrypt_html(html: &str) -> Result<EncryptionResult, Box<dyn std::error::Error>> {
    use flate2::{Compression, write::GzEncoder};
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
    let ciphertext = cipher
        .encrypt(nonce, compressed.as_slice())
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
    let compressed = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("decryption failed: {}", e))?;

    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut html = String::new();
    decoder.read_to_string(&mut html)?;

    Ok(html)
}

// Helper: compute SHA256 hash of key for authentication
fn compute_key_hash(key_b64: &str) -> String {
    let key_bytes = URL_SAFE_NO_PAD.decode(key_b64).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(&key_bytes);
    hex::encode(hasher.finalize())
}
