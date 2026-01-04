import { describe, test, expect } from "bun:test";
import { readFileSync } from "fs";
import { join } from "path";

// Extract the parseMarkdownTranscript function from lib.rs
// This is the JS code embedded in the Rust file that we need to test
function parseMarkdownTranscript(text: string) {
  const data: {
    messages: Array<{
      role: string;
      content: string;
      model: string | null;
      raw: string | null;
      raw_label: string | null;
    }>;
    tool: string;
    models: string[];
    title?: string;
    shared_at?: string;
    total_input_tokens?: number;
    total_output_tokens?: number;
    total_cache_read_tokens?: number;
    total_cache_creation_tokens?: number;
  } = { messages: [], tool: "Claude Code", models: [] };

  // Extract title (first h1)
  const titleMatch = text.match(/^# (.+)$/m);
  if (titleMatch) data.title = titleMatch[1];

  // Extract metadata line
  const metaMatch = text.match(/^\*([^*]+)\*$/m);
  if (metaMatch) {
    const parts = metaMatch[1].split(" · ");
    if (parts.length > 0) data.tool = parts[0];
    if (parts.length > 1) data.models = [parts[1]];
    if (parts.length > 2) data.shared_at = parts[2];
  }

  // Split by message headers (### Role)
  // Note: We use \z for end-of-string since $ matches end-of-line in multiline mode
  // But JS doesn't support \z, so we use a two-pass approach or negative lookahead
  const msgRegex = /^### ([^\n]+)\n\n([\s\S]*?)(?=\n### |\n---|\n\*Input:)/gm;
  let match;
  while ((match = msgRegex.exec(text)) !== null) {
    const header = match[1];
    let content = match[2].trim();

    // Parse role from header
    let role = "assistant";
    let model: string | null = null;
    if (header.includes("User")) role = "user";
    else if (header.includes("Tool")) role = "tool";
    else if (header.includes("Thinking")) role = "thinking";
    else if (header.includes("System")) role = "system";

    // Extract model if present
    const modelMatch = header.match(/\(([^)]+)\)/);
    if (modelMatch) model = modelMatch[1];

    // Handle details sections
    let raw: string | null = null;
    let rawLabel: string | null = null;
    const detailsMatch = content.match(
      /<details>\s*<summary>([^<]+)<\/summary>\s*```json\s*([\s\S]*?)```\s*<\/details>/
    );
    if (detailsMatch) {
      rawLabel = detailsMatch[1];
      raw = detailsMatch[2].trim();
      content = content.replace(detailsMatch[0], "").trim();
    }

    data.messages.push({ role, content, model, raw, raw_label: rawLabel });
  }

  // Extract token stats from footer
  const statsMatch = text.match(/^\*Input: (\d+) tokens/m);
  if (statsMatch) {
    const inputMatch = text.match(/Input: (\d+) tokens/);
    const outputMatch = text.match(/Output: (\d+) tokens/);
    const cacheReadMatch = text.match(/Cache read: (\d+) tokens/);
    const cacheCreateMatch = text.match(/Cache write: (\d+) tokens/);
    if (inputMatch) data.total_input_tokens = parseInt(inputMatch[1]);
    if (outputMatch) data.total_output_tokens = parseInt(outputMatch[1]);
    if (cacheReadMatch)
      data.total_cache_read_tokens = parseInt(cacheReadMatch[1]);
    if (cacheCreateMatch)
      data.total_cache_creation_tokens = parseInt(cacheCreateMatch[1]);
  }

  return data;
}

