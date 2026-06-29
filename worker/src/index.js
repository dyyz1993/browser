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
const CACHE_TTL = 60_000;

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

        // 简单内存缓存：同一个 URL+格式 60s 内复用
        const cacheKey = `${targetUrl}:${format}`;
        const cached = responseCache.get(cacheKey);
        if (cached && Date.now() - cached.ts < CACHE_TTL) {
          const result = { ...cached.data };
          result._timing = cached.backendTiming || { cached: true, age: Date.now() - cached.ts };
          return json(result);
        }

        // M70.13: 智能路由——Worker 先用 wasm 静态提取（边缘执行，~100ms）
        // 如果正文够长（>500 字符），说明是 SSR/SSG 站，直接返回（省掉 NAS 往返 1-3s）
        // 如果正文太短（<100 字符），是纯 SPA 空壳，走 NAS 后端做 JS 渲染
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
        const t1 = Date.now();

        // wasm 提取
        await ensureWasm();
        const staticContent = extractStatic(html, targetUrl, format);
        const staticTitle = extract_title(html);
        const t2 = Date.now();

        const backend = env.BROWSER_BACKEND;
        const contentLen = staticContent.trim().length;

        // 正文够长 → SSR/SSG，直接返回（不走 NAS）
        // 阈值 300：example.com 这种最小站正文 165 字符，纯 SPA 壳通常 <50 字符，
        // bark 这种带点静态内容的 CSR 站 ~167 字符——给点余量，>300 才信 SSR
        if (contentLen >= 300 || !backend) {
          const result = {
            url: targetUrl,
            title: staticTitle,
            content: staticContent,
            format,
            _timing: { fetch_ms: t1 - t0, wasm_ms: t2 - t1, total_ms: t2 - t0 },
            _source: contentLen >= 500 ? 'wasm-ssr' : 'wasm-only',
          };
          responseCache.set(cacheKey, { ts: Date.now(), data: { ...result }, backendTiming: result._timing });
          return json(result);
        }

        // 正文太短 → SPA 空壳，走 NAS 后端 JS 渲染
        const t3 = Date.now();
        const backendUrl = `${backend.replace(/\/$/, '')}/`;
        const backendResp = await fetch(backendUrl, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ url: targetUrl, format, js_engine: 'quickjs' }),
        });
        const t4 = Date.now();
        if (!backendResp.ok) {
          // NAS 不可用 → 返回 Worker 的静态提取兜底
          const result = {
            url: targetUrl,
            title: staticTitle,
            content: staticContent,
            format,
            _timing: { fetch_ms: t1 - t0, wasm_ms: t2 - t1, backend_error: t4 - t3 },
            _source: 'wasm-fallback',
          };
          return json(result);
        }
        const backendResult = await backendResp.json();
        const backendTiming = backendResult._timing || {};
        backendTiming.worker_ms = t4 - t3;
        backendResult._timing = backendTiming;
        backendResult._source = 'backend-spa';
        responseCache.set(cacheKey, { ts: Date.now(), data: { ...backendResult }, backendTiming });
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
