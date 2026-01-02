# agentexport

Share Claude Code and Codex session transcripts with zero-knowledge encryption.

## Features

- **Zero-knowledge sharing**: Transcripts are encrypted client-side. The server only sees encrypted blobs.
- **Privacy by design**: Decryption keys live only in URL fragments, never sent to servers.
- **Works with Claude Code and Codex**: Just run `/agentexport` in Claude or the publish command in Codex.

## Installation

```bash
curl -fsSL https://agentexports.com/setup | sh
```

Then run setup to install skills and hooks:

```bash
agentexport setup-skills
```

This will:
- **Claude Code**: Install the `/agentexport` skill and a SessionStart hook
- **Codex**: Install the publish prompt

Restart Claude/Codex after setup.

## Usage

### Claude Code

Just type `/agentexport` in any session:

```
/agentexport
```

Claude will publish your current session and return a shareable URL like:
```
https://agentexports.com/v/ga1b2c3d4e5f6g7h8#SGVsbG8gV29ybGQh...
```

### Codex

Use the publish command to share your current session.

## How It Works

```
┌─────────────────┐      ┌─────────────────┐      ┌─────────────────┐
│   Your Terminal │      │   Server (R2)   │      │   Recipient     │
│                 │      │                 │      │                 │
│ 1. Gzip         │      │                 │      │                 │
│ 2. Encrypt      │ POST │ Store encrypted │ GET  │ Fetch blob      │
│ 3. Upload       │─────>│ blob (opaque)   │─────>│ Decrypt in JS   │
│ 4. Get URL      │      │                 │      │ Decompress      │
│                 │      │                 │      │ View transcript │
└─────────────────┘      └─────────────────┘      └─────────────────┘
                                                          │
                              Key in URL fragment ────────┘
                              (never sent to server)
```

The server operator cannot read your transcripts because:
1. Content is encrypted with AES-256-GCM before upload
2. The decryption key is placed in the URL fragment (`#key`)
3. Browsers never send URL fragments to servers
4. Decryption happens entirely in the recipient's browser

### URL Format

```
https://agentexports.com/v/{id}#{key}
                            │    │
                            │    └── Base64url AES-256 key (never sent to server)
                            └─────── TTL prefix + content hash (e.g., g = 30 days)
```

### Managing Shares

List your shares and their expiration:

```bash
agentexport shares
```

Delete a share:

```bash
agentexport shares unshare <id>
```

Shares are stored locally in `~/.cache/agentexport/shares.json` with the decryption keys needed for deletion.

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
4. Enter your domain

Then configure the CLI to use your domain:

```bash
agentexport config set upload_url https://your-domain.com
```

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

# Terminal 2: Test
agentexport publish --tool claude --upload-url http://localhost:8787
```

### Run tests

```bash
# Unit tests
cargo test --lib

# E2E tests (requires worker running on :8787)
cargo test --test e2e -- --ignored
```

## Encryption Details

| Component | Value |
|-----------|-------|
| Algorithm | AES-256-GCM |
| Key | 256 bits, random |
| IV/Nonce | 96 bits, random |
| Compression | Gzip before encryption |
| Expiration | 30 days (configurable: 30, 60, 90, 180, 365, or forever) |

## License

MIT
