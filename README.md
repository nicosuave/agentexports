# agentexport

Export and share Claude Code and Codex session transcripts with zero-knowledge encryption.

## Features

- **Zero-knowledge sharing**: Transcripts are encrypted client-side before upload. The server only sees encrypted blobs and cannot read your content.
- **Privacy by design**: Decryption keys live only in URL fragments, which browsers never send to servers.
- **Compression**: Gzip compression before encryption reduces upload size.
- **Expiration**: Shared transcripts auto-expire after 30 days.
- **Terminal-aware**: Automatically tracks sessions per terminal (supports tmux, iTerm2).

## Installation

```bash
cargo install --path .
```

## Quick Start

### Share a transcript

```bash
# Share current Claude session
agentexport publish --tool claude --upload-url https://agentexports.com
# => https://agentexports.com/v/abc123#decryptionKey...

# Share a Codex session
agentexport publish --tool codex --upload-url https://agentexports.com
```

The output URL contains everything needed to view the transcript. Anyone with the URL can decrypt and view it.

### Local export (no upload)

```bash
# Export to local HTML file
agentexport publish --tool claude --render

# Export with custom output path
agentexport publish --tool claude --out ./my-export.jsonl.gz
```

## Architecture

```
┌─────────────────┐      ┌─────────────────┐      ┌─────────────────┐
│   CLI           │      │   CF Worker     │      │   Browser       │
│                 │      │                 │      │                 │
│ 1. Render HTML  │      │                 │      │                 │
│ 2. Gzip         │ POST │ Store in R2     │ GET  │ Fetch blob      │
│ 3. Encrypt      │─────>│ (encrypted)     │─────>│ Decrypt (AES)   │
│ 4. Upload       │      │                 │      │ Decompress      │
│                 │      │                 │      │ Display         │
└─────────────────┘      └─────────────────┘      └─────────────────┘
                                                          │
                              Key in URL fragment ────────┘
                              (never sent to server)
```

### Encryption

- **Algorithm**: AES-256-GCM
- **Key**: 256-bit random, base64url encoded in URL fragment
- **IV**: 96-bit random, prepended to ciphertext
- **Compression**: Gzip before encryption

### URL Format

```
https://agentexports.com/v/{id}#{key}
                            │    │
                            │    └── Base64url decryption key (never sent to server)
                            └─────── Content-addressed blob ID (SHA-256 prefix)
```

## CLI Reference

### `agentexport publish`

Export and optionally upload a transcript.

```
Options:
  --tool <claude|codex>     Tool to export from (required)
  --term-key <KEY>          Terminal key (auto-detected if not provided)
  --transcript <PATH>       Path to transcript file (auto-resolved if not provided)
  --max-age-minutes <N>     Max age of transcript to accept [default: 10]
  --out <PATH>              Output path for gzip file
  --render                  Also render HTML locally
  --upload-url <URL>        Upload to sharing service
  --dry-run                 Skip actual upload
```

### `agentexport term-key`

Print the current terminal's unique key.

```bash
agentexport term-key
# => a1b2c3d4e5f6...
```

### `agentexport setup-skills`

Interactive setup for Claude/Codex skill integration.

```bash
agentexport setup-skills
```

## Self-Hosting

### Deploy the Worker

```bash
cd worker

# Create R2 bucket
wrangler r2 bucket create agent-exports

# Deploy to Cloudflare
wrangler deploy --env production
```

### Configure Custom Domain

1. In Cloudflare dashboard, go to Workers & Pages
2. Select `agentexport-share`
3. Settings > Triggers > Add Custom Domain
4. Enter your domain (e.g., `share.yourdomain.com`)

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `TTL_DAYS` | `30` | Days until transcripts expire |

## Development

### Prerequisites

- Rust (with `wasm32-unknown-unknown` target)
- Node.js (for wrangler)
- wrangler CLI

```bash
rustup target add wasm32-unknown-unknown
npm install -g wrangler
cargo install worker-build
```

### Run locally

```bash
# Terminal 1: Start worker
cd worker && wrangler dev --port 8787

# Terminal 2: Test upload
agentexport publish --tool claude --upload-url http://localhost:8787
```

### Run tests

```bash
# Unit tests
cargo test --lib

# E2E tests (requires worker running on :8787)
cargo test --test e2e -- --ignored
```

## How It Works

### Session Tracking

agentexport tracks terminal sessions using a hash of:
- TTY device path
- tmux pane ID (if in tmux)
- iTerm2 session ID (if in iTerm2)

This allows multiple concurrent sessions without confusion.

### Transcript Resolution

**Claude**: Uses environment variables set by Claude Code hooks, or reads from cached state.

**Codex**: Scans `~/.codex/sessions/` and matches against `~/.codex/history.jsonl` to find the most recent interactive session for the current directory.

### Security Model

1. **Client encrypts**: HTML is gzip-compressed, then encrypted with a random AES-256-GCM key
2. **Server stores blobs**: Worker receives opaque encrypted bytes, stores in R2
3. **Key in fragment**: The decryption key is placed in the URL fragment (`#key`)
4. **Fragments are private**: Browsers never send URL fragments to servers
5. **Client decrypts**: Viewer page fetches blob, decrypts using Web Crypto API

The server operator (you or agentexports.com) cannot read transcript contents because they never receive the decryption key.

## License

MIT
