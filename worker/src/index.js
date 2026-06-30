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

// 规范化用户输入的 URL：去空白、补 https://。
function normalizeUrl(rawUrl) {
  let targetUrl = (rawUrl || '').trim();
  if (!targetUrl) return null;
  if (!/^https?:\/\//i.test(targetUrl)) {
    targetUrl = 'https://' + targetUrl;
  }
  return targetUrl;
}

// 调用后端单页渲染（SPA/SSR 自动分流）。
// 返回 { ok, result } —— result 含 content/title/_timing/_source。
async function callBackend(env, targetUrl, format) {
  const backend = env.BROWSER_BACKEND;
  const t3 = Date.now();
  const backendUrl = `${backend.replace(/\/$/, '')}/`;
  const backendResp = await fetch(backendUrl, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ url: targetUrl, format, js_engine: 'quickjs' }),
  });
  const t4 = Date.now();
  if (!backendResp.ok) return { ok: false, result: null };
  const backendResult = await backendResp.json();
  const backendTiming = backendResult._timing || {};
  backendTiming.worker_ms = t4 - t3;
  backendResult._timing = backendTiming;
  backendResult._source = 'backend-spa';
  return { ok: true, result: backendResult };
}

// wasm 静态提取兜底（后端不可用时）。返回 { ok, result }。
async function callWasmFallback(targetUrl, format) {
  const t0 = Date.now();
  const resp = await fetch(targetUrl, {
    headers: {
      "User-Agent":
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
    },
  });
  if (!resp.ok) return { ok: false, result: null };
  const html = await resp.text();
  await ensureWasm();
  const staticContent = extractStatic(html, targetUrl, format);
  const staticTitle = extract_title(html);
  const t2 = Date.now();
  return {
    ok: true,
    result: {
      url: targetUrl, title: staticTitle, content: staticContent, format,
      _timing: { fetch_ms: t2 - t0, wasm_ms: 0, total_ms: t2 - t0 },
      _source: 'wasm-fallback',
    },
  };
}

// 单页渲染（带缓存）：先查缓存，命中则返回；否则后端 → wasm 兜底。
async function scrapePage(env, targetUrl, format) {
  const cacheKey = `${targetUrl}:${format}`;
  const cached = responseCache.get(cacheKey);
  if (cached && Date.now() - cached.ts < CACHE_TTL) {
    const result = { ...cached.data };
    result._timing = cached.backendTiming || { cached: true, age: Date.now() - cached.ts };
    return result;
  }
  if (env.BROWSER_BACKEND) {
    const { ok, result } = await callBackend(env, targetUrl, format);
    if (ok) {
      responseCache.set(cacheKey, { ts: Date.now(), data: { ...result }, backendTiming: result._timing });
      return result;
    }
  }
  const { ok, result } = await callWasmFallback(targetUrl, format);
  return ok ? result : { url: targetUrl, error: 'render failed', content: '' };
}

