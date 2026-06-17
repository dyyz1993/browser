// M48 场景 1：连接 → 新页 → 导航 example.com → title → 截图(base64)。
// 验证：CDP 握手、Target.createTarget、Page.navigate、Page.captureScreenshot。
const { makeReporter, STATIC_TARGET } = require('./_helper');

const r = makeReporter('basic: connect + navigate + screenshot');

r.run(async browser => {
  const page = await browser.newPage();
  await page.goto(STATIC_TARGET, { waitUntil: 'load' });

  // 1. 标题（example.com 的 <title> 是 "Example Domain"）。
  const title = await page.title();
  r.ok(typeof title === 'string' && title.length > 0, `title 非空 (got "${title}")`);
  r.ok(/example/i.test(title), `title 含 "example" (got "${title}")`);

  // 2. 截图：base64 PNG，签名应以 iVBOR 开头（PNG magic 的 base64）。
  const shot = await page.screenshot({ encoding: 'base64' });
  r.ok(typeof shot === 'string' && shot.length > 100, `screenshot 非空 (len=${shot?.length})`);
  r.ok(shot.startsWith('iVBOR'), `screenshot 是 PNG (base64 前缀 iVBOR)`);

  // 3. URL 已更新为导航目标。
  const url = page.url();
  r.ok(/example\.com/.test(url), `page.url() 含 example.com (got "${url}")`);
});
