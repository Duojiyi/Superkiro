// Local-only smoke/visual check. No backend, sample rows, or production requests.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '../dist');
const output = path.resolve(__dirname, '../visual-check');
fs.mkdirSync(output, {recursive: true});
const server = http.createServer((req, res) => {
  if (req.url.startsWith('/api/')) {res.writeHead(401, {'Content-Type': 'application/json'}); return res.end('{"error":"Local visual check: no backend session"}');}
  const relative = decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/, '') || 'index.html';
  const file = path.resolve(root, relative);
  if (!file.startsWith(root + path.sep) || !fs.existsSync(file)) {res.writeHead(404); return res.end();}
  res.setHeader('Content-Type', file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html');
  res.end(fs.readFileSync(file));
});
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await chromium.launch({headless: true, ...(process.env.CHROME_PATH ? {executablePath: process.env.CHROME_PATH} : {})});
  try {
    const page = await browser.newPage({viewport: {width: 1440, height: 1080}, deviceScaleFactor: 1});
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.route('**/*', route => new URL(route.request().url()).hostname === '127.0.0.1' ? route.continue() : route.abort());
    await page.goto(`http://127.0.0.1:${server.address().port}/admin/`);
    await page.getByRole('heading', {name: '管理员登录', exact: true}).waitFor();
    for (const [width, height] of [[1440,1080],[390,844]]) {
      await page.setViewportSize({width,height});
      assert.equal(await page.getByRole('navigation').count(),0);
      assert.equal(await page.getByRole('dialog').count(),0);
      assert.equal(await page.getByRole('button',{name:'取消',exact:true}).count(),0);
      await page.keyboard.press('Escape');
      assert.equal(await page.locator('.workspace,.sidebar').count(),0);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),true);
      await page.screenshot({path:path.join(output,`login-${width}.png`),fullPage:true});
    }
    assert.deepEqual(errors, []);
    console.log(`PASS: independent login page, desktop/mobile overflow, no cancel bypass or admin shell, no runtime errors. Screenshots: ${output}`);
  } finally {await browser.close();}
})().catch(error => {console.error(error); process.exitCode = 1;}).finally(() => server.close());
