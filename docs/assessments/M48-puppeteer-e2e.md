# M48 Puppeteer E2E Integration Test

## Purpose
Verify that our CDP implementation can be driven by real Puppeteer (not just WebSocket clients). This is the ultimate validation of CDP compatibility.

## Setup

```bash
# Install Puppeteer (no need to install Chromium)
cd /Users/xuyingzhou/Project/study-rust/browser
npm init -y
npm install puppeteer
```

## Starting the Browser

```bash
# In one terminal: start CDP server
cargo build --release
./target/release/browser cdp --port 9222
```

## Test Scenarios

### 1. Basic Navigation and Screenshot

```javascript
// tests/e2e/puppeteer_basic.js
const puppeteer = require('puppeteer');

(async () => {
  const browser = await puppeteer.connect({
    browserWSEndpoint: 'ws://127.0.0.1:9222',
  });
  const page = await browser.newPage();

  await page.goto('https://example.com');
  const title = await page.title();
  console.log('Title:', title);

  const screenshot = await page.screenshot({ encoding: 'base64' });
  console.log('Screenshot length:', screenshot.length);
  console.log('PNG signature (base64):', screenshot.substring(0, 20));

  await browser.disconnect();
})();
```

Expected output:
```
Title: Example Domain
Screenshot length: ~60000-70000 bytes
PNG signature (base64): iVBORw0KGgo (starts with "iVBOR")
```

### 2. DOM Querying

```javascript
// tests/e2e/puppeteer_dom.js
const puppeteer = require('puppeteer');

(async () => {
  const browser = await puppeteer.connect({
    browserWSEndpoint: 'ws://127.0.0.1:9222',
  });
  const page = await browser.newPage();

  await page.goto('https://example.com');

  // Single element
  const h1 = await page.$('h1');
  const h1Text = await page.evaluate(el => el.textContent, h1);
  console.log('H1:', h1Text);

  // Multiple elements
  const links = await page.$$('a');
  console.log('Links found:', links.length);
  for (const link of links) {
    const href = await page.evaluate(el => el.href, link);
    console.log('  -', href);
  }

  await browser.disconnect();
})();
```

Expected output:
```
H1: Example Domain
Links found: 1
  - https://www.iana.org/domains/example
```

### 3. JavaScript Evaluation (Limited)

```javascript
// tests/e2e/puppeteer_evaluate.js
const puppeteer = require('puppeteer');

(async () => {
  const browser = await puppeteer.connect({
    browserWSEndpoint: 'ws://127.0.0.1:9222',
  });
  const page = await browser.newPage();

  await page.goto('https://example.com');

  // Simple expression
  const result1 = await page.evaluate(() => 2 + 2);
  console.log('2 + 2 =', result1);

  // Document access (works if supported by boa)
  try {
    const result2 = await page.evaluate(() => document.title);
    console.log('document.title =', result2);
  } catch (e) {
    console.log('document.title error (expected if boa limited):', e.message);
  }

  await browser.disconnect();
})();
```

Expected output (varies by boa compatibility):
```
2 + 2 = 4
document.title = Example Domain  (or error if boa limitation)
```

## Running Tests

```bash
# Terminal 1: start server
cargo build --release
./target/release/browser cdp --port 9222

# Terminal 2: run tests
node tests/e2e/puppeteer_basic.js
node tests/e2e/puppeteer_dom.js
node tests/e2e/puppeteer_evaluate.js
```

## Known Limitations

1. **JS Compatibility**: Puppeteer's `page.evaluate()` depends on `Runtime.evaluate`, which uses our boa engine. Many modern JS features (ES6 shorthand, classes, async/await, etc.) are not supported.

2. **Unsupported CDP Methods**: Puppeteer may call methods we don't implement (e.g., `Runtime.callFunctionOn`, `Page.addScriptToEvaluateOnNewDocument`). We return empty ack responses for unknown methods to prevent crashes.

3. **Missing Domains**: No `Target`, `Emulation`, `Input`, `Performance` domains (M49+).

4. **No Full Page Lifecycle**: No `load`, `DOMContentLoaded`, `networkIdle` events via CDP (only `Page.navigate` result).

5. **Single Page**: Only one target/page (no multi-tab support).

## Success Criteria

- [ ] Puppeteer connects successfully via WebSocket
- [ ] `page.goto()` navigates and returns frameId
- [ ] `page.screenshot()` returns valid base64 PNG
- [ ] `page.$()` (querySelector) finds elements
- [ ] `page.$$()` (querySelectorAll) finds multiple elements
- [ ] `page.evaluate()` works for simple arithmetic
- [ ] No crashes or unhandled exceptions

## Results

**M48 Status**: NOT YET TESTED

To be verified after M49 (full CDP coverage) or after user confirms Puppeteer testing direction.

---

*This document is a guide for end-to-end validation with real Puppeteer library.*