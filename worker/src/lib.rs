use maud::{DOCTYPE, html, PreEscaped};
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
        .get("/setup", |_, _| {
            let mut response = Response::ok(setup_script())?;
            response.headers_mut().set("Content-Type", "text/plain")?;
            Ok(response)
        })
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
        body {
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            max-width: 600px;
            margin: 4rem auto;
            padding: 0 1rem;
            line-height: 1.6;
        }
        header { display: flex; align-items: baseline; gap: 1rem; margin-bottom: 0.25rem; }
        h1 { margin: 0; }
        header a { color: #666; font-size: 0.9rem; }
        .tagline { color: #666; margin-bottom: 2rem; }
        h2 { font-size: 1rem; margin-top: 2rem; color: #333; }
        p { margin: 0.5rem 0; }
        code { background: #f4f4f4; padding: 0.1em 0.3em; border-radius: 3px; }
        a { color: #0066cc; }
        .install-box {
            position: relative;
            display: flex;
            align-items: center;
            background: #f4f4f4;
            border-radius: 4px;
            padding: 0.75rem 1rem;
            margin: 0.5rem 0;
            font-family: monospace;
            cursor: pointer;
            transition: background 0.15s;
        }
        .install-box:hover { background: #e8e8e8; }
        .install-box code {
            flex: 1;
            background: none;
            padding: 0;
        }
        .install-box .copy-icon {
            width: 18px;
            height: 18px;
            opacity: 0.5;
            transition: opacity 0.15s;
        }
        .install-box:hover .copy-icon { opacity: 0.8; }
        .tooltip {
            position: absolute;
            right: 0;
            top: -32px;
            background: #333;
            color: white;
            padding: 4px 10px;
            border-radius: 4px;
            font-size: 12px;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            white-space: nowrap;
            opacity: 0;
            pointer-events: none;
            transition: opacity 0.15s;
        }
        .install-box:hover .tooltip { opacity: 1; }
        .tooltip.copied { background: #22863a; }
    </style>
</head>
<body>
    <header>
        <h1>agentexports</h1>
        <a href="https://github.com/nicosuave/agentexports">GitHub</a>
    </header>
    <p class="tagline">Share Claude Code and Codex transcripts. Encrypted locally, decryption key never leaves your URL.</p>

    <h2>Install</h2>
    <div class="install-box" onclick="copyCmd(this)">
        <span class="tooltip">Click to copy</span>
        <code>curl -fsSL https://agentexports.com/setup | sh</code>
        <svg class="copy-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>
    </div>
    <script>
    function copyCmd(el) {
        const text = el.querySelector('code').textContent;
        const tip = el.querySelector('.tooltip');
        const ta = document.createElement('textarea');
        ta.value = text;
        ta.style.position = 'fixed';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        document.execCommand('copy');
        document.body.removeChild(ta);
        tip.textContent = 'Copied to clipboard';
        tip.classList.add('copied');
        setTimeout(() => {
            tip.textContent = 'Click to copy';
            tip.classList.remove('copied');
        }, 2000);
    }
    </script>

    <h2>Usage</h2>
    <p>Run <code>agentexport setup-skills</code> to install the skill, then type <code>/agentexport</code> in Claude Code or Codex.</p>

    <h2>How it works</h2>
    <p>Transcripts are encrypted client-side before upload. The server only stores encrypted blobs. The decryption key lives in the URL fragment and is never sent to the server. Shares auto-expire after 30 days.</p>
</body>
</html>
"##.to_string()
}

fn setup_script() -> String {
    r##"#!/bin/sh
set -e

REPO="nicosuave/agentexports"
BINARY="agentexport"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"

# Detect OS
OS="$(uname -s)"
case "$OS" in
    Darwin) OS="macos" ;;
    Linux) OS="linux" ;;
    *) echo "Unsupported OS: $OS"; exit 1 ;;
esac

# Detect architecture
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    arm64|aarch64) ARCH="arm64" ;;
    *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

# Get latest version
VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | sed -E 's/.*"v([^"]+)".*/\1/')
if [ -z "$VERSION" ]; then
    echo "Failed to get latest version"
    exit 1
fi

echo "Installing $BINARY v$VERSION for $OS-$ARCH..."

# Download and extract
URL="https://github.com/$REPO/releases/download/v$VERSION/$BINARY-$VERSION-$OS-$ARCH.tar.gz"
TMP_DIR=$(mktemp -d)
trap "rm -rf $TMP_DIR" EXIT

curl -fsSL "$URL" | tar -xz -C "$TMP_DIR"

mkdir -p "$INSTALL_DIR"
mv "$TMP_DIR/$BINARY" "$INSTALL_DIR/"
chmod +x "$INSTALL_DIR/$BINARY"

echo "Installed $BINARY to $INSTALL_DIR/$BINARY"

# Check if ~/.local/bin is in PATH
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *)
        echo ""
        echo "Note: $INSTALL_DIR is not in your PATH."
        echo "Add this to your shell config (~/.bashrc, ~/.zshrc, etc.):"
        echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
        ;;
esac

echo ""
echo "Run 'agentexport setup-skills' to configure Claude Code or Codex"
"##.to_string()
}

