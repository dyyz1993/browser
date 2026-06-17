// M48 场景 2：DOM 查询。验证 DOM.getDocument / querySelector / querySelectorAll。
// example.com 结构：<h1>Example Domain</h1> + 一个 <a href="https://www.iana.org/.../example">。
const { makeReporter, STATIC_TARGET } = require('./_helper');

const r = makeReporter('dom: querySelector + querySelectorAll');

r.run(async browser => {
  const page = await browser.newPage();
  await page.goto(STATIC_TARGET, { waitUntil: 'load' });

  // 1. 单元素：h1 的文本。
  const h1 = await page.$('h1');
  r.ok(h1 !== null, `$('h1') 找到元素`);
  if (h1) {
    const h1Text = await page.evaluate(el => el.textContent, h1);
    r.ok(/example/i.test(h1Text || ''), `h1 textContent 含 example (got "${h1Text}")`);
  }

  // 2. 多元素：所有 <a>。example.com 恰好 1 个。
  const links = await page.$$('a');
  r.ok(Array.isArray(links), `$$('a') 返回数组`);
  r.ok(links.length >= 1, `至少 1 个 <a> (got ${links.length})`);

  // 3. 链接 href。
  if (links.length > 0) {
    const href = await page.evaluate(el => el.href, links[0]);
    r.ok(/iana/.test(href || ''), `首个 <a> href 含 iana (got "${href}")`);
  }

  // 4. body 文本含页面主标题词。
  const bodyText = await page.evaluate(() => document.body.textContent);
  r.ok(/example/i.test(bodyText || ''), `body 含 example 字样`);
});
