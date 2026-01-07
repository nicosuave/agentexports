---
description: Publish this Codex session transcript using agentexport.
---

First, generate a short, descriptive title for this session (max 60 chars). Summarize what was accomplished or discussed. Examples: "Implement user authentication", "Debug API rate limiting", "Refactor database schema".

Then run:

!`agentexport publish --tool codex --max-age-minutes 120 --title "<your title here>"`

## PR Mapping (optional)

If the user asks to map transcript edits to a PR:

1. Prefer auto-resolving the transcript with `--tool codex` (and/or `--tool claude` if needed).
2. Run:

!`agentexport map --tool codex --base <base-commit> --head <head-commit>`

3. The command prints a URL line for the PR. Paste it into the PR description or a comment:

```
agentexport-map: <url>
```

If `storage_type` is `gist`, the URL will look like:

```
https://your-worker-domain/gm/<gist_id>
```

Uploads respect `storage_type` in config (agentexport = encrypted blob, gist = raw JSON).
The Chrome extension reads that URL to render arrows, chips, and hover previews on the PR.

If you paste a raw gist URL manually, also add:

```
agentexport-map-proxy: https://your-worker-domain
```

`--base` and `--head` are optional; if omitted, `map` uses `HEAD` and merge-base with the default branch.
`--tool` is required unless you pass explicit `--transcript` paths.
