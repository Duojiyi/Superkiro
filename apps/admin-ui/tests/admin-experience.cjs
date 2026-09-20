// Built UI + authenticated local fixture. No deployed services, added dependencies or production writes.
// Run after npm run build: node tests/admin-experience.cjs
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const http = require('node:http'), fs = require('node:fs'), path = require('node:path'), assert = require('node:assert/strict');
const fixture = require('./fixture-api.cjs')();
const root = path.resolve(__dirname, '../dist');
const serverErrors = [];
const server = http.createServer(async (req, res) => {
  try {
    if (req.url.startsWith('/api/')) return await fixture.handle(req, res);
    const file = path.resolve(root, decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/, '') || 'index.html');
    if (!file.startsWith(root + path.sep) || !fs.existsSync(file)) {res.writeHead(404); return res.end();}
    res.setHeader('Content-Type', file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'text/html');
    res.end(fs.readFileSync(file));
  } catch (error) {serverErrors.push(error.message); res.writeHead(500); res.end(JSON.stringify({error: error.message}));}
});
async function waitFor(ready) {
  const deadline = Date.now() + 10000;
  while (!ready()) {assert(Date.now() < deadline, 'timed out waiting for request'); await new Promise(resolve => setTimeout(resolve, 10));}
}
(async () => {
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const browser = await chromium.launch({headless: true, ...(process.env.CHROME_PATH ? {executablePath: process.env.CHROME_PATH} : {})});
  try {
    const page = await browser.newPage({viewport: {width: 1280, height: 900}});
    page.setDefaultTimeout(12000);
    const origin = `http://127.0.0.1:${server.address().port}`, errors = [];
    page.on('pageerror', error => errors.push(error.message));
    await page.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort());
    let cardsMode = 'fail', tracesMode = 'fail', noticesMode = 'fail', heldCards;
    let sessionCsrf = 'fixture-csrf';
    await page.route('**/api/v1/admin/session', async route => {
      if (route.request().method() !== 'GET') return route.continue();
      const response = await route.fetch(), data = await response.json();
      return route.fulfill({response, json: {...data, csrfToken: sessionCsrf}});
    });
    await page.route('**/api/v1/admin/cards?*', async route => {
      if (cardsMode === 'fail') return route.fulfill({status: 503, json: {error: 'fixture cards unavailable'}});
      if (cardsMode === 'hold') {heldCards = route; return;}
      if (cardsMode === 'empty') return route.fulfill({json: {success: true, count: 0, cards: [], revision: 'fixture-empty'}});
      if (cardsMode === 'archived') {
        const response = await route.fetch(), data = await response.json();
        return route.fulfill({json: {...data, cards: data.cards.map(card => ({...card, status: 'voided', archivedAt: 1789790400}))}});
      }
      return route.continue();
    });
    await page.route('**/api/v1/admin/traces?*', route => tracesMode === 'fail' ? route.fulfill({json: {success: false, traces: []}}) : route.continue());
    await page.route('**/api/v1/admin/announcements', route => noticesMode === 'fail' ? route.fulfill({status: 503, json: {error: 'fixture notices unavailable'}}) : route.continue());
    const button = name => page.getByRole('button', {name, exact: true});
    const nav = name => page.getByRole('navigation').getByRole('button', {name, exact: true}).click();
    const refreshed = () => page.waitForFunction(() => [...document.querySelectorAll('.topbar button')].some(b => b.textContent === '刷新' && !b.disabled));
    const refresh = async () => {await button('刷新').click(); await refreshed();};
    const focusRecheck = async () => {
      const response = page.waitForResponse(response => response.url().endsWith('/api/v1/admin/session') && response.request().method() === 'GET');
      await page.evaluate(() => window.dispatchEvent(new Event('focus')));
      await response;
      await page.waitForFunction(() => ![...document.querySelectorAll('[role="status"]')].some(node => node.textContent?.trim() === '正在检查会话…'));
    };
    const emptyFailure = () => page.getByText('读取失败，暂时无法确认是否有记录。', {exact: true});
    await page.goto(origin + '/admin/');
    await page.getByLabel('密码', {exact: true}).fill('fixture-password'); await button('登录').click(); await refreshed();
    await nav('调用追踪'); await emptyFailure().waitFor(); assert.equal(await page.getByText('暂无调用记录', {exact: true}).count(), 0);
    await nav('公告管理'); await emptyFailure().waitFor();
    await nav('卡密资产'); await emptyFailure().waitFor();
    assert.equal(await page.getByText('尚无卡密记录。可通过「批量生成」创建卡密。', {exact: true}).count(), 0);
    cardsMode = 'hold';
    await page.locator('td').getByRole('button', {name: '重新读取数据', exact: true}).click();
    await waitFor(() => heldCards); await page.getByText('正在读取，请稍候…', {exact: true}).waitFor();
    assert.equal(await emptyFailure().count(), 0);
    cardsMode = 'empty'; await heldCards.fulfill({json: {success: true, count: 0, cards: [], revision: 'fixture-empty'}}); await refreshed();
    await page.getByText('尚无卡密记录。可通过「批量生成」创建卡密。', {exact: true}).waitFor();
    cardsMode = tracesMode = noticesMode = 'normal'; await refresh();
    assert.equal(await page.getByRole('checkbox').count(), 6);
    console.log('PASS: loading, first-read failure (HTTP and success:false), and real empty data are distinct');

    await page.getByRole('checkbox').first().check();
    await page.getByLabel('搜索卡密', {exact: true}).fill('  FIXTURE-CARD-0  ');
    const filteredCardCheckbox = page.getByRole('checkbox', {name: '选择卡密 fixture-card-0', exact: true});
    await filteredCardCheckbox.waitFor();
    await page.waitForFunction(() => !document.querySelector('input[aria-label="选择卡密 fixture-card-0"]')?.checked);
    assert.equal(await page.getByRole('checkbox').count(), 1);
    assert(!(await filteredCardCheckbox.isChecked()));
    assert((await page.getByLabel('分组筛选').locator('option').allTextContents()).includes('PRO · fixture-group-0'));
    await page.getByLabel('分组筛选').selectOption('fixture-group-1');
    await page.getByText('没有符合当前筛选条件的卡密。', {exact: true}).waitFor();
    await button('重置卡密筛选').click();
    assert.equal(await page.getByLabel('状态筛选', {exact: true}).inputValue(), 'CURRENT');
    assert.equal(await page.getByRole('checkbox').count(), 6);
    cardsMode = 'archived'; await refresh();
    await page.getByText('没有符合当前筛选条件的卡密。', {exact: true}).waitFor();
    await button('查看全部记录').click(); assert.equal(await page.getByRole('checkbox').count(), 6);
    assert.equal(await page.getByLabel('状态筛选', {exact: true}).inputValue(), 'ALL');
    cardsMode = 'normal'; await refresh(); await button('重置卡密筛选').click();
    cardsMode = 'fail'; await refresh();
    assert.equal(await page.getByRole('checkbox').count(), 6, 'last known rows must survive read failure');
    assert(await page.getByRole('checkbox').first().isDisabled());
    assert(await button('冻结').first().isDisabled());
    await page.getByRole('alert').filter({hasText: '不代表最新状态'}).waitFor();
    cardsMode = 'normal'; await refresh(); assert(!(await page.getByRole('checkbox').first().isDisabled()));
    console.log('PASS: trimmed search, named groups, reset, archived discovery, selection clearing and stale-data write guards');

    await nav('调用追踪'); await button('下一页').click();
    await page.getByLabel('追踪状态筛选').selectOption('in_progress');
    assert.equal(await button('详情 →').count(), 1); await page.getByText('处理中', {exact: true}).last().waitFor();
    assert(await button('上一页').isDisabled());
    await page.getByLabel('搜索调用记录').fill('  fixture-trace-23  '); assert.equal(await button('详情 →').count(), 1);
    await page.getByLabel('追踪状态筛选').selectOption('error');
    await page.getByText('没有符合条件的请求，请调整或清除筛选。', {exact: true}).waitFor();
    await button('清除追踪筛选').click(); assert.equal(await button('详情 →').count(), 20);
    await page.getByLabel('追踪状态筛选').selectOption('error'); assert.equal(await button('详情 →').count(), 3);
    await button('详情 →').first().click(); assert.equal(await page.evaluate(() => document.activeElement.id), 'trace-detail');
    console.log('PASS: structured trace statuses, combined search, pagination reset and detail focus');

    await nav('卡密资产'); const openBatch = button('＋ 批量生成'); await openBatch.click();
    const dialog = page.getByRole('dialog', {name: '批量生成卡密', exact: true});
    const quantity = dialog.getByLabel('生成数量', {exact: true}), generate = dialog.getByRole('button', {name: '生成并入库', exact: true});
    await dialog.getByLabel('模型与计费分组', {exact: true}).selectOption('fixture-group-0');
    for (const value of ['', '0', '501', '1.5']) {
      await quantity.fill(value); assert.equal(await quantity.inputValue(), value);
      assert.equal(await quantity.getAttribute('aria-invalid'), 'true'); assert(await generate.isDisabled());
    }
    await quantity.fill('2'); assert(!(await generate.isDisabled()));
    await page.getByText('本次生成 2 张；合计 4,000 积分。', {exact: true}).waitFor();
    page.once('dialog', async d => {assert(d.message().includes('2 张')); await d.dismiss();}); await generate.click();
    assert.equal(fixture.writes.filter(w => w.endpoint === 'cards/batch').length, 0);
    await dialog.getByRole('button', {name: '取消', exact: true}).focus(); await page.keyboard.press('Shift+Tab');
    assert(await dialog.evaluate(el => el.contains(document.activeElement)));
    await page.keyboard.press('Escape'); assert.equal(await dialog.count(), 0); assert(await openBatch.evaluate(el => el === document.activeElement));
    let batchPosts = 0, heldBatch;
    await page.route('**/api/v1/admin/cards/batch', route => {batchPosts++; heldBatch = route;});
    await openBatch.click(); page.once('dialog', d => d.accept()); await generate.click(); await waitFor(() => heldBatch);
    assert(await quantity.isDisabled()); assert(await dialog.getByLabel('积分套餐', {exact: true}).isDisabled());
    assert(await dialog.getByLabel('模型与计费分组', {exact: true}).isDisabled());
    assert.equal(await dialog.getAttribute('aria-busy'), 'true');
    await page.keyboard.press('Escape'); assert.equal(await dialog.count(), 1);
    await page.keyboard.press('Tab'); assert(await dialog.evaluate(el => el.contains(document.activeElement)));
    await dialog.getByRole('button', {name: '正在生成并入库...', exact: true}).evaluate(el => el.click()); assert.equal(batchPosts, 1);
    assert.equal(heldBatch.request().postDataJSON().count, 2);
    const batchResponse = await heldBatch.fetch(); await heldBatch.fulfill({response: batchResponse});
    const generated = page.getByRole('dialog', {name: '新生成的卡密', exact: true}); await generated.waitFor();
    page.once('dialog', d => d.dismiss()); await page.keyboard.press('Escape'); assert.equal(await generated.count(), 1);
    page.once('dialog', d => d.accept()); await page.keyboard.press('Escape'); assert.equal(await generated.count(), 0);
    assert.equal(fixture.writes.filter(w => w.endpoint === 'cards/batch').length, 1);
    await refreshed();
    console.log('PASS: batch pending fields lock, focus containment, one request, generated-secret Escape requires confirmation');
    await button('调账').first().click();
    await page.getByLabel('增减积分数量').fill('-10'); await page.getByLabel('调账原因说明').fill('fixture explanation');
    page.once('dialog', async d => {assert(d.message().includes('-10')); assert(d.message().includes('fixture explanation')); await d.dismiss();});
    await button('确认调账').click(); assert.equal(fixture.writes.filter(w => w.endpoint === 'cards/adjust').length, 0);
    await page.keyboard.press('Escape');
    page.once('dialog', async d => {assert(d.message().includes('本页面不支持解除封禁')); await d.dismiss();});
    await button('封禁').first().click(); assert.equal(fixture.writes.filter(w => w.endpoint === 'cards/status').length, 0);
    console.log('PASS: quantity validation without coercion, confirmation context, cancellation without writes, dialog focus and Escape');

    await nav('供应商与 Key');
    await page.route('**/api/v1/admin/providers/status', route => route.fulfill({json: {success: false}}));
    page.once('dialog', d => d.accept()); await button('停用此上游').click();
    await page.getByRole('alert').filter({hasText: '切换结果未确认'}).waitFor();
    assert.equal(await page.getByRole('status').filter({hasText: '状态已更新'}).count(), 0);
    await button('关闭提示').click(); await nav('财务对账');
    let prunePosts = 0, heldPrune;
    await page.route('**/api/v1/admin/traces/prune', route => {prunePosts++; heldPrune = route;});
    page.once('dialog', d => d.dismiss()); await button('永久清理 30 天前的追踪').click(); assert.equal(prunePosts, 0);
    page.once('dialog', async d => {assert(d.message().includes('审计与备份')); await d.accept();});
    await button('永久清理 30 天前的追踪').click(); await waitFor(() => heldPrune);
    assert(await button('正在清理，请稍候…').isDisabled());
    await button('正在清理，请稍候…').evaluate(el => el.click()); assert.equal(prunePosts, 1);
    assert.equal(heldPrune.request().headers()['x-csrf-token'], 'fixture-csrf');
    assert(Math.abs(heldPrune.request().postDataJSON().cutoffSecs - (Date.now() / 1000 - 30 * 86400)) < 10);
    await heldPrune.fulfill({json: {success: false}}); await page.getByRole('alert').filter({hasText: '清理结果未确认'}).waitFor();
    assert.equal(await page.getByRole('status').filter({hasText: '已清理'}).count(), 0);
    await button('关闭提示').click(); heldPrune = null;
    page.once('dialog', d => d.accept()); await button('永久清理 30 天前的追踪').click(); await waitFor(() => heldPrune);
    await heldPrune.fulfill({json: {success: true, pruned: 7}}); await page.getByRole('status').filter({hasText: '已清理 7 条调用追踪'}).waitFor();
    assert.equal(prunePosts, 2); await refreshed();
    console.log('PASS: provider negative acknowledgement; prune impact confirmation, CSRF, pending lock and accurate success/failure');

    await nav('公告管理');
    await page.getByLabel('公告标题', {exact: true}).fill('会话切换草稿');
    await page.getByLabel('正文内容', {exact: true}).fill('同一会话校验后应保留的正文');
    await nav('卡密资产');
    const sensitiveSearch = page.getByLabel('搜索卡密', {exact: true});
    await sensitiveSearch.fill('fixture-card-0');
    await button('查看卡密').first().click();
    const sensitiveDialog = page.getByRole('dialog', {name: '查看卡密', exact: true});
    await sensitiveDialog.waitFor();
    await focusRecheck();
    assert.equal(await sensitiveDialog.count(), 1, 'same CSRF must keep the sensitive dialog');
    assert.equal(await sensitiveSearch.inputValue(), 'fixture-card-0', 'same CSRF must keep card filters');
    await button('关闭并清除').click();
    await nav('公告管理');
    assert.equal(await page.getByLabel('公告标题', {exact: true}).inputValue(), '会话切换草稿');
    assert.equal(await page.getByLabel('正文内容', {exact: true}).inputValue(), '同一会话校验后应保留的正文');

    await nav('卡密资产');
    await button('查看卡密').first().click();
    await sensitiveDialog.waitFor();
    sessionCsrf = 'fixture-csrf-rotated';
    await focusRecheck();
    assert.equal(await sensitiveDialog.count(), 0, 'changed CSRF must clear plaintext');
    assert.equal(await page.getByRole('heading', {name: '运营概览', exact: true}).count(), 1, 'changed CSRF must reset the workspace');
    await nav('卡密资产');
    assert.equal(await sensitiveSearch.inputValue(), '', 'changed CSRF must clear card filters');
    await nav('公告管理');
    assert.equal(await page.getByLabel('公告标题', {exact: true}).inputValue(), '', 'changed CSRF must clear announcement drafts');
    assert.equal(await page.getByLabel('正文内容', {exact: true}).inputValue(), '', 'changed CSRF must clear announcement drafts');
    sessionCsrf = 'fixture-csrf';
    await focusRecheck();
    console.log('PASS: same CSRF focus preserves drafts and plaintext; changed CSRF remounts the sensitive workspace');

    await page.setViewportSize({width: 320, height: 740});
    for (const name of ['卡密资产', '调用追踪', '公告管理', '安全与审计']) {
      await nav(name);
      assert(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), `${name}: 320px overflow`);
    }
    await page.locator('.skip-link').focus(); await page.keyboard.press('Enter');
    assert.equal(await page.evaluate(() => document.activeElement.id), 'admin-workspace');
    let logoutConfirms = 0;
    page.on('dialog', async d => {logoutConfirms++; await d.accept();});
    await button('全部会话下线').click(); await page.getByRole('heading', {name: '管理员登录', exact: true}).waitFor();
    assert.equal(logoutConfirms, 1);
    await waitFor(() => fixture.writes.some(w => w.endpoint === 'session/revoke'));
    assert.deepEqual(fixture.writes.filter(w => w.endpoint === 'session/revoke').map(w => w.body), [{all: true}]);
    assert.deepEqual(errors, []); assert.deepEqual(serverErrors, []);
    console.log('PASS: narrow layout, keyboard skip link, no browser or fixture errors');
  } finally {await browser.close();}
})().catch(error => {console.error(error); process.exitCode = 1;}).finally(() => {server.closeAllConnections(); server.close();});
