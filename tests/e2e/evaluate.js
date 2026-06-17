// M48 场景 3：JS 求值。验证 Runtime.evaluate（boa 引擎）。
// 注意：boa 不支持 ES6 箭头函数/class/async，所以用最简表达式 + function。
const { makeReporter, STATIC_TARGET } = require('./_helper');

const r = makeReporter('evaluate: Runtime.evaluate (boa)');

r.run(async browser => {
  const page = await browser.newPage();
  await page.goto(STATIC_TARGET, { waitUntil: 'load' });

  // 1. 纯算术（最简，boa 必须支持）。
  const sum = await page.evaluate(() => 2 + 2);
  r.ok(sum === 4, `2+2 === 4 (got ${sum})`);

  // 2. 字符串拼接。
  const greet = await page.evaluate(() => 'hello' + ' ' + 'world');
  r.ok(greet === 'hello world', `字符串拼接 (got "${greet}")`);

  // 3. document.title（boa + document_shim）。
  try {
    const title = await page.evaluate(() => document.title);
    r.ok(/example/i.test(title || ''), `document.title 含 example (got "${title}")`);
  } catch (e) {
    r.ok(false, `document.title 抛错: ${e.message}`);
  }

  // 4. location（navigation_shim / compat_shim）。
  try {
    const href = await page.evaluate(() => location.href);
    r.ok(/example\.com/.test(href || ''), `location.href 含 example.com (got "${href}")`);
  } catch (e) {
    r.ok(false, `location.href 抛错: ${e.message}`);
  }
});
