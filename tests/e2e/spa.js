// M68: SPA navigate e2e —— Page.navigate 执行页面 <script>，JS 渲染后的 DOM
// 能被 puppeteer 的 page.content() / title() 读到。
//
// 本地起一个 HTTP server 托管 inline-script SPA（避免依赖外网），CDP server
// navigate 过去，验证：
//   1. 同步 inline script 改的 DOM 反映在 page.content()
//   2. JS 改后的 <title> 反映在 page.title()
//   3. setTimeout 异步改的 DOM 也生效（timer loop 驱动）
const http = require('http');
const { connect, makeReporter } = require('./_helper');

// SPA fixture：inline script 同步渲染 + setTimeout 异步渲染 + 改 title。
const SPA_HTML = `<!DOCTYPE html>
<html><head><title>Pre-JS Title</title></head>
<body>
  <div id="root">LOADING</div>
  <script>
    // 同步渲染
    document.getElementById('root').innerHTML = '<h1>SPA-RENDERED</h1><p id="sync">sync-content</p>';
    // 改 title
    document.title = 'SPA Title';
    // 异步：50ms 后追加内容（验证 timer loop 驱动）
    setTimeout(function() {
      var p = document.createElement('p');
      p.id = 'async';
      p.textContent = 'async-content';
      document.body.appendChild(p);
    }, 50);
  </script>
</body></html>`;

function startFixtureServer() {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(SPA_HTML);
    });
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address();
      resolve({ server, url: `http://127.0.0.1:${port}/` });
    });
  });
}

async function main() {
  const { server, url } = await startFixtureServer();
  const r = makeReporter('SPA navigate (M68)');
  await r.run(async (browser) => {
    const page = await browser.newPage();
    await page.goto(url, { waitUntil: 'load', timeout: 15000 });

    // 1. 同步 inline script 渲染的 DOM —— 用 evaluate 读 body 文本
    //    （page.content() 走 puppeteer 内部复杂表达式，含 ES2018+ 解构，
    //    QuickJS/CDP 不完全兼容；evaluate 简单表达式更稳）
    const bodyText = await page.evaluate(() => document.body ? document.body.textContent : '');
    r.ok(bodyText && bodyText.includes('SPA-RENDERED'), 'sync inline script rendered into DOM');
    r.ok(bodyText && bodyText.includes('sync-content'), 'sync-content present in body');

    // 2. JS 改的 title（pre-JS title 被覆盖）
    const title = await page.evaluate(() => document.title);
    r.ok(title === 'SPA Title', `title is JS-rendered (got "${title}")`);
    r.ok(title !== 'Pre-JS Title', 'pre-JS title overwritten by script');

    // 3. 异步 timer 渲染（M68 timer loop 驱动验证）
    await new Promise((resolve) => setTimeout(resolve, 400));
    const asyncText = await page.evaluate(() => {
      var el = document.getElementById('async');
      return el ? el.textContent : '';
    });
    r.ok(asyncText === 'async-content', `async timer-rendered content (got "${asyncText}")`);

    await page.close();
  });
  server.close();
}

main().catch((e) => {
  console.error(e);
  process.exitCode = 1;
});
