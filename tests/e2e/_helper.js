// Shared helpers for the browser-rs CDP Puppeteer e2e suite.
//
// M48: 用真实 Puppeteer 驱动 browser-rs 的 CDP server，验证握手 + 导航 +
// 截图 + DOM + evaluate 全链路。每个测试脚本 require 本文件复用连接逻辑。
//
// 连接方式：Puppeteer 的 `connect({browserWSEndpoint})` 走 CDP 的
// /json/version 发现 + WebSocket 升级。browser-rs 在
// /devtools/browser/<target> 暴露 browser-level WS（discovery.rs）。

// puppeteer-core：只复用 CDP 客户端能力（connect），不下载 Chromium
// （我们连的是 browser-rs 自己的 CDP server，不需要 Puppeteer 的浏览器）。
const puppeteer = require('puppeteer-core');

const CDP_HOST = process.env.CDP_HOST || '127.0.0.1';
const CDP_PORT = parseInt(process.env.CDP_PORT || '9222', 10);
const WS_ENDPOINT = `ws://${CDP_HOST}:${CDP_PORT}`;

// example.com 是 W3C 维护的纯静态页（无 JS、稳定），适合做 CDP 握手/导航
// 全链路验证。SPA（cls.cn 等）留待 Page.navigate 支持 JS 后再上。
const STATIC_TARGET = 'https://example.com';

/** 连到 CDP server，返回 browser 句柄。失败抛错（测试框架据此 fail）。 */
async function connect() {
  const browser = await puppeteer.connect({ browserWSEndpoint: WS_ENDPOINT });
  return browser;
}

/** 断言 + 收集结果。每个脚本用 ok() 累计，最后统一报告。 */
function makeReporter(name) {
  const results = [];
  return {
    ok(cond, label) {
      results.push({ pass: !!cond, label });
      if (!cond) {
        console.error(`  ✗ FAIL: ${label}`);
      } else {
        console.log(`  ✓ PASS: ${label}`);
      }
    },
    async run(fn) {
      console.log(`\n=== ${name} ===`);
      let browser;
      try {
        browser = await connect();
        await fn(browser);
      } finally {
        if (browser) await browser.disconnect();
      }
      const failed = results.filter(r => !r.pass);
      console.log(
        `\n[${name}] ${results.length - failed.length}/${results.length} passed`,
      );
      if (failed.length) {
        process.exitCode = 1;
      }
    },
  };
}

module.exports = { puppeteer, connect, makeReporter, STATIC_TARGET, WS_ENDPOINT };
