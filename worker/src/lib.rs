use sha2::{Digest, Sha256};
use worker::*;

const MAX_BLOB_SIZE: usize = 10 * 1024 * 1024; // 10MB
const DEFAULT_TTL_DAYS: u64 = 30;

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();

    router
        .post_async("/upload", handle_upload)
        .get_async("/v/:id", handle_viewer)
        .get_async("/blob/:id", handle_blob)
        .options_async("/upload", handle_cors_preflight)
        .run(req, env)
        .await
}

fn cors_headers() -> Headers {
    let headers = Headers::new();
    let _ = headers.set("Access-Control-Allow-Origin", "*");
    let _ = headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    let _ = headers.set("Access-Control-Allow-Headers", "Content-Type");
    headers
}

fn with_cors(mut response: Response) -> Result<Response> {
    let cors = cors_headers();
    for (key, value) in cors.entries() {
        response.headers_mut().set(&key, &value)?;
    }
    Ok(response)
}

fn generate_id(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = hasher.finalize();
    hex::encode(&hash[..8])
}

fn is_valid_id(id: &str) -> bool {
    id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit())
}

fn current_timestamp() -> u64 {
    js_sys::Date::now() as u64 / 1000
}

fn is_expired(uploaded_at: u64, ttl_days: u64) -> bool {
    let now = current_timestamp();
    let ttl_seconds = ttl_days * 24 * 60 * 60;
    now > uploaded_at + ttl_seconds
}

async fn handle_upload(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    // Size check
    if let Some(len) = req.headers().get("content-length")? {
        if let Ok(size) = len.parse::<usize>() {
            if size > MAX_BLOB_SIZE {
                return with_cors(Response::error("Blob too large", 413)?);
            }
        }
    }

    let body = req.bytes().await?;
    if body.len() > MAX_BLOB_SIZE {
        return with_cors(Response::error("Blob too large", 413)?);
    }
    if body.is_empty() {
        return with_cors(Response::error("Empty body", 400)?);
    }

    let id = generate_id(&body);
    let bucket = ctx.env.bucket("TRANSCRIPTS")?;

    // Store with timestamp metadata
    let timestamp = current_timestamp().to_string();
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("uploaded_at".to_string(), timestamp);
    bucket
        .put(&id, body)
        .custom_metadata(metadata)
        .execute()
        .await?;

    let response_body = serde_json::json!({ "id": id });
    with_cors(Response::from_json(&response_body)?)
}

async fn handle_blob(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let id = ctx.param("id").unwrap();

    if !is_valid_id(id) {
        return with_cors(Response::error("Invalid ID", 400)?);
    }

    let bucket = ctx.env.bucket("TRANSCRIPTS")?;

    match bucket.get(id).execute().await? {
        Some(object) => {
            // Check expiration
            let ttl_days = ctx
                .env
                .var("TTL_DAYS")
                .map(|v| v.to_string().parse().unwrap_or(DEFAULT_TTL_DAYS))
                .unwrap_or(DEFAULT_TTL_DAYS);

            if let Some(uploaded_at) = object
                .custom_metadata()
                .ok()
                .and_then(|m| m.get("uploaded_at").cloned())
                .and_then(|s| s.parse::<u64>().ok())
            {
                if is_expired(uploaded_at, ttl_days) {
                    // Optionally delete expired blob
                    let _ = bucket.delete(id).await;
                    return with_cors(Response::error("Expired", 410)?);
                }
            }

            let body = object.body().ok_or_else(|| Error::from("No body"))?;
            let bytes = body.bytes().await?;

            let headers = Headers::new();
            headers.set("Content-Type", "application/octet-stream")?;
            headers.set("Cache-Control", "public, max-age=86400")?;

            let mut response = Response::from_bytes(bytes)?;
            *response.headers_mut() = headers;
            with_cors(response)
        }
        None => with_cors(Response::error("Not found", 404)?),
    }
}

async fn handle_viewer(_req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let id = ctx.param("id").unwrap();

    if !is_valid_id(id) {
        return Response::error("Invalid ID", 400);
    }

    // Check blob exists and not expired
    let bucket = ctx.env.bucket("TRANSCRIPTS")?;
    match bucket.head(id).await? {
        Some(object) => {
            let ttl_days = ctx
                .env
                .var("TTL_DAYS")
                .map(|v| v.to_string().parse().unwrap_or(DEFAULT_TTL_DAYS))
                .unwrap_or(DEFAULT_TTL_DAYS);

            if let Some(uploaded_at) = object
                .custom_metadata()
                .ok()
                .and_then(|m| m.get("uploaded_at").cloned())
                .and_then(|s| s.parse::<u64>().ok())
            {
                if is_expired(uploaded_at, ttl_days) {
                    return Response::error("Expired", 410);
                }
            }
        }
        None => return Response::error("Not found", 404),
    }

    let html = viewer_html(id);
    let mut response = Response::from_html(html)?;

    response.headers_mut().set(
        "Content-Security-Policy",
        "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; frame-src 'self' blob:",
    )?;
    response
        .headers_mut()
        .set("X-Content-Type-Options", "nosniff")?;

    Ok(response)
}

