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
      // 保留 hash（docsify 等 hash 路由站点依赖它区分页面）。
      // 爬取时才去 hash（后端 serve 会在 fetch 阶段去除）。
      const key = u.toString();
      if (seen.has(key)) continue;
      seen.add(key);
      links.push({ text: text || '(untitled)', url: key });
    } catch (e) { /* 无效 URL 跳过 */ }
  }
  return links;
}

// ── 递归探索同域 URL（BFS，深度控制，并发批量）──
// 从 targetUrl 开始，逐层提取所有同域链接。depth=1 只根页+子页。
// 同层链接并发批量（每批 3 个），防后端过载。
async function mapRecursive(env, targetUrl, depth, max) {
  const rootUrl = targetUrl.replace(/\/$/, '');
  let baseHost = '';
  try { baseHost = new URL(rootUrl).hostname; } catch (e) {}

  const visited = new Set([rootUrl]);
  const discovered = [];
  // 按层处理：先根页（depth=0），再子页（depth=1），再孙页（depth=2）……
  let currentLayer = [rootUrl];
  let currentDepth = 0;

  while (currentLayer.length > 0 && currentDepth <= depth) {
    // 并发批量处理当前层所有 URL（每批 3 个）
    const BATCH = 3;
    const nextLayer = [];

    for (let i = 0; i < currentLayer.length; i += BATCH) {
      const batch = currentLayer.slice(i, i + BATCH);
      const results = await Promise.allSettled(
        batch.map(url => scrapePage(env, url, 'links'))
      );

      for (let j = 0; j < results.length; j++) {
        const r = results[j];
        if (r.status !== 'fulfilled') continue;

        const links = parseLinks(r.value.content || '', batch[j]);
        if (!links.length) continue;

        for (const l of links) {
          if (discovered.length >= max) break;
          const cleanUrl = l.url.replace(/\/$/, '');
          try {
            const u = new URL(cleanUrl);
            if (u.hostname !== baseHost) continue;
          } catch (e) { continue; }
          if (visited.has(cleanUrl)) continue;
          visited.add(cleanUrl);
          discovered.push(l);
          // 只在下层深度时才加入下一轮（避免 depth 全满时无用入队）
          if (currentDepth + 1 <= depth) {
            nextLayer.push(l.url);
          }
        }
        if (discovered.length >= max) break;
      }
      if (discovered.length >= max) break;
    }

    currentLayer = nextLayer;
    currentDepth++;
  }

  return { discovered, all: discovered.map(l => l.url) };
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

	    // ── Map API（递归发现同域所有可达 URL）──
    if (url.pathname === "/api/map" && request.method === "POST") {
      try {
        const { url: rawUrl, depth } = await request.json();
        const targetUrl = normalizeUrl(rawUrl);
        if (!targetUrl) return json({ error: 'URL is required', links: [] }, 400);
        const d = Math.max(0, Math.min(parseInt(depth ?? 1, 10), 5));  // 0-5 级深度
        const t0 = Date.now();
        const { discovered } = await mapRecursive(env, targetUrl, d, 500);
        return json({
          url: targetUrl,
          links: discovered,
          count: discovered.length,
          _timing: { total_ms: Date.now() - t0 },
          _source: 'backend-spa',
        });
      } catch (e) {
        return json({ error: e.message, links: [] }, 500);
      }
    }

    // ── Crawl API（递归 Map → 并发 scrape 所有页面）──
    if (url.pathname === "/api/crawl" && request.method === "POST") {
      try {
        const { url: rawUrl, max, depth } = await request.json();
        const targetUrl = normalizeUrl(rawUrl);
        if (!targetUrl) return json({ error: 'URL is required', pages: [] }, 400);
        const maxPages = Math.min(Math.max(parseInt(max, 10) || 5, 1), 10);
        const d = Math.max(0, Math.min(parseInt(depth ?? 1, 10), 3));
        const t0 = Date.now();

        // 1) 递归 Map 发现全站 URL
        const rootUrl = targetUrl.replace(/\/$/, '');
        const { discovered } = await mapRecursive(env, targetUrl, d, maxPages);
        const allUrls = [rootUrl, ...discovered.map(l => l.url)];

        // 2) 并发抓所有页面（每批 3 个）
        const pages = [];
        const BATCH = 3;
        for (let i = 0; i < allUrls.length; i += BATCH) {
          const batch = allUrls.slice(i, i + BATCH);
          const results = await Promise.allSettled(
            batch.map(u => scrapePage(env, u, 'markdown'))
          );
          for (let j = 0; j < results.length; j++) {
            const r = results[j];
            if (r.status === 'fulfilled' && r.value.content) {
              pages.push({ url: batch[j], title: r.value.title || '', content: r.value.content });
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
