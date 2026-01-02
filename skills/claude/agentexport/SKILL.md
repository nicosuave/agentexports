---
name: agentexport
description: Publish or share Claude Code session transcripts using the agentexport CLI. Use when the user asks to export, publish, or generate a share page for a Claude session.
---

# Agent Export (Claude)

## Instructions

1. Use the agentexport CLI to publish the current Claude session transcript.
2. Prefer the environment variables set by the SessionStart hook:
   - `AGENTEXPORT_TERM`
   - `AGENTEXPORT_CLAUDE_TRANSCRIPT_PATH`

Run:

```
agentexport publish --tool claude --term-key "$AGENTEXPORT_TERM" --transcript "$AGENTEXPORT_CLAUDE_TRANSCRIPT_PATH"
```

This uploads to agentexports.com by default and returns a shareable URL.

If those env vars are missing, ask the user to run `agentexport setup-skills` to install the Claude hook, then restart Claude.

## Managing Shares

To list or delete previously shared transcripts:

```
agentexport shares
```
