// M48: 验证 SPA 爬虫的实际取数路径 —— page.evaluate 手写 querySelector。
// （puppeteer 的 page.$()/$$() 内部用 async generator + for-await + using，
//  是 ES2018+ 特性，boa 0.20 不支持 —— 那是引擎限制，非 CDP 协议 bug。
//  爬虫场景下用户用 page.evaluate 手写取数逻辑，这条路径必须通。）
const { connect, makeReporter, STATIC_TARGET } = require('./_helper');

const r = makeReporter('dom-via-evaluate');

r.run(async (browser) => {
  const page = await browser.newPage();
  await page.goto(STATIC_TARGET, { waitUntil: 'load', timeout: 15000 });

  // 1. h1 文本（example.com 的标题）
  const h1 = await page.evaluate(() => {
    const el = document.querySelector('h1');
    return el ? el.textContent : null;
  });
  r.ok(h1 === 'Example Domain', `document.querySelector('h1').textContent === "${h1}"`);

  // 2. p 文本
  const p = await page.evaluate(() => {
    const el = document.querySelector('p');
    return el ? el.textContent : null;
  });
  r.ok(p && p.length > 0, `document.querySelector('p') 取到段落 (len=${p ? p.length : 0})`);

  // 3. 统计 a 标签数量（example.com 有 1 个链接）
  const linkCount = await page.evaluate(() => {
    return document.querySelectorAll('a').length;
  });
  r.ok(typeof linkCount === 'number', `document.querySelectorAll('a').length === ${linkCount}`);

  // 4. document.body 存在性
  const hasBody = await page.evaluate(() => !!document.body);
  r.ok(hasBody === true, `document.body 存在 (${hasBody})`);
});
