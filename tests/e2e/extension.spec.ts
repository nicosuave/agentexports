import { test, expect, chromium } from "@playwright/test";
import path from "path";

const mapping = {
  base: "base",
  head: "head",
  messages: [
    {
      id: "msg-1",
      role: "user",
      content: "Please update the greeting.",
      timestamp: "2026-01-01T00:00:00Z",
      tool: "codex",
    },
  ],
  edits: [
    {
      id: "edit-1",
      tool: "codex",
      file_path: "src/app.ts",
      start_line: 3,
      end_line: 3,
      message_id: "msg-1",
      confidence: "exact",
    },
  ],
  hunks: [
    {
      id: "hunk-1",
      file_path: "src/app.ts",
      old_start: 3,
      old_lines: 1,
      new_start: 3,
      new_lines: 1,
    },
  ],
  edit_hunks: [{ edit_id: "edit-1", hunk_id: "hunk-1" }],
  errors: [],
};

const html = `<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>PR</title>
  </head>
  <body>
    <div class="js-comment-body">
      agentexport-map: https://example.com/mapping.json
    </div>
    <div id="files">
      <div class="js-diff-progressive-container">
        <div class="js-file" data-path="src/app.ts">
          <table>
            <tbody>
              <tr class="js-diff-hunk">
                <td class="blob-code">@@</td>
              </tr>
              <tr>
                <td data-line-number="3" class="blob-code">const greeting = "hi";</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </div>
  </body>
</html>`;

test("extension renders panel, chip, and preview", async () => {
  const extensionPath = path.resolve(__dirname, "../../extension");
  const browser = await chromium.launch({
    headless: true,
  });
  const context = await browser.newContext();
  const page = await context.newPage();

  await context.route("https://example.com/mapping.json", (route) => {
    route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(mapping),
    });
  });

  await context.route("https://github.com/acme/repo/pull/123/files", (route) => {
    route.fulfill({
      status: 200,
      contentType: "text/html",
      body: html,
    });
  });

  await page.goto("https://github.com/acme/repo/pull/123/files");
  await page.addStyleTag({ path: path.join(extensionPath, "styles.css") });
  await page.addScriptTag({ path: path.join(extensionPath, "content.js") });

  await expect(page.locator("#agentexport-panel")).toBeVisible();
  await expect(page.locator(".agentexport-hunk-chip")).toBeVisible();
  await page.hover(".agentexport-hunk-chip");
  await expect(page.locator("#agentexport-preview")).toBeVisible();

  await browser.close();
});
