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

import init, {
  extract_markdown, extract_text, extract_links,
  extract_html, extract_title,
  extract_images, extract_highlights, extract_branding,
} from "../wasm/worker_wasm.js";
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
<meta name="viewport" content="width=device-width,initial-scale=1,maximum-scale=5">
<title>Browser Fetch — SPA Scraper</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body { font-family: -apple-system, system-ui, sans-serif; background: #0d1117; color: #c9d1d9; min-height: 100vh; display: flex; flex-direction: column; }

  /* ── Header ── */
  .header { background: #161b22; padding: 10px 16px; border-bottom: 1px solid #30363d; display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
  .logo { font-weight: 700; font-size: 16px; color: #58a6ff; white-space: nowrap; }
  .tabs { display: flex; gap: 3px; }
  .tab { padding: 5px 10px; border-radius: 6px; font-size: 13px; cursor: pointer; color: #8b949e; white-space: nowrap; }
  .tab.active { background: #1f6feb; color: #fff; }
  .tab.disabled { opacity: 0.35; cursor: not-allowed; }

  /* ── Main ── */
  .main { max-width: 920px; width: 100%; margin: 0 auto; padding: 16px; flex: 1; }

  /* ── URL row ── */
  .input-row { display: flex; gap: 8px; margin-bottom: 12px; }
  .input-row input[type="text"] {
    flex: 1; min-width: 0;
    padding: 10px 12px; background: #161b22; border: 1px solid #30363d;
    border-radius: 6px; color: #c9d1d9; font-size: 14px; outline: none;
  }
  .input-row input[type="text"]:focus { border-color: #1f6feb; }
  .input-row select {
    padding: 10px 8px; background: #161b22; border: 1px solid #30363d;
    border-radius: 6px; color: #c9d1d9; font-size: 13px; cursor: pointer; outline: none;
  }
  .input-row button {
    padding: 10px 18px; background: #238636; color: #fff; border: none;
    border-radius: 6px; font-size: 14px; cursor: pointer; font-weight: 600;
    white-space: nowrap; min-height: 42px;
  }
  .input-row button:hover { background: #2ea043; }
  .input-row button:disabled { background: #21262d; color: #484f58; cursor: not-allowed; }
  .input-row button.loading { background: #1f6feb; animation: pulse 1.2s infinite; }
  @keyframes pulse { 0%,100%{opacity:1} 50%{opacity:0.6} }

  /* ── Format quick-pick chips ── */
  .formats { display: flex; flex-wrap: wrap; gap: 5px; margin-bottom: 12px; }
  .fmt {
    padding: 4px 9px; border-radius: 5px; font-size: 11px; cursor: pointer;
    border: 1px solid #30363d; background: #161b22; color: #8b949e; user-select: none;
    transition: all .15s;
  }
  .fmt:hover { border-color: #58a6ff; color: #58a6ff; }
  .fmt.active { border-color: #1f6feb; background: #0d2240; color: #58a6ff; }

  /* ── Quick-sample URLs ── */
  .samples { display: flex; flex-wrap: wrap; gap: 5px; margin-bottom: 14px; }
  .sample {
    padding: 3px 8px; border-radius: 4px; font-size: 11px; cursor: pointer;
    background: #0d2240; color: #58a6ff; border: 1px solid #1f6feb33;
    transition: all .15s;
  }
  .sample:hover { background: #1f6feb22; }

  /* ── Meta bar ── */
  .meta { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 12px; font-size: 12px; color: #8b949e; }
  .meta span { background: #161b22; padding: 3px 8px; border-radius: 4px; border: 1px solid #30363d; }

  /* ── Output ── */
  .result { margin-top: 8px; }
  pre {
    background: #161b22; border: 1px solid #30363d; border-radius: 6px;
    padding: 14px; overflow: auto; max-height: 70vh; min-height: 80px;
    white-space: pre-wrap; word-wrap: break-word; font-size: 13px; line-height: 1.5;
  }
  .placeholder { color: #484f58; }

  /* ── Footer ── */
  .footer { text-align: center; padding: 20px; font-size: 12px; color: #484f58; border-top: 1px solid #21262d; margin-top: 24px; }
  .footer a { color: #58a6ff; text-decoration: none; }

  /* ═══════════ Mobile (≤640px) ═══════════ */
  @media (max-width: 640px) {
    .header { padding: 8px 12px; }
    .main { padding: 12px; }
    .input-row { flex-direction: column; gap: 8px; }
    .input-row input[type="text"],
    .input-row select,
    .input-row button { width: 100%; }
    .input-row button { min-height: 44px; font-size: 16px; }
    .meta { gap: 6px; }
    .meta span { font-size: 11px; }
    pre { max-height: 60vh; font-size: 12px; }
    .formats { gap: 4px; }
    .fmt { font-size: 10px; padding: 4px 7px; }
  }
</style>
</head>
<body>
<div class="header">
  <div class="logo">🌐 Browser Fetch</div>
  <div class="tabs">
    <span class="tab active">Scrape</span>
    <span class="tab disabled" title="Coming soon">Search</span>
    <span class="tab disabled" title="Coming soon">Map</span>
    <span class="tab disabled" title="Coming soon">Crawl</span>
  </div>
</div>
<div class="main">

  <!-- URL input -->
  <div class="input-row">
    <input type="text" id="url" placeholder="https://example.com" value="https://vuejs.org/" autofocus>
    <select id="format">
      <option value="markdown">Markdown</option>
      <option value="html">HTML</option>
      <option value="text">Text</option>
      <option value="links">Links</option>
      <option value="images">Images</option>
      <option value="highlights">Highlights</option>
      <option value="branding">Branding</option>
    </select>
    <button id="btn" onclick="scrape()">🚀 Scrape</button>
  </div>

  <!-- Format quick-pick chips -->
  <div class="formats" id="fmt-chips">
    <span class="fmt" data-fmt="markdown" onclick="pickFormat('markdown')">Markdown</span>
    <span class="fmt" data-fmt="html" onclick="pickFormat('html')">HTML</span>
    <span class="fmt" data-fmt="text" onclick="pickFormat('text')">Text</span>
    <span class="fmt" data-fmt="links" onclick="pickFormat('links')">Links</span>
    <span class="fmt" data-fmt="images" onclick="pickFormat('images')">Images</span>
    <span class="fmt" data-fmt="highlights" onclick="pickFormat('highlights')">Highlights</span>
    <span class="fmt" data-fmt="branding" onclick="pickFormat('branding')">Branding</span>
  </div>

  <!-- Quick sample URLs -->
  <div class="samples">
    <span class="sample" onclick="fillUrl('https://vuejs.org/')">Vue.js</span>
    <span class="sample" onclick="fillUrl('https://react.dev/')">React</span>
    <span class="sample" onclick="fillUrl('https://news.ycombinator.com/')">HN</span>
    <span class="sample" onclick="fillUrl('https://example.com/')">example.com</span>
  </div>

  <!-- Meta -->
  <div class="meta">
    <span id="meta-status">Ready</span>
    <span id="meta-size"></span>
    <span id="meta-time"></span>
  </div>

  <!-- Output -->
  <div class="result">
    <pre id="output" class="placeholder">Enter a URL and click "Scrape"</pre>
  </div>
</div>

<div class="footer">
  Powered by <a href="https://browser-fetch.dyyz1993.workers.dev" target="_blank">Browser Fetch</a> &nbsp;·&nbsp; core crate compiled to wasm
</div>

<script>
async function scrape() {
  const url = document.getElementById('url').value.trim();
  if (!url) { document.getElementById('url').focus(); return; }
  const format = document.getElementById('format').value;
  const btn = document.getElementById('btn');
  const out = document.getElementById('output');
  const status = document.getElementById('meta-status');
  const sizeEl = document.getElementById('meta-size');
  const timeEl = document.getElementById('meta-time');

  btn.disabled = true;
  btn.classList.add('loading');
  btn.textContent = '⏳ Scraping';
  status.textContent = 'Fetching…';
  out.textContent = '';
  out.className = '';
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
    sizeEl.textContent = data.content ? data.content.length + ' chars' : '';
    timeEl.textContent = ms + 'ms';
  } catch (e) {
    status.textContent = 'Error: ' + e.message;
    out.textContent = 'Request failed. Check the URL and try again.';
  } finally {
    btn.disabled = false;
    btn.classList.remove('loading');
    btn.textContent = '🚀 Scrape';
  }
}

function pickFormat(fmt) {
  document.getElementById('format').value = fmt;
  document.querySelectorAll('.fmt').forEach(el => {
    el.classList.toggle('active', el.dataset.fmt === fmt);
  });
}

function fillUrl(url) {
  document.getElementById('url').value = url;
  document.getElementById('url').focus();
}

// Highlight current format chip on page load
document.addEventListener('DOMContentLoaded', () => {
  const curFmt = document.getElementById('format').value;
  document.querySelectorAll('.fmt').forEach(el => {
    el.classList.toggle('active', el.dataset.fmt === curFmt);
  });
});

// Sync format chips when select changes
document.getElementById('format').addEventListener('change', (e) => {
  document.querySelectorAll('.fmt').forEach(el => {
    el.classList.toggle('active', el.dataset.fmt === e.target.value);
  });
});

// Enter key to scrape
document.getElementById('url').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') scrape();
});
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
          case "images":
            content = extract_images(html, targetUrl);
            break;
          case "highlights":
            content = extract_highlights(html, targetUrl);
            break;
          case "branding":
            content = extract_branding(html, targetUrl);
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