fn viewer_html(blob_id: &str) -> String {
    let markup = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="UTF-8";
                meta name="viewport" content="width=device-width, initial-scale=1.0";
                title { "Shared Transcript" }
                style { (PreEscaped(VIEWER_CSS)) }
            }
            body {
                div #loading class="loading" {
                    div class="spinner" {}
                    p { "Decrypting..." }
                }
                div #error class="error" style="display:none" {
                    h2 { "Decryption Failed" }
                    p #error-message {}
                }
                div #app style="display:none" {
                    header {
                        div class="title-row" {
                            div class="title-left" {
                                h1 #tool-name { "Transcript" }
                                span #model-info class="model" {}
                            }
                            span #shared-at class="date" {}
                        }
                        div class="meta-row" {
                            span #session-id class="session" {}
                            div class="toggle" {
                                label {
                                    input #show-details type="checkbox";
                                    " Show tool calls"
                                }
                            }
                        }
                    }
                    section #messages class="messages hide-details" {}
                    footer {
                        "via "
                        a href="https://agentexports.com" { "agentexports.com" }
                    }
                }
                script { (PreEscaped(viewer_js(blob_id))) }
            }
        }
    };
    markup.into_string()
}

const VIEWER_CSS: &str = r#"
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    background: #fff;
    color: #111;
    line-height: 1.6;
    max-width: 720px;
    margin: 0 auto;
    padding: 48px 24px;
}
.loading, .error {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    min-height: 60vh;
    text-align: center;
}
.spinner {
    width: 32px; height: 32px;
    border: 3px solid #eee;
    border-top-color: #333;
    border-radius: 50%;
    animation: spin 1s linear infinite;
    margin-bottom: 1rem;
}
@keyframes spin { to { transform: rotate(360deg); } }
.error { color: #c00; }
.error h2 { margin-bottom: 0.5rem; }
header { margin-bottom: 32px; }
.title-row { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 8px; }
.title-left { display: flex; align-items: baseline; gap: 12px; }
h1 { font-size: 18px; font-weight: 600; }
.model { font-size: 13px; color: #666; font-family: ui-monospace, monospace; }
.date { font-size: 13px; color: #666; }
.meta-row { display: flex; justify-content: space-between; align-items: center; }
.session { font-family: ui-monospace, monospace; font-size: 12px; color: #999; }
.toggle { font-size: 13px; color: #666; }
.toggle label { cursor: pointer; display: flex; align-items: center; gap: 4px; }
.messages { margin-top: 24px; }
.msg { padding: 16px 0; }
.msg-header { display: flex; justify-content: space-between; align-items: baseline; margin-bottom: 6px; }
.msg-role { font-size: 12px; font-weight: 600; text-transform: uppercase; color: #666; }
.msg-role.user { color: #0066cc; }
.msg-role.assistant { color: #1a1a1a; }
.msg-model { font-size: 11px; color: #999; font-family: ui-monospace, monospace; }
.msg-content { font-size: 15px; }
.msg-content p { margin: 0.5em 0; }
.msg-content p:first-child { margin-top: 0; }
.msg-content code { background: #f5f5f5; padding: 0.1em 0.3em; border-radius: 3px; font-size: 0.9em; }
.msg-content pre { background: #f5f5f5; padding: 12px; border-radius: 6px; overflow-x: auto; margin: 0.5em 0; }
.msg-content pre code { background: none; padding: 0; }
.msg-content ul, .msg-content ol { margin: 0.5em 0 0.5em 1.5em; }
.msg-content h1, .msg-content h2, .msg-content h3 { margin: 1em 0 0.5em; font-size: 1.1em; }
.msg-content table { border-collapse: collapse; margin: 0.5em 0; width: 100%; }
.msg-content th, .msg-content td { border: 1px solid #ddd; padding: 8px 12px; text-align: left; }
.msg-content th { background: #f5f5f5; font-weight: 600; }
.msg.tool, .msg.system { opacity: 0.7; }
.msg.tool .msg-content { font-family: ui-monospace, monospace; font-size: 13px; white-space: pre-wrap; }
.msg.system .msg-content { font-size: 13px; color: #666; border-left: 3px solid #ddd; padding-left: 12px; }
.hide-details .msg.tool, .hide-details .msg.system { display: none; }
.raw { margin-top: 8px; }
.raw summary { font-size: 12px; color: #666; cursor: pointer; }
.raw pre { background: #f5f5f5; padding: 12px; border-radius: 6px; overflow-x: auto; font-size: 12px; margin-top: 8px; max-height: 300px; }
footer { margin-top: 48px; font-size: 12px; color: #999; text-align: center; }
footer a { color: #666; text-decoration: none; }
footer a:hover { text-decoration: underline; }
"#;

fn viewer_js(blob_id: &str) -> String {
    format!(r#"
const BLOB_ID = "{blob_id}";

// Minimal markdown parser with table support
function md(text) {{
    if (!text) return '';

    // Extract code blocks first (before any processing)
    const codeBlocks = [];
    text = text.replace(/```(\w*)\n([\s\S]*?)```/g, (m, lang, code) => {{
        const placeholder = '%%CODE' + codeBlocks.length + '%%';
        codeBlocks.push('<pre><code>' + escapeHtml(code) + '</code></pre>');
        return placeholder;
    }});

    // Extract inline code
    const inlineCodes = [];
    text = text.replace(/`([^`]+)`/g, (m, code) => {{
        const placeholder = '%%INLINE' + inlineCodes.length + '%%';
        inlineCodes.push('<code>' + escapeHtml(code) + '</code>');
        return placeholder;
    }});

    // Extract tables
    const tableRegex = /^\|(.+)\|\n\|[-:\| ]+\|\n((?:\|.+\|\n?)+)/gm;
    const tables = [];
    text = text.replace(tableRegex, (match, headerRow, bodyRows) => {{
        const headers = headerRow.split('|').map(h => h.trim()).filter(h => h);
        const rows = bodyRows.trim().split('\n').map(row =>
            row.split('|').map(c => c.trim()).filter(c => c)
        );
        let table = '<table><thead><tr>';
        headers.forEach(h => {{ table += '<th>' + escapeHtml(h) + '</th>'; }});
        table += '</tr></thead><tbody>';
        rows.forEach(row => {{
            table += '<tr>';
            row.forEach(c => {{ table += '<td>' + escapeHtml(c) + '</td>'; }});
            table += '</tr>';
        }});
        table += '</tbody></table>';
        const placeholder = '%%TABLE' + tables.length + '%%';
        tables.push(table);
        return placeholder;
    }});

    // Now escape HTML
    text = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

    // Process markdown
    text = text
        // Bold (must come before italic)
        .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
        .replace(/__(.+?)__/g, '<strong>$1</strong>')
        // Italic
        .replace(/\*(.+?)\*/g, '<em>$1</em>')
        .replace(/_(.+?)_/g, '<em>$1</em>')
        // Headers
        .replace(/^### (.+)$/gm, '<h3>$1</h3>')
        .replace(/^## (.+)$/gm, '<h2>$1</h2>')
        .replace(/^# (.+)$/gm, '<h1>$1</h1>')
        // Lists
        .replace(/^- (.+)$/gm, '<li>$1</li>')
        .replace(/(<li>.*<\/li>\n?)+/g, '<ul>$&</ul>')
        // Links
        .replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2">$1</a>');

    // Paragraphs
    text = text.split(/\n\n+/).map(p => {{
        if (p.startsWith('<h') || p.startsWith('<pre') || p.startsWith('<ul') || p.startsWith('%%')) return p;
        return '<p>' + p.replace(/\n/g, '<br>') + '</p>';
    }}).join('');

    // Restore placeholders
    tables.forEach((t, i) => {{ text = text.replace('%%TABLE' + i + '%%', t); }});
    inlineCodes.forEach((c, i) => {{ text = text.replace('%%INLINE' + i + '%%', c); }});
    codeBlocks.forEach((c, i) => {{ text = text.replace('%%CODE' + i + '%%', c); }});

    return text;
}}

function escapeHtml(str) {{
    return str.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}}

function render(data) {{
    document.getElementById('tool-name').textContent = data.tool || 'Transcript';
    document.getElementById('shared-at').textContent = data.shared_at || '';
    document.getElementById('session-id').textContent = data.session_id || '';

    // Model display
    const models = data.models || [];
    const modelEl = document.getElementById('model-info');
    if (models.length === 1) {{
        modelEl.textContent = models[0];
    }} else if (models.length > 1) {{
        modelEl.textContent = models.join(' + ');
    }}

    const showMultipleModels = models.length > 1;
    const container = document.getElementById('messages');
    container.innerHTML = '';

    for (const msg of data.messages || []) {{
        const div = document.createElement('div');
        div.className = 'msg ' + (msg.role || 'event');

        const header = document.createElement('div');
        header.className = 'msg-header';

        const role = document.createElement('span');
        role.className = 'msg-role ' + (msg.role || '');
        role.textContent = msg.role || 'event';
        header.appendChild(role);

        if (showMultipleModels && msg.model) {{
            const model = document.createElement('span');
            model.className = 'msg-model';
            model.textContent = msg.model;
            header.appendChild(model);
        }}

        div.appendChild(header);

        const content = document.createElement('div');
        content.className = 'msg-content';
        if (msg.role === 'tool') {{
            content.textContent = msg.content || '';
        }} else {{
            content.innerHTML = md(msg.content || '');
        }}
        div.appendChild(content);

        if (msg.raw) {{
            const details = document.createElement('details');
            details.className = 'raw';
            const summary = document.createElement('summary');
            summary.textContent = msg.raw_label || 'Raw';
            details.appendChild(summary);
            const pre = document.createElement('pre');
            pre.textContent = msg.raw;
            details.appendChild(pre);
            div.appendChild(details);
        }}

        container.appendChild(div);
    }}

    document.getElementById('show-details').addEventListener('change', function() {{
        document.getElementById('messages').classList.toggle('hide-details', !this.checked);
    }});
}}

async function main() {{
    try {{
        const fragment = window.location.hash.slice(1);
        if (!fragment) throw new Error("No decryption key in URL");

        const keyBytes = base64UrlDecode(fragment);
        if (keyBytes.length !== 32) throw new Error("Invalid key length");

        const response = await fetch('/blob/' + BLOB_ID);
        if (response.status === 410) throw new Error("This transcript has expired");
        if (!response.ok) throw new Error('Failed to fetch: ' + response.status);

        const encrypted = await response.arrayBuffer();
        if (encrypted.byteLength < 13) throw new Error("Invalid blob");

        const iv = encrypted.slice(0, 12);
        const ciphertext = encrypted.slice(12);

        const key = await crypto.subtle.importKey("raw", keyBytes, {{ name: "AES-GCM" }}, false, ["decrypt"]);
        const compressed = await crypto.subtle.decrypt({{ name: "AES-GCM", iv }}, key, ciphertext);
        const json = await decompress(new Uint8Array(compressed));
        const data = JSON.parse(json);

        document.getElementById('loading').style.display = 'none';
        document.getElementById('app').style.display = 'block';
        render(data);
    }} catch (err) {{
        document.getElementById('loading').style.display = 'none';
        document.getElementById('error').style.display = 'flex';
        document.getElementById('error-message').textContent = err.message;
    }}
}}

function base64UrlDecode(str) {{
    const pad = str.length % 4;
    if (pad) str += '='.repeat(4 - pad);
    str = str.replace(/-/g, '+').replace(/_/g, '/');
    const bin = atob(str);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
}}

async function decompress(data) {{
    const ds = new DecompressionStream('gzip');
    const writer = ds.writable.getWriter();
    writer.write(data);
    writer.close();
    const chunks = [];
    const reader = ds.readable.getReader();
    while (true) {{
        const {{ done, value }} = await reader.read();
        if (done) break;
        chunks.push(value);
    }}
    const result = new Uint8Array(chunks.reduce((a, c) => a + c.length, 0));
    let offset = 0;
    for (const chunk of chunks) {{ result.set(chunk, offset); offset += chunk.length; }}
    return new TextDecoder().decode(result);
}}

main();
"#)
}