describe("parseMarkdownTranscript", () => {
  test("parses gemini-cli transcript correctly", () => {
    const markdown = readFileSync(
      join(__dirname, "fixtures/gemini-cli-transcript.md"),
      "utf-8"
    );

    const result = parseMarkdownTranscript(markdown);

    // Check metadata
    expect(result.title).toBe("Homebrew Tap Setup Auto-Update Integration");
    expect(result.tool).toBe("Claude Code");
    expect(result.models).toEqual(["claude-opus-4-5-20251101"]);
    expect(result.shared_at).toBe("Jan 4, 2026 1:56pm");

    // Check messages
    expect(result.messages.length).toBe(5);

    // User message
    expect(result.messages[0].role).toBe("user");
    expect(result.messages[0].content).toBe(
      "hi claude what do you think of gemini cli?"
    );

    // Thinking message
    expect(result.messages[1].role).toBe("thinking");
    expect(result.messages[1].model).toBe("claude-opus-4-5-20251101");
    expect(result.messages[1].content).toContain(
      "The user is asking for my opinion on Gemini CLI"
    );

    // Assistant message - THIS IS THE CRITICAL ONE
    expect(result.messages[2].role).toBe("assistant");
    expect(result.messages[2].model).toBe("claude-opus-4-5-20251101");

    // Make sure the full content is captured, not truncated
    const assistantContent = result.messages[2].content;
    expect(assistantContent).toContain(
      "Gemini CLI is Google's answer to Claude Code"
    );
    expect(assistantContent).toContain("**Strengths:**");
    expect(assistantContent).toContain("**Limitations:**");
    expect(assistantContent).toContain("**My honest take:**");
    expect(assistantContent).toContain(
      "Is there something specific about it you're evaluating or comparing?"
    );

    // User command message
    expect(result.messages[3].role).toBe("user");
    expect(result.messages[3].content).toContain("<command-message>");

    // Tool message
    expect(result.messages[4].role).toBe("tool");
    expect(result.messages[4].content).toContain("Bash");

    // Token stats
    expect(result.total_input_tokens).toBe(10);
    expect(result.total_output_tokens).toBe(354);
    expect(result.total_cache_read_tokens).toBe(46487);
    expect(result.total_cache_creation_tokens).toBe(8573);
  });

  test("parses simple conversation", () => {
    const markdown = `# Test Title

*Claude Code · claude-sonnet-4-20250514 · Jan 1, 2025*

---

### User

Hello world

### Assistant (claude-sonnet-4-20250514)

Hi there! How can I help you today?

---

*Input: 5 tokens · Output: 10 tokens*
`;

    const result = parseMarkdownTranscript(markdown);

    expect(result.title).toBe("Test Title");
    expect(result.messages.length).toBe(2);
    expect(result.messages[0].role).toBe("user");
    expect(result.messages[0].content).toBe("Hello world");
    expect(result.messages[1].role).toBe("assistant");
    expect(result.messages[1].content).toBe("Hi there! How can I help you today?");
    expect(result.messages[1].model).toBe("claude-sonnet-4-20250514");
  });

  test("handles multiline assistant responses with markdown formatting", () => {
    const markdown = `# Multi-line Test

*Claude Code · claude-opus-4-5-20251101 · Jan 1, 2025*

---

### User

Explain something

### Assistant (claude-opus-4-5-20251101)

Here's my explanation:

**First Point:**
- Item 1
- Item 2

**Second Point:**
This has some \`code\` in it.

\`\`\`python
def hello():
    print("world")
\`\`\`

That's all!

---

*Input: 10 tokens · Output: 50 tokens*
`;

    const result = parseMarkdownTranscript(markdown);

    expect(result.messages.length).toBe(2);
    const assistant = result.messages[1];
    expect(assistant.role).toBe("assistant");
    expect(assistant.content).toContain("Here's my explanation:");
    expect(assistant.content).toContain("**First Point:**");
    expect(assistant.content).toContain("**Second Point:**");
    expect(assistant.content).toContain("def hello():");
    expect(assistant.content).toContain("That's all!");
  });

  test("bug regression: old regex with $ truncated multiline content", () => {
    // This test verifies the fix for the bug where the regex:
    //   /^### ([^\n]+)\n\n([\s\S]*?)(?=^### |^---|$)/gm
    // would truncate content because $ matches end-of-line in multiline mode.
    // The fix changes it to:
    //   /^### ([^\n]+)\n\n([\s\S]*?)(?=\n### |\n---|\n\*Input:)/gm

    const markdown = `# Test

*Claude Code · claude-opus-4-5-20251101 · Jan 1, 2025*

---

### Assistant (claude-opus-4-5-20251101)

First line.

Second line.

Third line with **bold**.

Fourth line.

---

*Input: 10 tokens · Output: 50 tokens*
`;

    const result = parseMarkdownTranscript(markdown);

    expect(result.messages.length).toBe(1);
    const content = result.messages[0].content;

    // With the old buggy regex, this would only capture "First line."
    // because $ would match the end of "First line." in multiline mode
    expect(content).toContain("First line.");
    expect(content).toContain("Second line.");
    expect(content).toContain("Third line with **bold**.");
    expect(content).toContain("Fourth line.");
  });
});
