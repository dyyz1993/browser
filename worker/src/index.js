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
import HTML_PAGE from "./ui.html";

let wasmInitialized = false;

async function ensureWasm() {
  if (!wasmInitialized) {
    // Workers 环境用模块导入的 wasm，不走 URL fetch
    await init(wasmModule);
    wasmInitialized = true;
  }
}

// ── 前端 UI 页面：从 text blob 加载 ───────────────────
// 模板在 ui.html（wrangler.toml text_blobs 绑定为 UI_HTML）

// ── API 处理 ──────────────────────────────────────────

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    // 前端 UI（从 ui.html 模块导入）
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
