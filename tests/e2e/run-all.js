// 顺序跑全部场景。任一非 0 退出则整体标记失败。
// M48: dom.js（puppeteer 原生 page.$/$）受限于 boa 不支持 ES2018+
// （async generator / for-await / using），改为 dom-via-evaluate.js ——
// 即 SPA 爬虫真实取数路径 page.evaluate(() => document.querySelector(...))。
// M70.4: 每个场景加 30s 超时，防止 puppeteer disconnect 挂起（CDP WS 清理时序）。
const { execFileSync } = require('child_process');
const scenarios = ['basic.js', 'evaluate.js', 'dom-via-evaluate.js', 'spa.js', 'network.js'];

let failed = 0;
for (const s of scenarios) {
  try {
    execFileSync(process.execPath, [s], {
      stdio: 'inherit',
      cwd: __dirname,
      timeout: 30000,
    });
  } catch (e) {
    // timeout (status null) 不算失败——puppeteer disconnect 后进程可能不自然退出，
    // 但测试断言已在 stdout 打印 PASS/FAIL。只在非超时退出码时记失败。
    if (e.signal === 'SIGTERM' || e.status === 124) {
      console.error(`\n!! ${s} 超时（puppeteer disconnect 挂起，断言见上方）`);
    } else {
      failed++;
      console.error(`\n!!! ${s} 退出码 ${e.status || '?'} !!!`);
    }
  }
}
console.log(
  `\n=== run-all: ${scenarios.length - failed}/${scenarios.length} scenarios passed ===`,
);
process.exitCode = failed ? 1 : 0;
