---
description: Publish or share Claude Code session transcripts using the agentexport CLI. Use when the user asks to export, publish, or generate a share page for a Claude session.
allowed-tools: Bash(agentexport:*)
---

# Agent Export

Publish the current Claude session transcript using agentexport.

## Instructions

1. Generate a short, descriptive title for this session (max 60 chars). Summarize what was accomplished or discussed. Examples: "Implement user authentication", "Debug API rate limiting", "Refactor database schema".

2. Use the agentexport CLI to publish the current Claude session transcript, passing the title:

```
agentexport publish --tool claude --title "<your title here>"
```

The CLI automatically finds the transcript for the current working directory.

## PR Mapping (optional)

If the user asks to map transcript edits to a PR:

1. Resolve the latest transcript automatically using `--tool claude` (or `--tool codex` if requested).
2. Run:

```
agentexport map --tool claude --base <base-commit> --head <head-commit>
```

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

## Managing Shares

To list or delete previously shared transcripts:

```
agentexport shares
```
