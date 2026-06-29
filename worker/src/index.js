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

// 简单内存缓存（URL+格式 → 结果，60s TTL）
const responseCache = new Map();
// M70.14: 缓存 TTL——后端渲染结果缓存 5 分钟
const CACHE_TTL = 300_000;  // 5 min

async function ensureWasm() {
  if (!wasmInitialized) {
    // Workers 环境用模块导入的 wasm，不走 URL fetch
    await init(wasmModule);
    wasmInitialized = true;
  }
}

// 根据 format 分发到对应的 wasm 提取函数
function extractStatic(html, baseUrl, format) {
  switch (format) {
    case "markdown": return extract_markdown(html, baseUrl);
    case "text": return extract_text(html, baseUrl);
    case "links": return extract_links(html, baseUrl);
    case "html": return extract_html(html, baseUrl);
    case "images": return extract_images(html, baseUrl);
    case "highlights": return extract_highlights(html, baseUrl);
    case "branding": return extract_branding(html, baseUrl);
    default: return extract_markdown(html, baseUrl);
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
        const { url: rawUrl, format } = await request.json();

        // M70.13: 自动补全协议——用户输入 "bark.day.app" → "https://bark.day.app"
        let targetUrl = rawUrl.trim();
        if (!targetUrl) {
          return json({ error: 'URL is required', content: '' }, 400);
        }
        if (!/^https?:\/\//i.test(targetUrl)) {
          targetUrl = 'https://' + targetUrl;
        }

        // 简单内存缓存：同一个 URL+格式内复用
        const cacheKey = `${targetUrl}:${format}`;
        const cached = responseCache.get(cacheKey);
        if (cached) {
          const ttl = CACHE_TTL;
          if (Date.now() - cached.ts < ttl) {
            const result = { ...cached.data };
            result._timing = cached.backendTiming || { cached: true, age: Date.now() - cached.ts };
            return json(result);
          }
        }

        const backend = env.BROWSER_BACKEND;

        // M70.14: 始终走后端做完整 JS 渲染（SPA 核心价值）。
        // CF 边缘 wasm 只做后端不可用时的兜底——curl 就能做到的没意义。
        if (backend) {
          const t3 = Date.now();
          const backendUrl = `${backend.replace(/\/$/, '')}/`;
          const backendResp = await fetch(backendUrl, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ url: targetUrl, format, js_engine: 'quickjs' }),
          });
          const t4 = Date.now();
          if (backendResp.ok) {
            const backendResult = await backendResp.json();
            const backendTiming = backendResult._timing || {};
            backendTiming.worker_ms = t4 - t3;
            backendResult._timing = backendTiming;
            backendResult._source = 'backend-spa';
            responseCache.set(cacheKey, { ts: Date.now(), data: { ...backendResult }, backendTiming });
            return json(backendResult);
          }
        }

        // 后端不可用 → wasm 静态提取兜底（仅 fallback）
        const t0 = Date.now();
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
        await ensureWasm();
        const staticContent = extractStatic(html, targetUrl, format);
        const staticTitle = extract_title(html);
        const t2 = Date.now();
        const result = {
          url: targetUrl, title: staticTitle, content: staticContent, format,
          _timing: { fetch_ms: t2 - t0, wasm_ms: 0, total_ms: t2 - t0 },
          _source: 'wasm-fallback',
        };
        return json(result);
        return json(backendResult);
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
