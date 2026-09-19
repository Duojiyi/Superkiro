// Local-only smoke/visual check. No backend, sample rows, or production requests.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');
const root = path.resolve(__dirname, '../.build-check');
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
    const pages = [['overview', '运营概览'], ['cards', '卡密资产'], ['groups', '分组与权益'], ['providers', '供应商与 Key'], ['pricing', '模型与定价'], ['trace', '调用追踪'], ['finance', '财务对账'], ['security', '安全与审计'], ['announcements', '公告管理']];
    for (const [id, name] of pages) {
      await page.getByRole('navigation').getByRole('button', {name, exact: true}).click();
      await page.getByRole('heading', {name, exact: true, level: 2}).waitFor();
      await page.screenshot({path: path.join(output, `${id}-desktop.png`), fullPage: true});
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `${id} desktop overflow`);
    }
    await page.getByRole('navigation').getByRole('button', {name: '模型与定价', exact: true}).click();
    await page.getByText('高级配置 JSON · 新增条目与价格版本', {exact: true}).click();
    for (const value of ['null', '{"models":{}}', '{"models":[null,1]}', '{']) {
      await page.getByRole('textbox', {name: '配置 JSON', exact: true}).fill(value);
      await page.waitForTimeout(30);
      assert.equal(await page.getByRole('heading', {name: '模型与定价', exact: true, level: 2}).count(), 1);
    }
    await page.getByRole('navigation').getByRole('button', {name: '卡密资产', exact: true}).click();
    await page.getByRole('button', {name: '＋ 批量生成', exact: true}).click();
    const modal = page.locator('.fixed.inset-0');
    const options = await modal.getByRole('combobox', {name: '积分套餐'}).locator('option').allTextContents();
    assert.deepEqual(options, ['PRO · 1,000 积分 · 单设备', 'PRO+ · 2,000 积分 · 单设备', 'PRO Max · 5,000 积分 · 单设备', 'Power · 10,000 积分 · 单设备']);
    await page.screenshot({path: path.join(output, 'card-creation-desktop.png')});
    await modal.getByRole('button', {name: '取消', exact: true}).click();
    await page.getByRole('button', {name: '管理员登录', exact: true}).click();
    assert.equal(await page.locator('input[type=password]').count(), 1);
    await page.screenshot({path: path.join(output, 'authentication-desktop.png')});
    await page.locator('.fixed.inset-0').getByRole('button', {name: '取消', exact: true}).click();
    await page.setViewportSize({width: 390, height: 844});
    for (const [id, name] of pages) {
      await page.getByRole('navigation').getByRole('button', {name, exact: true}).click();
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, `${id} mobile overflow`);
      if (id === 'overview' || id === 'cards') await page.screenshot({path: path.join(output, `${id}-mobile.png`), fullPage: true});
    }
    assert.deepEqual(errors, []);
    console.log(`PASS: all nine tabs, desktop/mobile overflow, four card tiers, password form, no runtime errors. Screenshots: ${output}`);
  } finally {await browser.close();}
})().catch(error => {console.error(error); process.exitCode = 1;}).finally(() => server.close());