async fn handle_cors_preflight(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    let mut response = Response::empty()?;
    *response.headers_mut() = cors_headers();
    response
        .headers_mut()
        .set("Access-Control-Max-Age", "86400")?;
    Ok(response)
}

fn viewer_html(blob_id: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Shared Transcript</title>
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: #1a1a1a;
            color: #e0e0e0;
            min-height: 100vh;
        }}
        .loading, .error {{
            display: flex;
            flex-direction: column;
            align-items: center;
            justify-content: center;
            min-height: 100vh;
            padding: 2rem;
            text-align: center;
        }}
        .spinner {{
            width: 40px;
            height: 40px;
            border: 3px solid #333;
            border-top-color: #0084ff;
            border-radius: 50%;
            animation: spin 1s linear infinite;
            margin-bottom: 1rem;
        }}
        @keyframes spin {{ to {{ transform: rotate(360deg); }} }}
        .error {{ color: #ff6b6b; }}
        .error h2 {{ margin-bottom: 0.5rem; }}
        #content {{ display: none; }}
        iframe {{
            width: 100%;
            height: 100vh;
            border: none;
            background: #fff;
        }}
    </style>
</head>
<body>
    <div id="loading" class="loading">
        <div class="spinner"></div>
        <p>Decrypting transcript...</p>
    </div>

    <div id="error" class="error" style="display: none;">
        <h2>Decryption Failed</h2>
        <p id="error-message"></p>
    </div>

    <div id="content">
        <iframe id="viewer" sandbox="allow-scripts allow-same-origin"></iframe>
    </div>

    <script>
        const BLOB_ID = "{blob_id}";

        async function main() {{
            try {{
                const fragment = window.location.hash.slice(1);
                if (!fragment) {{
                    throw new Error("No decryption key in URL");
                }}

                const keyBytes = base64UrlDecode(fragment);
                if (keyBytes.length !== 32) {{
                    throw new Error("Invalid key length");
                }}

                const response = await fetch(`/blob/${{BLOB_ID}}`);
                if (response.status === 410) {{
                    throw new Error("This transcript has expired");
                }}
                if (!response.ok) {{
                    throw new Error(`Failed to fetch: ${{response.status}}`);
                }}
                const encryptedData = await response.arrayBuffer();

                if (encryptedData.byteLength < 13) {{
                    throw new Error("Invalid blob format");
                }}
                const iv = encryptedData.slice(0, 12);
                const ciphertext = encryptedData.slice(12);

                const key = await crypto.subtle.importKey(
                    "raw",
                    keyBytes,
                    {{ name: "AES-GCM" }},
                    false,
                    ["decrypt"]
                );

                const compressed = await crypto.subtle.decrypt(
                    {{ name: "AES-GCM", iv: iv }},
                    key,
                    ciphertext
                );

                // Decompress gzip
                const html = await decompress(new Uint8Array(compressed));

                document.getElementById("loading").style.display = "none";
                document.getElementById("content").style.display = "block";

                const iframe = document.getElementById("viewer");
                iframe.srcdoc = html;

            }} catch (err) {{
                document.getElementById("loading").style.display = "none";
                document.getElementById("error").style.display = "flex";
                document.getElementById("error-message").textContent = err.message;
                console.error("Decryption error:", err);
            }}
        }}

        function base64UrlDecode(str) {{
            const pad = str.length % 4;
            if (pad) {{
                str += "=".repeat(4 - pad);
            }}
            str = str.replace(/-/g, "+").replace(/_/g, "/");
            const binary = atob(str);
            const bytes = new Uint8Array(binary.length);
            for (let i = 0; i < binary.length; i++) {{
                bytes[i] = binary.charCodeAt(i);
            }}
            return bytes;
        }}

        async function decompress(data) {{
            // Use DecompressionStream if available (modern browsers)
            if (typeof DecompressionStream !== 'undefined') {{
                const ds = new DecompressionStream('gzip');
                const writer = ds.writable.getWriter();
                writer.write(data);
                writer.close();
                const reader = ds.readable.getReader();
                const chunks = [];
                while (true) {{
                    const {{ done, value }} = await reader.read();
                    if (done) break;
                    chunks.push(value);
                }}
                const totalLength = chunks.reduce((acc, chunk) => acc + chunk.length, 0);
                const result = new Uint8Array(totalLength);
                let offset = 0;
                for (const chunk of chunks) {{
                    result.set(chunk, offset);
                    offset += chunk.length;
                }}
                return new TextDecoder().decode(result);
            }} else {{
                throw new Error("Browser does not support DecompressionStream");
            }}
        }}

        main();
    </script>
</body>
</html>
"##
    )
}