// 解析后端 links 格式（"text → URL" 每行一条）为结构化数组。
function parseLinks(content, baseUrl) {
  if (!content) return [];
  let baseHost = '';
  try { baseHost = new URL(baseUrl).hostname; } catch (e) {}
  const seen = new Set();
  const links = [];
  for (const line of content.split('\n')) {
    // 格式："anchor text → https://..."（分隔符是 3 字符：空格+箭头+空格）
    const idx = line.lastIndexOf(' → ');
    if (idx < 0) continue;
    const text = line.slice(0, idx).trim();
    let urlStr = line.slice(idx + 3).trim();
    if (!urlStr) continue;
    // 同域过滤（Map 只列本站链接）
    try {
      const u = new URL(urlStr);
      if (baseHost && u.hostname !== baseHost) continue;
      // 去重 + 去 hash fragment
      u.hash = '';
      const key = u.toString();
      if (seen.has(key)) continue;
      seen.add(key);
      links.push({ text: text || '(untitled)', url: key });
    } catch (e) { /* 无效 URL 跳过 */ }
  }
  return links;
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);

    // 前端 UI（从 ui.html 模块导入）
    if (url.pathname === "/" || url.pathname === "/index.html") {
      return new Response(HTML_PAGE, {
        headers: { "Content-Type": "text/html; charset=utf-8" },
      });
    }

    // ── Scrape API（单页渲染）──
    if (url.pathname === "/api/scrape" && request.method === "POST") {
      try {
        const { url: rawUrl, format } = await request.json();
        const targetUrl = normalizeUrl(rawUrl);
        if (!targetUrl) return json({ error: 'URL is required', content: '' }, 400);
        const result = await scrapePage(env, targetUrl, format || 'markdown');
        return json(result);
      } catch (e) {
        return json({ error: e.message, content: "" }, 500);
      }
    }

    // ── Map API（站点地图：列出同域所有可发现链接）──
    // 纯复用 scrape + format=links，Worker 端解析+同域过滤。
    if (url.pathname === "/api/map" && request.method === "POST") {
      try {
        const { url: rawUrl } = await request.json();
        const targetUrl = normalizeUrl(rawUrl);
        if (!targetUrl) return json({ error: 'URL is required', links: [] }, 400);
        const t0 = Date.now();
        const result = await scrapePage(env, targetUrl, 'links');
        const links = parseLinks(result.content || '', targetUrl);
        return json({
          url: targetUrl,
          title: result.title || '',
          links,
          count: links.length,
          _timing: { total_ms: Date.now() - t0, ...(result._timing || {}) },
          _source: result._source || '',
        });
      } catch (e) {
        return json({ error: e.message, links: [] }, 500);
      }
    }

    // ── Crawl API（递归爬取：Map 根页 → 并发抓取同域子页）──
    // Worker 层编排，后端只做单页渲染。受 Cloudflare 子请求上限保护。
    if (url.pathname === "/api/crawl" && request.method === "POST") {
      try {
        const { url: rawUrl, max } = await request.json();
        const targetUrl = normalizeUrl(rawUrl);
        if (!targetUrl) return json({ error: 'URL is required', pages: [] }, 400);
        // 安全上限：免费版 Worker 子请求 50/请求，留余量。
        const maxPages = Math.min(Math.max(parseInt(max, 10) || 5, 1), 10);
        const t0 = Date.now();

        // 1) Map 根页拿同域链接
        const mapResult = await scrapePage(env, targetUrl, 'links');
        const rootUrl = targetUrl.replace(/\/$/, '');  // 规范化去尾斜杠用于去重
        const links = parseLinks(mapResult.content || '', targetUrl)
          .filter(l => l.url.replace(/\/$/, '') !== rootUrl)  // 去掉根页自身（已单独抓）
          .slice(0, maxPages);

        // 2) 根页自身也算一页
        const pages = [];
        const visited = new Set([rootUrl]);
        const rootPage = await scrapePage(env, targetUrl, 'markdown');
        if (rootPage.content) {
          pages.push({ url: targetUrl, title: rootPage.title || '', content: rootPage.content });
        }

        // 3) 分批并发抓取子页（每批 3 个，防后端过载）
        const BATCH = 3;
        for (let i = 0; i < links.length; i += BATCH) {
          const batch = links.slice(i, i + BATCH).filter(l => {
            const k = l.url.replace(/\/$/, '');
            if (visited.has(k)) return false;
            visited.add(k);
            return true;
          });
          const results = await Promise.allSettled(
            batch.map(l => scrapePage(env, l.url, 'markdown'))
          );
          for (let j = 0; j < results.length; j++) {
            const r = results[j];
            if (r.status === 'fulfilled' && r.value.content) {
              pages.push({ url: batch[j].url, title: r.value.title || batch[j].text, content: r.value.content });
            }
          }
        }

        return json({
          url: targetUrl,
          pages,
          count: pages.length,
          _timing: { total_ms: Date.now() - t0 },
          _source: 'backend-spa',
        });
      } catch (e) {
        return json({ error: e.message, pages: [] }, 500);
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
