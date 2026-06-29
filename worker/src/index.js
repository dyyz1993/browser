/**
 * Cloudflare Worker 入口 — browser-fetch API
 *
 * 架构：
 *   Workers fetch(url) → 拿 HTML → wasm 解析+提取 → 返回 markdown/html/text/links
 *
 * API:
 *   GET  /            → 前端 UI 页面
 *   POST /api/scrape  → { url, format } → { title, content }
 */

import init, { extract_markdown, extract_text, extract_links, extract_html, extract_title } from "../wasm/worker_wasm.js";
import wasmModule from "../wasm/worker_wasm_bg.wasm";

let wasmInitialized = false;

async function ensureWasm() {
  if (!wasmInitialized) {
    // Workers 环境用模块导入的 wasm，不走 URL fetch
    await init(wasmModule);
    wasmInitialized = true;
  }
}

// ── 前端 UI 页面 ──────────────────────────────────────

const HTML_PAGE = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Browser Fetch — SPA Scraper</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: -apple-system, system-ui, sans-serif; background: #0d1117; color: #c9d1d9; }
  .header { background: #161b22; padding: 12px 24px; border-bottom: 1px solid #30363d; display: flex; align-items: center; gap: 16px; }
  .logo { font-weight: 700; font-size: 18px; color: #58a6ff; }
  .tabs { display: flex; gap: 4px; }
  .tab { padding: 6px 14px; border-radius: 6px; font-size: 14px; cursor: pointer; color: #8b949e; }
  .tab.active { background: #1f6feb; color: #fff; }
  .tab.disabled { opacity: 0.3; cursor: not-allowed; }
  .main { max-width: 900px; margin: 0 auto; padding: 24px; }
  .input-row { display: flex; gap: 8px; margin-bottom: 16px; }
  input[type="text"] { flex: 1; padding: 10px 14px; background: #161b22; border: 1px solid #30363d; border-radius: 6px; color: #c9d1d9; font-size: 14px; }
  select { padding: 10px; background: #161b22; border: 1px solid #30363d; border-radius: 6px; color: #c9d1d9; font-size: 14px; }
  button { padding: 10px 20px; background: #238636; color: #fff; border: none; border-radius: 6px; font-size: 14px; cursor: pointer; font-weight: 600; }
  button:hover { background: #2ea043; }
  button:disabled { background: #21262d; color: #484f58; cursor: not-allowed; }
  .result { margin-top: 16px; }
  pre { background: #161b22; border: 1px solid #30363d; border-radius: 6px; padding: 16px; overflow: auto; max-height: 600px; white-space: pre-wrap; word-wrap: break-word; font-size: 13px; line-height: 1.5; }
  .status { color: #8b949e; font-size: 13px; padding: 8px 0; }
  .meta { display: flex; gap: 16px; margin-bottom: 12px; font-size: 13px; color: #8b949e; }
  .meta span { background: #161b22; padding: 4px 10px; border-radius: 4px; border: 1px solid #30363d; }
  .formats { display: flex; flex-wrap: wrap; gap: 6px; margin-bottom: 16px; }
  .fmt { padding: 4px 10px; border-radius: 4px; font-size: 12px; cursor: pointer; border: 1px solid #30363d; background: #161b22; }
  .fmt.active { border-color: #1f6feb; background: #0d2240; color: #58a6ff; }
</style>
</head>
<body>
<div class="header">
  <div class="logo">🌐 Browser Fetch</div>
  <div class="tabs">
    <span class="tab active">Scrape</span>
    <span class="tab disabled" title="coming soon">Search</span>
    <span class="tab disabled" title="coming soon">Map</span>
    <span class="tab disabled" title="coming soon">Crawl</span>
  </div>
</div>
<div class="main">
  <div class="input-row">
    <input type="text" id="url" placeholder="https://example.com" value="https://vuejs.org/">
    <select id="format">
      <option value="markdown">Markdown</option>
      <option value="html">HTML</option>
      <option value="text">Text</option>
      <option value="links">Links</option>
    </select>
    <button id="btn" onclick="scrape()">Start scraping</button>
  </div>
  <div class="meta">
    <span id="meta-status">Ready</span>
    <span id="meta-size"></span>
    <span id="meta-time"></span>
  </div>
  <div class="result">
    <pre id="output">Enter a URL and click "Start scraping"</pre>
  </div>
</div>
<script>
async function scrape() {
  const url = document.getElementById('url').value;
  const format = document.getElementById('format').value;
  const btn = document.getElementById('btn');
  const out = document.getElementById('output');
  const status = document.getElementById('meta-status');
  const sizeEl = document.getElementById('meta-size');
  const timeEl = document.getElementById('meta-time');

  btn.disabled = true;
  status.textContent = 'Fetching...';
  out.textContent = '';
  const t0 = Date.now();

  try {
    const resp = await fetch('/api/scrape', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ url, format }),
    });
    const data = await resp.json();
    const ms = Date.now() - t0;
    out.textContent = data.content || '(empty)';
    status.textContent = data.error || 'Done';
    sizeEl.textContent = (data.content || '').length + ' chars';
    timeEl.textContent = ms + 'ms';
  } catch (e) {
    status.textContent = 'Error: ' + e.message;
  } finally {
    btn.disabled = false;
  }
}
</script>
</body>
</html>`;

// ── API 处理 ──────────────────────────────────────────

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    // 前端 UI
    if (url.pathname === "/" || url.pathname === "/index.html") {
      return new Response(HTML_PAGE, {
        headers: { "Content-Type": "text/html; charset=utf-8" },
      });
    }

    // Scrape API
    if (url.pathname === "/api/scrape" && request.method === "POST") {
      try {
        const { url: targetUrl, format } = await request.json();

        // 1. Workers fetch 拿 HTML
        const resp = await fetch(targetUrl, {
          headers: {
            "User-Agent":
              "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
          },
        });

        if (!resp.ok) {
          return json({ error: `HTTP ${resp.status}`, content: "" }, 502);
        }

        const html = await resp.text();

        // 2. wasm 解析+提取
        await ensureWasm();
        let content;
        let title;
        switch (format) {
          case "markdown":
            content = extract_markdown(html, targetUrl);
            break;
          case "text":
            content = extract_text(html, targetUrl);
            break;
          case "links":
            content = extract_links(html, targetUrl);
            break;
          case "html":
            content = extract_html(html, targetUrl);
            break;
          default:
            content = extract_markdown(html, targetUrl);
        }
        title = extract_title(html);

        return json({ url: targetUrl, title, content, format });
      } catch (e) {
        return json({ error: e.message, content: "" }, 500);
      }
    }

    // 404
    return new Response("Not Found", { status: 404 });
  },
};

function json(obj, status = 200) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
