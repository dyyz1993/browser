// M70.4: Puppeteer e2e — Network events + cookies.
//
// 验证 Page.navigate 发出 Network.requestWillBeSent / responseReceived /
// loadingFinished 事件，且 Network.getCookies 能返回数组（不抛错）。
// 使用 example.com（稳定静态页）。
//
// 运行：node tests/e2e/network.js（需先启动 browser cdp --port 9222）

const { connect, makeReporter, STATIC_TARGET } = require('./_helper');

const reporter = makeReporter('network-events');
const { ok } = reporter;

reporter.run(async (browser) => {
  const page = await browser.newPage();

  // 1. Network.requestWillBeSent → puppeteer page.on('request')
  const requests = [];
  page.on('request', (req) => {
    requests.push({ url: req.url(), method: req.method() });
  });

  // 2. Network.responseReceived → puppeteer page.on('response')
  const responses = [];
  page.on('response', (resp) => {
    responses.push({ url: resp.url(), status: resp.status() });
  });

  // 3. Network.loadingFinished → puppeteer page.on('requestfinished')
  let requestFinished = false;
  page.on('requestfinished', () => {
    requestFinished = true;
  });

  await page.goto(STATIC_TARGET, { waitUntil: 'load', timeout: 30000 });

  const okRequest = requests.some(
    (r) => r.url.includes('example.com') && r.method === 'GET',
  );
  const okResponse = responses.some(
    (r) => r.url.includes('example.com') && r.status === 200,
  );

  console.log(
    `    captured ${requests.length} requests, ${responses.length} responses, finished=${requestFinished}`,
  );

  ok(okRequest, 'Network.requestWillBeSent emitted for document');
  ok(okResponse, 'Network.responseReceived emitted with status 200');
  ok(requestFinished, 'Network.loadingFinished (requestfinished) emitted');

  // 4. Network.getCookies — just verify the API doesn't throw
  let cookiesOk = false;
  try {
    const cookies = await page.cookies();
    cookiesOk = Array.isArray(cookies);
    console.log(`    getCookies returned ${cookies.length} cookies`);
  } catch (e) {
    console.log(`    getCookies error: ${e.message}`);
  }
  ok(cookiesOk, 'Network.getCookies returns array without error');

  await page.close();
  // Force exit — puppeteer may leave a dangling connection that prevents
  // the process from exiting naturally (CDP server WS cleanup timing).
  if (process.exitCode === undefined || process.exitCode === 0) {
    process.exit(0);
  }
});
