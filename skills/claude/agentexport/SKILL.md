---
name: agentexport
description: Publish or share Claude Code session transcripts using the agentexport CLI. Use when the user asks to export, publish, or generate a share page for a Claude session.
---

# Agent Export (Claude)

## Instructions

Use the agentexport CLI to publish the current Claude session transcript:

```
agentexport publish --tool claude
```

The CLI automatically finds the transcript for the current working directory.

## Managing Shares

To list or delete previously shared transcripts:

```
agentexport shares
```
