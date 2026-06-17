// 顺序跑全部场景。任一非 0 退出则整体标记失败。
// M48: dom.js（puppeteer 原生 page.$/$）受限于 boa 不支持 ES2018+
// （async generator / for-await / using），改为 dom-via-evaluate.js ——
// 即 SPA 爬虫真实取数路径 page.evaluate(() => document.querySelector(...))。
const { execFileSync } = require('child_process');
const scenarios = ['basic.js', 'evaluate.js', 'dom-via-evaluate.js'];

let failed = 0;
for (const s of scenarios) {
  try {
    execFileSync(process.execPath, [s], { stdio: 'inherit', cwd: __dirname });
  } catch (e) {
    failed++;
    console.error(`\n!!! ${s} 退出码 ${e.status || '?'} !!!`);
  }
}
console.log(`\n=== run-all: ${scenarios.length - failed}/${scenarios.length} scenarios passed ===`);
process.exitCode = failed ? 1 : 0;
