use sha2::{Digest, Sha256};
use worker::*;

const MAX_BLOB_SIZE: usize = 10 * 1024 * 1024; // 10MB
const DEFAULT_TTL_DAYS: u64 = 30;

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();

    router
        .get("/", |_, _| Response::from_html(homepage_html()))
        .post_async("/upload", handle_upload)
        .get_async("/v/:id", handle_viewer)
        .get_async("/blob/:id", handle_blob)
        .delete_async("/blob/:id", handle_delete)
        .options_async("/upload", handle_cors_preflight)
        .options_async("/blob/:id", handle_cors_preflight)
        .run(req, env)
        .await
}

fn cors_headers() -> Headers {
    let headers = Headers::new();
    let _ = headers.set("Access-Control-Allow-Origin", "*");
    let _ = headers.set("Access-Control-Allow-Methods", "GET, POST, DELETE, OPTIONS");
    let _ = headers.set("Access-Control-Allow-Headers", "Content-Type, X-Key-Hash");
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

    // Get key hash from header (required for delete auth)
    let key_hash = req
        .headers()
        .get("X-Key-Hash")?
        .unwrap_or_default();
    if key_hash.is_empty() || key_hash.len() != 64 {
        return with_cors(Response::error("Missing or invalid X-Key-Hash header", 400)?);
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

    // Calculate expiration
    let ttl_days = ctx
        .env
        .var("TTL_DAYS")
        .map(|v| v.to_string().parse().unwrap_or(DEFAULT_TTL_DAYS))
        .unwrap_or(DEFAULT_TTL_DAYS);
    let uploaded_at = current_timestamp();
    let expires_at = uploaded_at + (ttl_days * 24 * 60 * 60);

    // Store with metadata
    let mut metadata = std::collections::HashMap::new();
    metadata.insert("uploaded_at".to_string(), uploaded_at.to_string());
    metadata.insert("key_hash".to_string(), key_hash);
    bucket
        .put(&id, body)
        .custom_metadata(metadata)
        .execute()
        .await?;

    let response_body = serde_json::json!({
        "id": id,
        "expires_at": expires_at
    });
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

async fn handle_delete(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let id = ctx.param("id").unwrap();

    if !is_valid_id(id) {
        return with_cors(Response::error("Invalid ID", 400)?);
    }

    // Get key hash from header
    let key_hash = req
        .headers()
        .get("X-Key-Hash")?
        .unwrap_or_default();
    if key_hash.is_empty() {
        return with_cors(Response::error("Missing X-Key-Hash header", 401)?);
    }

    let bucket = ctx.env.bucket("TRANSCRIPTS")?;

    // Check blob exists and verify key hash
    match bucket.head(id).await? {
        Some(object) => {
            let stored_hash = object
                .custom_metadata()
                .ok()
                .and_then(|m| m.get("key_hash").cloned())
                .unwrap_or_default();

            if stored_hash.is_empty() {
                // Legacy blob without key_hash - can't be deleted via API
                return with_cors(Response::error("Blob predates delete support", 403)?);
            }

            if stored_hash != key_hash {
                return with_cors(Response::error("Invalid key hash", 401)?);
            }

            // Delete the blob
            bucket.delete(id).await?;
            with_cors(Response::empty()?.with_status(204))
        }
        None => with_cors(Response::error("Not found", 404)?),
    }
}

async fn handle_cors_preflight(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    let mut response = Response::empty()?;
    *response.headers_mut() = cors_headers();
    response
        .headers_mut()
        .set("Access-Control-Max-Age", "86400")?;
    Ok(response)
}

fn homepage_html() -> String {
    r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>agentexports</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: #0a0a0a;
            color: #e0e0e0;
            min-height: 100vh;
            display: flex;
            align-items: center;
            justify-content: center;
        }
        .container {
            max-width: 600px;
            padding: 2rem;
            text-align: center;
        }
        h1 {
            font-size: 2.5rem;
            margin-bottom: 1rem;
            background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            background-clip: text;
        }
        .tagline {
            font-size: 1.25rem;
            color: #888;
            margin-bottom: 2rem;
        }
        .features {
            text-align: left;
            margin: 2rem 0;
            padding: 1.5rem;
            background: #111;
            border-radius: 8px;
            border: 1px solid #222;
        }
        .features h2 {
            font-size: 0.875rem;
            text-transform: uppercase;
            letter-spacing: 0.1em;
            color: #666;
            margin-bottom: 1rem;
        }
        .features ul {
            list-style: none;
        }
        .features li {
            padding: 0.5rem 0;
            color: #aaa;
        }
        .features li::before {
            content: "→ ";
            color: #667eea;
        }
        .links {
            margin-top: 2rem;
        }
        .links a {
            color: #667eea;
            text-decoration: none;
            margin: 0 1rem;
        }
        .links a:hover {
            text-decoration: underline;
        }
        code {
            background: #1a1a1a;
            padding: 0.2em 0.4em;
            border-radius: 4px;
            font-size: 0.9em;
        }
    </style>
</head>
<body>
    <div class="container">
        <h1>agentexports</h1>
        <p class="tagline">Zero-knowledge transcript sharing for Claude Code and Codex</p>

        <div class="features">
            <h2>How it works</h2>
            <ul>
                <li>Transcripts are encrypted locally before upload</li>
                <li>Server only sees encrypted blobs</li>
                <li>Decryption key stays in URL fragment (never sent to server)</li>
                <li>Auto-expires after 30 days</li>
            </ul>
        </div>

        <div class="features">
            <h2>Usage</h2>
            <ul>
                <li>Install: <code>cargo install agentexport</code></li>
                <li>Setup: <code>agentexport setup-skills</code></li>
                <li>Share: type <code>/agentexport</code> in Claude Code</li>
            </ul>
        </div>

        <div class="links">
            <a href="https://github.com/nicosuave/agentexports">GitHub</a>
        </div>
    </div>
</body>
</html>
"##.to_string()
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
