# agentexport

Share Claude Code and Codex session transcripts with end-to-end encryption.

[Check out a demo](https://agentexports.com/v/nbc6b43907ec5c0f3#EzyQxZQA3hJnwoO7rzJYym0kjIArv4DuPh2asptdEPM)

## Features

- **Private by default**: Your transcripts are encrypted before they leave your machine. The server never sees your content.
- **Safe links**: The decryption key is part of the URL itself, so only people you share with can read it.
- **Works with Claude Code and Codex**: Just run `/agentexport` in Claude or the publish command in Codex.

## Installation

```bash
brew install nicosuave/tap/agentexport
```

Or

```bash
curl -fsSL https://agentexports.com/setup | sh
```

Then run setup to install commands:

```bash
agentexport setup
```

This will:
- **Claude Code**: Install the `/agentexport` command
- **Codex**: Install the `/agentexport` prompt

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

## PR Mapping (experimental)

Links transcript edits to git diff hunks. A Chrome extension (coming soon) will use this to show prompt history on GitHub PRs.

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
5. URLs without the correct key will fail to decrypt

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

### GitHub Gist Backend (No Encryption)

You can upload to GitHub Gist instead of the default server. This stores the share payload as a gist and returns the gist URL. Requires the GitHub CLI to be authenticated.

```bash
gh auth login
agentexport config set storage_type gist
```

Gists are created as secret (unlisted) by default. Gists are not encrypted and do not expire. The TTL setting is ignored. `upload_url` is ignored for the gist backend.

## Self-Hosting

[![Deploy to Cloudflare](https://deploy.workers.cloudflare.com/button)](https://deploy.workers.cloudflare.com/?url=https://github.com/nicosuave/agentexport/tree/main/worker)

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

### Configuration

Set environment variables in `wrangler.toml` under `[vars]`:

| Variable | Description | Default |
|----------|-------------|---------|
| `MAX_TTL_DAYS` | Maximum allowed retention period. Requests exceeding this are rejected. Set to `365` to disable "forever" retention. | unlimited |

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
