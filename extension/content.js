(() => {
  const MAP_REGEX = /agentexport-map:/i;
  const PROXY_REGEX = /agentexport-(?:map-)?proxy:/i;
  const DEFAULT_PROXY_BASE = "https://agentexports.com";
  let currentAnchors = [];
  let previewEl = null;
  let panelEl = null;
  let attachObserver = null;
  let proxyBase = DEFAULT_PROXY_BASE;

  function parsePrPath() {
    const parts = window.location.pathname.split("/").filter(Boolean);
    if (parts.length < 4) return null;
    if (parts[2] !== "pull") return null;
    return {
      owner: parts[0],
      repo: parts[1],
      id: parts[3],
      base: `/${parts[0]}/${parts[1]}/pull/${parts[3]}`,
    };
  }

  function mappingCacheKey(info) {
    return `agentexport-map:${info.owner}/${info.repo}#${info.id}`;
  }

  function proxyCacheKey(info) {
    return `agentexport-proxy:${info.owner}/${info.repo}#${info.id}`;
  }

  function extractMappingUrl(text) {
    if (!MAP_REGEX.test(text)) return null;
    const lineMatch = text.match(/agentexport-map:\s*(.+)/i);
    if (!lineMatch) return null;
    const rest = lineMatch[1].trim();
    const markdownMatch = rest.match(/\((https?:\/\/[^\s)]+)\)/);
    if (markdownMatch) return sanitizeMappingUrl(markdownMatch[1]);
    const urlMatch = rest.match(/https?:\/\/[^\s)>\]]+/);
    if (urlMatch) return sanitizeMappingUrl(urlMatch[0]);
    return null;
  }

  function extractProxyUrl(text) {
    if (!PROXY_REGEX.test(text)) return null;
    const lineMatch = text.match(/agentexport-(?:map-)?proxy:\s*(.+)/i);
    if (!lineMatch) return null;
    const rest = lineMatch[1].trim();
    const urlMatch = rest.match(/https?:\/\/[^\s)>\]]+/);
    if (urlMatch) return sanitizeProxyUrl(urlMatch[0]);
    return null;
  }

  function extractMappingUrlFromElement(el) {
    const text = el.textContent || "";
    if (!MAP_REGEX.test(text)) return null;

    const containers = Array.from(el.querySelectorAll("p, li, pre, code, blockquote, div"))
      .filter((node) => (node.textContent || "").toLowerCase().includes("agentexport-map:"))
      .slice(0, 20);

    const candidates = [];
    for (const container of containers) {
      const anchors = Array.from(container.querySelectorAll("a[href^='http']"));
      for (const anchor of anchors) {
        const href = anchor.href || "";
        const sanitized = sanitizeMappingUrl(href);
        if (sanitized) candidates.push(sanitized);
      }
      const extracted = extractMappingUrl(container.textContent || "");
      if (extracted) candidates.push(extracted);
    }

    if (candidates.length) {
      let best = null;
      let bestScore = -1;
      for (const href of candidates) {
        let score = 0;
        if (/\/gm\//i.test(href)) score += 80;
        if (/agentexport-map/i.test(href)) score += 50;
        if (/gist\.githubusercontent\.com/i.test(href)) score += 40;
        if (/\.json(\?|#|$)/i.test(href)) score += 30;
        if (/\/blob\//i.test(href)) score += 25;
        if (/agentexports\.com/i.test(href)) score += 10;
        if (/\/v\//i.test(href)) score -= 10;
        if (/\/g\//i.test(href) && !/\/gm\//i.test(href)) score -= 100;
        if (score > bestScore) {
          bestScore = score;
          best = href;
        }
      }
      if (best) return best;
    }

    return extractMappingUrl(text);
  }

  function extractProxyUrlFromElement(el) {
    const text = el.textContent || "";
    if (!PROXY_REGEX.test(text)) return null;
    return extractProxyUrl(text);
  }

  function normalizeMappingUrl(raw) {
    const url = new URL(raw, window.location.href);
    if (url.hash && url.pathname.startsWith("/v/")) {
      url.pathname = url.pathname.replace("/v/", "/blob/");
    }
    if (url.pathname.startsWith("/g/")) {
      url.pathname = url.pathname.replace("/g/", "/gm/");
    }
    return url;
  }

  function sanitizeMappingUrl(raw) {
    let url = raw.trim();
    if (url.startsWith("<") && url.endsWith(">")) {
      url = url.slice(1, -1);
    }
    while (url.length && ")]}>.,;\"'`”.‘’".includes(url[url.length - 1])) {
      url = url.slice(0, -1);
    }
    if ((url.startsWith("\"") && url.endsWith("\"")) || (url.startsWith("'") && url.endsWith("'"))) {
      url = url.slice(1, -1);
    }
    try {
      const parsed = new URL(url);
      const host = parsed.hostname;
      if (host.startsWith("gist.githubusercontent.") && host !== "gist.githubusercontent.com") {
        parsed.hostname = "gist.githubusercontent.com";
      }
      if (host.startsWith("gist.githubuser") && host !== "gist.githubusercontent.com") {
        parsed.hostname = "gist.githubusercontent.com";
      }
      if (/\/g\//i.test(parsed.pathname) && !/\/gm\//i.test(parsed.pathname)) {
        return null;
      }
      return parsed.toString();
    } catch {
      return null;
    }
  }

  function sanitizeProxyUrl(raw) {
    try {
      const parsed = new URL(raw.trim());
      return parsed.origin;
    } catch {
      return null;
    }
  }

  function base64UrlDecode(str) {
    let input = str.replace(/-/g, "+").replace(/_/g, "/");
    const pad = input.length % 4;
    if (pad) input += "=".repeat(4 - pad);
    const bin = atob(input);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  }

  function base64Decode(str) {
    const bin = atob(str);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return bytes;
  }

  async function decompress(data) {
    const ds = new DecompressionStream("gzip");
    const writer = ds.writable.getWriter();
    writer.write(data);
    writer.close();
    const chunks = [];
    const reader = ds.readable.getReader();
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
    }
    const result = new Uint8Array(chunks.reduce((a, c) => a + c.length, 0));
    let offset = 0;
    for (const chunk of chunks) {
      result.set(chunk, offset);
      offset += chunk.length;
    }
    return new TextDecoder().decode(result);
  }

  async function decryptBlob(buffer, key) {
    const keyBytes = base64UrlDecode(key);
    if (keyBytes.length !== 32) throw new Error("Invalid mapping key");
    if (buffer.byteLength < 13) throw new Error("Invalid mapping blob");
    const iv = buffer.slice(0, 12);
    const ciphertext = buffer.slice(12);
    const cryptoKey = await crypto.subtle.importKey("raw", keyBytes, { name: "AES-GCM" }, false, [
      "decrypt",
    ]);
    const compressed = await crypto.subtle.decrypt({ name: "AES-GCM", iv }, cryptoKey, ciphertext);
    return decompress(new Uint8Array(compressed));
  }

  function proxiedUrlFor(url) {
    if (url.hostname === "gist.githubusercontent.com") {
      const proxy = new URL("/proxy", proxyBase || DEFAULT_PROXY_BASE);
      proxy.searchParams.set("url", url.toString());
      return proxy;
    }
    return url;
  }

  async function fetchMapping(mappingUrl) {
    const url = normalizeMappingUrl(mappingUrl);
    const key = url.hash.slice(1);
    url.hash = "";
    const resource = await fetchMappingResource(proxiedUrlFor(url).toString());
    if (!resource.ok) throw new Error(`failed to fetch mapping: ${resource.status}`);
    const buffer = resource.buffer;
    if (key) {
      const json = await decryptBlob(buffer, key);
      return JSON.parse(json);
    }
    const text = new TextDecoder().decode(new Uint8Array(buffer));
    if (/^\s*</.test(text)) {
      throw new Error("mapping URL returned HTML, expected JSON");
    }
    return JSON.parse(text);
  }

  async function fetchMappingResource(url) {
    try {
      const response = await fetch(url);
      const buffer = await response.arrayBuffer();
      return { ok: response.ok, status: response.status, buffer };
    } catch (err) {
      if (chrome?.runtime?.sendMessage) {
        return new Promise((resolve, reject) => {
          chrome.runtime.sendMessage({ type: "agentexport-fetch", url }, (response) => {
            if (chrome.runtime.lastError) {
              reject(new Error(chrome.runtime.lastError.message));
              return;
            }
            if (!response) {
              reject(new Error("No response from extension fetch"));
              return;
            }
            if (response.error) {
              reject(new Error(response.error));
              return;
            }
            const bytes = base64Decode(response.body_b64 || "");
            resolve({ ok: response.ok, status: response.status, buffer: bytes.buffer });
          });
        });
      }
      throw err;
    }
  }

  function findMappingUrlInDom() {
    const bodies = Array.from(document.querySelectorAll(".js-comment-body, .markdown-body"));
    for (const body of bodies) {
      const url = extractMappingUrlFromElement(body);
      if (url) return url;
    }
    return null;
  }

  function findProxyUrlInDom() {
    const bodies = Array.from(document.querySelectorAll(".js-comment-body, .markdown-body"));
    for (const body of bodies) {
      const url = extractProxyUrlFromElement(body);
      if (url) return url;
    }
    return null;
  }

  async function fetchMappingUrlFromConversation(info) {
    const response = await fetch(info.base, { credentials: "same-origin" });
    if (!response.ok) return null;
    const html = await response.text();
    const doc = new DOMParser().parseFromString(html, "text/html");
    const bodies = Array.from(doc.querySelectorAll(".js-comment-body, .markdown-body"));
    for (const body of bodies) {
      const url = extractMappingUrlFromElement(body);
      if (url) return url;
    }
    return null;
  }

  async function fetchProxyUrlFromConversation(info) {
    const response = await fetch(info.base, { credentials: "same-origin" });
    if (!response.ok) return null;
    const html = await response.text();
    const doc = new DOMParser().parseFromString(html, "text/html");
    const bodies = Array.from(doc.querySelectorAll(".js-comment-body, .markdown-body"));
    for (const body of bodies) {
      const url = extractProxyUrlFromElement(body);
      if (url) return url;
    }
    return null;
  }

  async function resolveMappingUrl() {
    const info = parsePrPath();
    if (!info) return null;
    const domUrl = findMappingUrlInDom();
    if (domUrl && isLikelyMappingUrl(domUrl)) {
      sessionStorage.setItem(mappingCacheKey(info), domUrl);
      return domUrl;
    }
    const cached = sessionStorage.getItem(mappingCacheKey(info));
    let cachedOk = false;
    if (cached) {
      if (isLikelyMappingUrl(cached)) {
        try {
          normalizeMappingUrl(cached);
          cachedOk = true;
        } catch {
          sessionStorage.removeItem(mappingCacheKey(info));
        }
      } else {
        sessionStorage.removeItem(mappingCacheKey(info));
      }
    }
    const fetched = await fetchMappingUrlFromConversation(info);
    if (fetched && isLikelyMappingUrl(fetched)) {
      sessionStorage.setItem(mappingCacheKey(info), fetched);
      return fetched;
    }
    if (cachedOk) return cached;
    return null;
  }

  function isLikelyMappingUrl(url) {
    if (!url) return false;
    if (/\/gm\//i.test(url)) return true;
    if (/\/blob\//i.test(url)) return true;
    if (/gist\.githubusercontent\.com/i.test(url)) return true;
    if (/\.json(\?|#|$)/i.test(url)) return true;
    if (/\/g\//i.test(url) && !/\/gm\//i.test(url)) return false;
    return false;
  }

  async function resolveProxyUrl() {
    const info = parsePrPath();
    if (!info) return null;
    const domUrl = findProxyUrlInDom();
    if (domUrl) {
      sessionStorage.setItem(proxyCacheKey(info), domUrl);
      return domUrl;
    }
    const cached = sessionStorage.getItem(proxyCacheKey(info));
    if (cached) {
      const normalized = sanitizeProxyUrl(cached);
      if (normalized) return normalized;
      sessionStorage.removeItem(proxyCacheKey(info));
    }
    const fetched = await fetchProxyUrlFromConversation(info);
    if (fetched) {
      sessionStorage.setItem(proxyCacheKey(info), fetched);
      return fetched;
    }
    return null;
  }

  function isFilesTab() {
    return /\/(files|changes)$/.test(window.location.pathname);
  }

  function ensureFilesTabLink(prBase) {
    if (isFilesTab()) return null;
    const link = document.createElement("a");
    link.href = `${prBase}/files`;
    link.textContent = "Open files tab";
    link.className = "agentexport-link";
    return link;
  }

  function buildPanel() {
    const panel = document.createElement("div");
    panel.id = "agentexport-panel";
    panel.innerHTML = `
      <div class="agentexport-header">
        <div class="agentexport-title">Prompt History</div>
        <div class="agentexport-subtitle">Transcript</div>
      </div>
      <div class="agentexport-body">
        <div class="agentexport-loading">Loading mapping…</div>
      </div>
    `;
    return panel;
  }

  function ensureOverlays() {
    if (!document.getElementById("agentexport-arrows")) {
      const arrows = document.createElementNS("http://www.w3.org/2000/svg", "svg");
      arrows.id = "agentexport-arrows";
      document.body.appendChild(arrows);
    }
    if (!previewEl) {
      previewEl = document.createElement("div");
      previewEl.id = "agentexport-preview";
      previewEl.classList.add("agentexport-preview-hidden");
      document.body.appendChild(previewEl);
    }
  }

  function findFilesContainer() {
    const candidates = [
      ".js-diff-progressive-container",
      "#files",
      ".js-diff-entry",
      ".js-file-list",
      "[data-testid='diff-view']",
      "[data-test-selector='pr-diff']",
    ];
    for (const selector of candidates) {
      const el = document.querySelector(selector);
      if (el && el.parentElement) {
        return { container: el, parent: el.parentElement };
      }
    }

    const fileCards = Array.from(document.querySelectorAll("div.js-file")).slice(0, 6);
    if (fileCards.length >= 2) {
      const common = normalizeContainer(lowestCommonAncestor(fileCards));
      if (common && common.parentElement) {
        if (common === document.body || common === document.documentElement) return null;
        return { container: common, parent: common.parentElement };
      }
    }

    const diffRows = Array.from(
      document.querySelectorAll(".diff-line-row, .diff-text-cell, [data-line-anchor]")
    ).slice(0, 12);
    if (diffRows.length >= 2) {
      const common = normalizeContainer(lowestCommonAncestor(diffRows));
      if (common && common.parentElement) {
        if (common === document.body || common === document.documentElement) return null;
        return { container: common, parent: common.parentElement };
      }
    }

    return null;
  }

  function findSidebarContainer() {
    const filterInput =
      document.querySelector("input[placeholder*='Filter files']") ||
      document.querySelector("input[aria-label*='Filter files']") ||
      document.querySelector("input[name='file-filter']");
    if (!filterInput) return null;
    let current = filterInput.parentElement;
    while (current) {
      if (
        current.querySelector("[data-file-name]") ||
        current.querySelector("[data-path]") ||
        current.querySelector("a[href*='#diff-']") ||
        current.querySelector("a[href*='/files']")
      ) {
        return current;
      }
      current = current.parentElement;
    }
    return null;
  }

  function lowestCommonAncestor(nodes) {
    if (!nodes.length) return null;
    let ancestors = getAncestors(nodes[0]);
    for (const node of nodes.slice(1)) {
      const set = new Set(getAncestors(node));
      ancestors = ancestors.filter((ancestor) => set.has(ancestor));
      if (!ancestors.length) break;
    }
    return ancestors[0] || null;
  }

  function directChildOf(parent, node) {
    let current = node;
    while (current && current.parentElement !== parent) {
      current = current.parentElement;
    }
    return current && current.parentElement === parent ? current : null;
  }

  function normalizeContainer(node) {
    let current = node;
    const blocked = new Set(["TBODY", "TR", "TABLE", "THEAD", "TFOOT"]);
    while (current && blocked.has(current.tagName)) {
      current = current.parentElement;
    }
    return current;
  }

  function getAncestors(node) {
    const list = [];
    let current = node;
    while (current && current !== document.documentElement) {
      list.push(current);
      current = current.parentElement;
    }
    return list;
  }

  function attachPanel(panel) {
    const target = findFilesContainer();
    if (!target) return false;
    const sidebar = findSidebarContainer();
    if (sidebar) {
      const common = lowestCommonAncestor([sidebar, target.container]);
      if (common && common !== document.body && common !== document.documentElement) {
        const sidebarChild = directChildOf(common, sidebar);
        const diffChild = directChildOf(common, target.container);
        if (sidebarChild && diffChild && sidebarChild !== diffChild) {
          let layout = document.getElementById("agentexport-layout");
          if (!layout) {
            layout = document.createElement("div");
            layout.id = "agentexport-layout";
            layout.classList.add("agentexport-layout-wide");
            common.insertBefore(layout, sidebarChild);
          }
          if (!layout.contains(panel)) layout.appendChild(panel);
          if (sidebarChild.parentElement !== layout) layout.appendChild(sidebarChild);
          if (diffChild.parentElement !== layout) layout.appendChild(diffChild);
          return true;
        }
      }
    }
    const existing = document.getElementById("agentexport-layout");
    if (existing) {
      if (!existing.contains(panel)) {
        existing.insertBefore(panel, existing.firstChild);
      }
      return true;
    }
    const layout = document.createElement("div");
    layout.id = "agentexport-layout";
    target.parent.insertBefore(layout, target.container);
    layout.appendChild(panel);
    layout.appendChild(target.container);
    return true;
  }

  function waitForFilesContainer(panel) {
    if (attachPanel(panel)) return;
    if (attachObserver) attachObserver.disconnect();
    attachObserver = new MutationObserver(() => {
      if (attachPanel(panel)) {
        attachObserver.disconnect();
        attachObserver = null;
      }
    });
    attachObserver.observe(document.body, { childList: true, subtree: true });
  }

  function truncate(text, max) {
    if (!text) return "";
    const clean = text.replace(/\s+/g, " ").trim();
    if (clean.length <= max) return clean;
    return `${clean.slice(0, max)}…`;
  }

  function findLineElement(filePath, lineNumber) {
    if (!lineNumber) return null;
    const fileEl = findFileContainer(filePath);
    if (fileEl) {
      const byNumber = fileEl.querySelector(`td[data-line-number="${lineNumber}"]`);
      if (byNumber) return byNumber;
      const byAnchor = fileEl.querySelector(`[data-line-anchor$="R${lineNumber}"]`);
      if (byAnchor) return byAnchor;
    }
    const globalNumber = document.querySelector(`td[data-line-number="${lineNumber}"]`);
    if (globalNumber) return globalNumber;
    const directAnchor = document.querySelector(`[data-line-anchor$="R${lineNumber}"]`);
    if (directAnchor) return directAnchor;
    return findLineByAnchorNumber(lineNumber);
  }

  function highlightLine(lineEl) {
    document.querySelectorAll(".agentexport-highlight").forEach((el) => {
      el.classList.remove("agentexport-highlight");
    });
    const row =
      lineEl.closest(".diff-line-row") ||
      lineEl.closest("tr") ||
      lineEl.closest("[data-grid-cell-id]");
    if (row) row.classList.add("agentexport-highlight");
    lineEl.classList.add("agentexport-highlight");
    if (lineEl.parentElement) {
      lineEl.parentElement.classList.add("agentexport-highlight");
    }
    if (lineEl.closest(".diff-text-cell")) {
      lineEl.closest(".diff-text-cell").classList.add("agentexport-highlight");
    }
    if (lineEl.closest(".diff-line-row")) {
      lineEl.closest(".diff-line-row").classList.add("agentexport-highlight");
    }
  }

  function scrollToEdit(edit) {
    if (!edit || !edit.file_path || !edit.start_line) return;
    const lineEl = findLineElement(edit.file_path, edit.start_line);
    if (lineEl) {
      lineEl.scrollIntoView({ behavior: "smooth", block: "center" });
      setTimeout(() => highlightLine(lineEl), 0);
      return;
    }
    const fileEl = findFileContainer(edit.file_path);
    if (fileEl) {
      fileEl.scrollIntoView({ behavior: "smooth", block: "start" });
    }
  }

  function findFileContainer(filePath) {
    if (!filePath) return null;
    const escaped = CSS.escape(filePath);
    const direct =
      document.querySelector(`[data-path="${escaped}"]`) ||
      document.querySelector(`[data-file-path="${escaped}"]`) ||
      document.querySelector(`[data-file-name="${escaped}"]`);
    if (direct) return direct;
    const candidates = Array.from(
      document.querySelectorAll("[data-file-path], [data-file-name]")
    );
    for (const node of candidates) {
      const value =
        node.getAttribute("data-file-path") ||
        node.getAttribute("data-file-name") ||
        "";
      if (value === filePath || value.endsWith(`/${filePath}`)) {
        return node;
      }
    }
    return null;
  }

  function findLineByAnchorNumber(lineNumber) {
    const anchors = Array.from(document.querySelectorAll("[data-line-anchor]"));
    for (const node of anchors) {
      const anchor = node.getAttribute("data-line-anchor") || "";
      const match = anchor.match(/R(\d+)\b/);
      if (match && Number(match[1]) === Number(lineNumber)) {
        return node;
      }
    }
    return null;
  }

  function renderPanel(panel, mapping, prBase) {
    const body = panel.querySelector(".agentexport-body");
    body.innerHTML = "";

    const fileLink = ensureFilesTabLink(prBase);
    if (fileLink) {
      const notice = document.createElement("div");
      notice.className = "agentexport-notice";
      notice.textContent = "Open the files tab to jump to edits.";
      notice.appendChild(fileLink);
      body.appendChild(notice);
    }

    const messagesById = new Map();
    (mapping.messages || []).forEach((msg) => messagesById.set(msg.id, msg));

    const hunksById = new Map();
    (mapping.hunks || []).forEach((hunk) => {
      hunksById.set(hunk.id, hunk);
    });

    const hunkIdsByEdit = new Map();
    (mapping.edit_hunks || []).forEach((link) => {
      if (!hunkIdsByEdit.has(link.edit_id)) hunkIdsByEdit.set(link.edit_id, []);
      hunkIdsByEdit.get(link.edit_id).push(link.hunk_id);
    });

    const editsByMessage = new Map();
    (mapping.edits || []).forEach((edit) => {
      const key = edit.user_message_id || edit.message_id;
      if (!key) return;
      if (!editsByMessage.has(key)) editsByMessage.set(key, []);
      editsByMessage.get(key).push(edit);
    });

    const messagesByHunk = new Map();
    (mapping.edit_hunks || []).forEach((link) => {
      const hunk = hunksById.get(link.hunk_id);
      if (!hunk) return;
      const edit = (mapping.edits || []).find((e) => e.id === link.edit_id);
      if (!edit) return;
      const msgId = edit.user_message_id || edit.message_id;
      if (!msgId) return;
      const msg = messagesById.get(msgId);
      if (!msg) return;
      if (!messagesByHunk.has(link.hunk_id)) messagesByHunk.set(link.hunk_id, []);
      messagesByHunk.get(link.hunk_id).push(msg);
    });

    const messages = Array.from(messagesById.values());
    const indexById = new Map();
    messages.forEach((msg, idx) => indexById.set(msg.id, idx));
    messages.sort((a, b) => {
      const aTime = a.timestamp || "";
      const bTime = b.timestamp || "";
      if (aTime && bTime) return aTime.localeCompare(bTime);
      if (aTime) return -1;
      if (bTime) return 1;
      return (indexById.get(a.id) || 0) - (indexById.get(b.id) || 0);
    });

    const rows = messages.map((msg) => {
      const edits = editsByMessage.get(msg.id) || [];
      const anchors = [];
      edits.forEach((edit) => {
        const hunkIds = hunkIdsByEdit.get(edit.id) || [];
        if (hunkIds.length === 0) {
          if (edit.start_line) {
            anchors.push({
              file_path: edit.file_path,
              line: edit.start_line,
              edit,
            });
          }
          return;
        }
        hunkIds.forEach((hunkId) => {
          const hunk = hunksById.get(hunkId);
          if (!hunk) return;
          anchors.push({
            file_path: hunk.file_path,
            line: hunk.new_start,
            edit,
            hunk,
          });
        });
      });
      return { msg, edits, anchors };
    });

    rows.sort((a, b) => {
      const aTime = a.msg.timestamp || "";
      const bTime = b.msg.timestamp || "";
      return aTime.localeCompare(bTime);
    });

    if (rows.length === 0) {
      const empty = document.createElement("div");
      empty.className = "agentexport-empty";
      empty.textContent = "No edits mapped.";
      body.appendChild(empty);
      return;
    }

    const list = document.createElement("div");
    list.className = "agentexport-list";
    const anchorEntries = [];

    rows.forEach(({ msg, edits, anchors }) => {
      const item = document.createElement("button");
      item.type = "button";
      item.className = "agentexport-item";
      if (edits.length === 0) {
        item.classList.add("agentexport-item-empty");
        item.disabled = true;
      }
      const firstAnchor = anchors[0];
      const fileMeta = firstAnchor
        ? `${firstAnchor.file_path}:${firstAnchor.line}`
        : edits[0]
          ? `${edits[0].file_path}:${edits[0].start_line || "?"}`
          : "";
      const timeMeta = msg.timestamp ? new Date(msg.timestamp).toLocaleTimeString() : "";
      const metaParts = [];
      if (edits.length) {
        metaParts.push(`${edits.length} edit${edits.length === 1 ? "" : "s"}`);
      } else {
        metaParts.push("No edits");
      }
      if (fileMeta) metaParts.push(fileMeta);
      if (timeMeta) metaParts.push(timeMeta);
      item.innerHTML = `
        <div class="agentexport-item-role">${msg.role}</div>
        <div class="agentexport-item-text">${truncate(msg.content, 180)}</div>
        <div class="agentexport-item-meta">${metaParts.join(" · ")}</div>
      `;
      if (edits.length) {
        item.addEventListener("click", () => {
          if (firstAnchor) {
            scrollToEdit({ file_path: firstAnchor.file_path, start_line: firstAnchor.line });
          } else if (edits[0]) {
            scrollToEdit(edits[0]);
          }
        });
      }
      list.appendChild(item);

      anchors.forEach((anchor) => {
        anchorEntries.push({ item, anchor });
      });
    });

    body.appendChild(list);
    currentAnchors = anchorEntries;
    requestAnimationFrame(() => drawArrows(currentAnchors));
    requestAnimationFrame(() => renderHunkChips(mapping, hunksById, messagesByHunk));
  }

  function renderHunkChips(mapping, hunksById, messagesByHunk) {
    document.querySelectorAll(".agentexport-hunk-chip").forEach((el) => el.remove());
    (mapping.hunks || []).forEach((hunk) => {
      const messages = messagesByHunk.get(hunk.id) || [];
      if (!messages.length) return;
      const chip = document.createElement("button");
      chip.type = "button";
      chip.className = "agentexport-hunk-chip";
      chip.textContent = `Prompt ×${messages.length}`;
      chip.addEventListener("mouseenter", (event) => {
        showPreview(event, messages);
      });
      chip.addEventListener("mousemove", (event) => {
        movePreview(event);
      });
      chip.addEventListener("mouseleave", hidePreview);

      const lineEl = findLineElement(hunk.file_path, hunk.new_start);
      if (!lineEl) return;
      const row = lineEl.closest("tr");
      if (!row) return;
      const hunkRow = findHunkRow(row);
      const targetCell = hunkRow.querySelector("td.blob-code, td.blob-code-inner, td");
      if (!targetCell) return;
      targetCell.appendChild(chip);
    });
  }

  function findHunkRow(startRow) {
    let row = startRow;
    while (row) {
      if (row.classList.contains("js-diff-hunk")) return row;
      row = row.previousElementSibling;
    }
    return startRow;
  }

  function showPreview(event, messages) {
    if (!previewEl) return;
    const items = messages
      .slice(0, 5)
      .map((msg) => `<div class="agentexport-preview-item"><div class="agentexport-preview-role">${msg.role}</div><div class="agentexport-preview-text">${escapeHtml(truncate(msg.content, 220))}</div></div>`)
      .join("");
    const extra = messages.length > 5 ? `<div class="agentexport-preview-more">+${messages.length - 5} more</div>` : "";
    previewEl.innerHTML = `<div class="agentexport-preview-title">Transcript</div>${items}${extra}`;
    previewEl.classList.remove("agentexport-preview-hidden");
    movePreview(event);
  }

  function movePreview(event) {
    if (!previewEl) return;
    const offset = 12;
    const maxX = window.innerWidth - previewEl.offsetWidth - offset;
    const maxY = window.innerHeight - previewEl.offsetHeight - offset;
    const x = Math.min(maxX, event.clientX + offset);
    const y = Math.min(maxY, event.clientY + offset);
    previewEl.style.transform = `translate(${x}px, ${y}px)`;
  }

  function hidePreview() {
    if (!previewEl) return;
    previewEl.classList.add("agentexport-preview-hidden");
  }

  function escapeHtml(value) {
    const div = document.createElement("div");
    div.textContent = value;
    return div.innerHTML;
  }

  function drawArrows(entries) {
    const svg = document.getElementById("agentexport-arrows");
    if (!svg) return;
    svg.setAttribute("width", `${window.innerWidth}`);
    svg.setAttribute("height", `${window.innerHeight}`);
    svg.innerHTML = "";

    entries.forEach(({ item, anchor }) => {
      const lineEl = findLineElement(anchor.file_path, anchor.line);
      if (!lineEl) return;
      const from = item.getBoundingClientRect();
      const to = lineEl.getBoundingClientRect();
      if (from.bottom < 0 || from.top > window.innerHeight) return;
      if (to.bottom < 0 || to.top > window.innerHeight) return;

      const startX = from.right;
      const startY = from.top + from.height / 2;
      const endX = Math.max(0, to.left - 8);
      const endY = to.top + to.height / 2;
      const curve = Math.max(60, Math.min(180, Math.abs(endX - startX) / 2));

      const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      path.setAttribute(
        "d",
        `M ${startX} ${startY} C ${startX + curve} ${startY}, ${endX - curve} ${endY}, ${endX} ${endY}`
      );
      path.setAttribute("class", "agentexport-arrow");
      svg.appendChild(path);

      const dot = document.createElementNS("http://www.w3.org/2000/svg", "circle");
      dot.setAttribute("cx", `${endX}`);
      dot.setAttribute("cy", `${endY}`);
      dot.setAttribute("r", "3");
      dot.setAttribute("class", "agentexport-arrow-dot");
      svg.appendChild(dot);
    });
  }

  function clearPanel() {
    if (panelEl) panelEl.remove();
    panelEl = null;
    currentAnchors = [];
  }

  async function init() {
    if (!isFilesTab()) {
      clearPanel();
      return;
    }

    ensureOverlays();
    panelEl = panelEl || buildPanel();
    waitForFilesContainer(panelEl);
    const prBase = parsePrPath()?.base || window.location.pathname.replace(/\/(files|changes)$/, "");

    try {
      const resolvedProxy = await resolveProxyUrl();
      if (resolvedProxy) proxyBase = resolvedProxy;
      let mappingUrl = await resolveMappingUrl();
      if (!mappingUrl) {
        const body = panelEl.querySelector(".agentexport-body");
        if (body) {
          body.innerHTML = `<div class="agentexport-empty">No agentexport-map URL found.</div>`;
        }
        return;
      }
      let mapping;
      try {
        mapping = await fetchMapping(mappingUrl);
      } catch (err) {
        const info = parsePrPath();
        if (info) {
          sessionStorage.removeItem(mappingCacheKey(info));
          mappingUrl = await resolveMappingUrl();
          if (mappingUrl) {
            mapping = await fetchMapping(mappingUrl);
          } else {
            throw err;
          }
        } else {
          throw err;
        }
      }
      renderPanel(panelEl, mapping, prBase);
      const onUpdate = () => {
        drawArrows(currentAnchors);
      };
      window.addEventListener("scroll", () => requestAnimationFrame(onUpdate));
      window.addEventListener("resize", () => requestAnimationFrame(onUpdate));
    } catch (err) {
      const body = panelEl.querySelector(".agentexport-body");
      if (body) {
        body.innerHTML = `<div class="agentexport-error">${err.message}</div>`;
      }
    }
  }

  init();
  document.addEventListener("turbo:render", () => init());
  document.addEventListener("pjax:end", () => init());
})();
