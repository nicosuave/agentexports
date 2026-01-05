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

## Managing Shares

To list or delete previously shared transcripts:

```
agentexport shares
```
