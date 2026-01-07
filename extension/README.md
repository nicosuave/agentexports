# AgentExport PR Links Extension

Chrome extension that overlays a "Prompt History" panel on GitHub PRs and lets you jump from transcript messages to mapped edits.

## Install (developer mode)

1. Open Chrome > `chrome://extensions`
2. Enable **Developer mode**
3. Click **Load unpacked**
4. Select the `extension` folder in this repo

## Usage

1. Generate a mapping URL with `agentexport map --upload`.
2. Add a line in the PR description or a comment:

```
agentexport-map: https://example.com/your-mapping.json
```

If you use `storage_type = gist`, the URL will look like:

```
https://your-worker-domain/gm/<gist_id>
```

The panel appears on PR pages, draws arrows into diff hunks, adds hunk chips, and shows hover previews.
It supports encrypted `agentexport` blob URLs (with `#key`) and raw gist URLs.

If you paste a raw gist URL manually, add:

```
agentexport-map-proxy: https://your-worker-domain
```
