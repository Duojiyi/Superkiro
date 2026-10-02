// Authenticated, loopback-only fixture. No real gateway, classifier, keys, or paid API.
const {chromium} = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict'), http = require('node:http'), path = require('node:path');
const fixture = require('./fixture-api.cjs')();
// Model 0 may route to either upstream; model 1 deliberately has only its original primary.
fixture.config.models[0].fallback_chain = [{provider_id:'fixture-openai',target_model:'fixture-answer'}];
const files = new Map(), writes = [], previews = [], serverErrors = [], externalRequests = [];
let config = {revision:'routing-r1', classifier:null, policies:[], audit:[]};
let status = {retained_decisions:0,budget:{day:20000,calls:0,cost_micro_cny:0},recent_decisions:[]};
let readMode = 'hold', writeMode = 'success', previewMode = 'success', referenceFail = false, releaseRead, releaseWrite, releasePreview;
const server = http.createServer(async (req, res) => {
  const reply = (value, code = 200) => {res.writeHead(code, {'Content-Type':'application/json'});res.end(JSON.stringify(value));};
  try {
    if (req.url.startsWith('/api/v1/admin/complexity-routing')) {
      assert(req.headers.cookie?.includes('fixture_session=valid'));
      if (req.method === 'GET') {
        if (readMode === 'hold') await new Promise(resolve => {releaseRead = resolve;});
        if (readMode === 'fail') return reply({error:'Fixture read unavailable'}, 500);
        if (readMode === 'malformed') return reply({success:true,config:{...config,audit:null},status});
        return reply({success:true,config,status});
      }
      assert.equal(req.method, 'POST'); assert.equal(req.headers['x-csrf-token'], 'fixture-csrf');
      let raw = ''; for await (const chunk of req) raw += chunk;
      const body = JSON.parse(raw);
      if (req.url.endsWith('/preview')) {
        assert.deepEqual(Object.keys(body).sort(), ['continuation','has_attachments','history','model_map_id','text']);
        previews.push(body); status.budget.calls++; status.budget.cost_micro_cny += 7;
        if (previewMode === 'hold') await new Promise(resolve => {releasePreview = resolve;});
        if (previewMode === 'fail') return reply({error:'Fixture preview failed'}, 503);
        if (previewMode === 'malformed') return reply({success:true,decision:{preview:true}});
        const policy = config.policies.find(p => p.model_map_id === body.model_map_id);
        const mode = policy?.mode ?? 'off';
        const decision = {invocation_id:'preview-'+previews.length,scope:'preview',request_hash:'fixture-hash',revision:config.revision,
          model_map_id:body.model_map_id,mode,complexity:previewMode === 'uncertain' ? 'unknown' : 'simple',
          reason:previewMode === 'uncertain' ? 'semantic_uncertain' : 'semantic_simple',provider_ids:policy?.simple_provider_ids ?? ['fixture-provider'],
          served_provider_id:null,created_at_secs:1700000000,pending:false,classifier_attempted:true,classifier_latency_ms:23,input_tokens:16,output_tokens:2,
          classifier_cost_micro_cny:7,usage_estimated:true,preview:true};
        status.recent_decisions = [decision, {...decision, invocation_id:'fixture-live', scope:'request', preview:false, served_provider_id:'fixture-provider'}, {...decision, invocation_id:'fixture-pending', scope:'request', preview:false, pending:true}];
        return reply({success:true,decision,eligible_provider_ids:['fixture-provider','fixture-openai'],applied_provider_ids:mode === 'enforce' ? decision.provider_ids : ['fixture-provider','fixture-openai']});
      }
      assert.equal(req.url, '/api/v1/admin/complexity-routing');
      assert.deepEqual(Object.keys(body).sort(), ['classifier','expected_revision','policies','reason']);
      assert(body.reason.trim()); assert.equal(body.reason, body.reason.trim()); writes.push(body);
      if (writeMode === 'reject') return reply({error:'Fixture config rejected'}, 400);
      if (writeMode === 'unavailable') return reply({error:'Fixture persistence unavailable'}, 503);
      if (writeMode === 'conflict') {config = {...config,revision:'routing-other-admin'};return reply({error:'Fixture revision conflict'}, 409);}
      assert.equal(body.expected_revision, config.revision);
      if (writeMode === 'hold') await new Promise(resolve => {releaseWrite = resolve;});
      config = {revision:'routing-r'+(writes.length+1),classifier:body.classifier,policies:body.policies,
        audit:[...config.audit,{revision:'routing-r'+(writes.length+1),previous_revision:body.expected_revision,reason:body.reason,created_at_secs:1700000000}]};
      if (writeMode === 'uncertain') return reply({error:'Fixture lost receipt after commit'}, 500);
      if (writeMode === 'malformed') return reply({success:true,config:{...config,audit:null}});
      return reply({success:true,config});
    }
    if (referenceFail && req.url === '/api/v1/admin/providers') return reply({error:'Fixture providers unavailable'}, 500);
    if (req.url.startsWith('/api/')) return await fixture.handle(req,res);
    const file = decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'') || 'index.html';
    if (!files.has(file)) return reply({error:'Not found'},404);
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(files.get(file));
  } catch (error) {serverErrors.push(error.message); reply({error:error.message},500);}
});
(async () => {
  let browser;
  try {
    process.chdir(path.resolve(__dirname,'..'));
    const {build} = await import('vite'), {default:react} = await import('@vitejs/plugin-react');
    const result = await build({root:path.resolve(__dirname,'..'),configFile:false,base:'/admin/',plugins:[react()],build:{write:false},logLevel:'warn'});
    for (const output of Array.isArray(result)?result:[result]) for (const item of output.output) files.set(item.fileName,item.type==='chunk'?item.code:item.source);
    await new Promise(resolve => server.listen(0,'127.0.0.1',resolve));
    browser = await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const context = await browser.newContext({viewport:{width:1440,height:1000},serviceWorkers:'block'});
    const origin = 'http://127.0.0.1:'+server.address().port;
    await context.route('**/*', route => {
      if (new URL(route.request().url()).origin === origin) return route.continue();
      externalRequests.push(route.request().url()); return route.abort();
    });
    const page = await context.newPage(), pageErrors = [];
    page.setDefaultTimeout(12000);page.on('pageerror',e => pageErrors.push(e.message));
    const button = name => page.getByRole('button',{name,exact:true});
    const field = name => page.getByLabel(name,{exact:true});
    const nav = name => page.getByRole('navigation').getByRole('button',{name,exact:true});
    const alert = text => page.getByRole('alert').filter({hasText:text});
    const dialog = page.getByRole('alertdialog');
    const accept = async () => {await dialog.waitFor();const checkbox=dialog.getByRole('checkbox');if(await checkbox.count()) await checkbox.check();await dialog.locator('[data-confirm="accept"]').click();await dialog.waitFor({state:'detached'});};
    const cancel = async () => {await dialog.waitFor();await dialog.getByRole('button',{name:'取消',exact:true}).click();await dialog.waitFor({state:'detached'});};
    const idle = () => page.waitForFunction(() => !document.querySelector('.routing-editor')?.disabled && document.querySelector('.routing-editor'));
    const saved = async () => {await page.waitForFunction(() => document.querySelector('.routing-editor input[maxlength]')?.value==='');await idle();};
    const publish = async () => {await button('保存智能分流').click();await accept();};
    const reason = field('修改原因');
    const policy = index => page.locator('.routing-policy').nth(index);
    await page.goto(origin+'/admin/');await field('密码').fill('fixture-password');await button('登录').click();await button('刷新').waitFor();
    await nav('智能分流').click();await page.getByRole('status').filter({hasText:'正在读取智能分流配置'}).waitFor();
    assert.equal(await button('保存智能分流').isDisabled(),true);
    await page.waitForFunction(() => document.querySelector('.routing-page'));
    while (!releaseRead) await new Promise(resolve => setImmediate(resolve));
    readMode='fail';releaseRead();await alert('Fixture read unavailable').waitFor();assert.equal(await reason.count(),0);
    readMode='malformed';await button('重新读取').click();await alert('服务器未返回有效智能分流').waitFor();
    readMode='success';await button('重新读取').click();await reason.waitFor();await idle();
    assert.equal(new URL(page.url()).hash,'#/routing');assert.equal(await page.title(),'智能分流 · Superkiro');
    await page.getByText('暂无分流策略，所有模型使用原路由。',{exact:true}).waitFor();await page.getByText('暂无分类记录。',{exact:true}).waitFor();
    assert.equal(await button('快速关停全部').isDisabled(),true);
    console.log('PASS: authenticated navigation, explicit loading, failed/malformed reads, retry and empty states');

    await page.getByText('保留记录：0 / 20000 条', {exact:false}).waitFor();
    status.retained_decisions=20000; await button('重新读取').click();
    await page.getByRole('status').filter({hasText:'记录容量已满'}).waitFor();
    status.retained_decisions=0; await button('重新读取').click();
    await page.getByText('保留记录：0 / 20000 条', {exact:false}).waitFor();
    await field('配置分类器（不自动开启任何模型）').check();
    assert.equal(await field('分类超时（毫秒）').inputValue(),'1500');assert.equal(await field('分类上下文字符上限').inputValue(),'4096');
    assert.equal(await field('每日分类调用上限').inputValue(),'1000');assert.equal(await field('输入价格（元/百万 token）').inputValue(),'');
    await field('分类上游').selectOption('fixture-openai');await field('分类器模型 ID（上游模型名）').fill('fixture-classifier');
    await field('分类预算（元/日）').fill('0.100001');await field('输入价格（元/百万 token）').fill('0.29');await field('输出价格（元/百万 token）').fill('1.234567');
    await field('待添加模型').selectOption('fixture-model-0');await button('添加模型策略').click();assert.equal(await field('策略 1 模式').inputValue(),'off');
    await alert('简单链至少选择一个').waitFor();await policy(0).getByRole('button',{name:'添加简单上游',exact:true}).click();
    await alert('复杂链至少选择一个').waitFor();await policy(0).getByRole('button',{name:'添加复杂上游',exact:true}).click();
    await policy(0).getByLabel('简单链第 1 上游',{exact:true}).selectOption('fixture-openai');
    await policy(0).getByRole('button',{name:'添加复杂上游',exact:true}).click();
    await policy(0).getByLabel('复杂链第 2 上游',{exact:true}).selectOption('fixture-openai');
    await policy(0).getByRole('button',{name:'复杂链第 2 项上移',exact:true}).click();assert.equal(await policy(0).getByLabel('复杂链第 1 上游',{exact:true}).inputValue(),'fixture-openai');
    await field('待添加模型').selectOption('fixture-model-1');await button('添加模型策略').click();
    await policy(1).getByRole('button',{name:'添加简单上游',exact:true}).click();
    assert.deepEqual(await policy(1).getByLabel('简单链第 1 上游',{exact:true}).locator('option').evaluateAll(els=>els.map(el=>el.value)),['fixture-provider']);
    assert.equal(await policy(1).getByRole('button',{name:'添加简单上游',exact:true}).isDisabled(),true);await policy(1).getByRole('button',{name:'添加复杂上游',exact:true}).click();
    await reason.fill('汉'.repeat(342));await alert('1024 UTF-8').waitFor();assert.equal(await button('保存智能分流').isDisabled(),true);
    await reason.fill('  initial off configuration  ');
    for (const [name,value,message,restore] of [['分类超时（毫秒）','199','200–5000','1500'],['分类上下文字符上限','16001','256–16000','4096'],['每日分类调用上限','100001','1–100000','1000'],['输入价格（元/百万 token）','0','0.000001–1000000','0.29'],['输出价格（元/百万 token）','1000000.000001','0.000001–1000000','1.234567']]) {
      await field(name).fill(value);await alert(message).waitFor();assert.equal(await button('保存智能分流').isDisabled(),true);await field(name).fill(restore);
    }
    await field('分类预算（元/日）').fill('0.0000001');await alert('最多 6 位小数').waitFor();
    await field('分类预算（元/日）').fill('0.100001');await field('预览模型').selectOption('fixture-model-0');await field('测试文本').fill('fixture text');
    assert.equal(await button('预览已保存配置').isDisabled(),true);
    await nav('运营概览').click();await cancel();assert.equal(await reason.inputValue(),'  initial off configuration  ');
    assert(await page.evaluate(() => {const e=new Event('beforeunload',{cancelable:true});window.dispatchEvent(e);return e.defaultPrevented;}));
    await button('保存智能分流').click();await dialog.waitFor();assert.equal(await dialog.locator('[data-confirm="accept"]').isDisabled(),true);
    assert.equal(await reason.isDisabled(),true);await cancel();await idle();assert.equal(writes.length,0);
    writeMode='hold';await publish();while(!releaseWrite) await new Promise(resolve=>setImmediate(resolve));
    assert.equal(await button('保存智能分流').isDisabled(),true);assert.equal(await reason.isDisabled(),true);assert.equal(writes.length,1);
    await button('保存智能分流').evaluate(el=>el.click());assert.equal(writes.length,1);
    releaseWrite();await saved();writeMode='success';
    assert.equal(writes[0].reason,'initial off configuration');assert.equal(writes[0].classifier.daily_budget_micro_cny,100001);
    assert.equal(writes[0].classifier.input_price_micro_cny_per_million,290000);assert.equal(writes[0].classifier.output_price_micro_cny_per_million,1234567);
    assert.deepEqual(writes[0].policies.map(p=>p.mode),['off','off']);assert.deepEqual(writes[0].policies[0].complex_provider_ids,['fixture-openai','fixture-provider']);
    assert(!await page.evaluate(()=>{const e=new Event('beforeunload',{cancelable:true});window.dispatchEvent(e);return e.defaultPrevented;}));
    console.log('PASS: explicit privacy confirmation, exact prices, independent default-off policies, chain order, reason limits, draft/leave guard, frozen save and duplicate prevention');

    await field('策略 1 模式').selectOption('observe');await reason.fill('enable observation');await publish();await saved();
    await field('历史上下文（可选）').fill('fixture history');await field('续写请求').check();await field('含附件（仅标记，不上传附件）').check();
    await button('预览已保存配置').click();await dialog.waitFor();assert.equal(await dialog.locator('[data-confirm="accept"]').isDisabled(),true);await cancel();await idle();assert.equal(previews.length,0);
    previewMode='hold';await button('预览已保存配置').click();await accept();while(!releasePreview) await new Promise(resolve=>setImmediate(resolve));
    assert.equal(await button('预览已保存配置').isDisabled(),true);assert.equal(await field('测试文本').isDisabled(),true);
    await button('预览已保存配置').evaluate(el=>el.click());assert.equal(previews.length,1);releasePreview();await idle();
    const resultBox=page.locator('section').filter({has:page.getByRole('heading',{name:'已保存配置预览',exact:true})});
    await resultBox.getByText(/观察模式未应用分流，仍用原顺序：测试供应商/).waitFor();
    await resultBox.getByText(/建议链：OpenAI 格式/).waitFor();await resultBox.getByText(/保守估算，可能高于实际/).waitFor();
    assert.deepEqual(previews[0],{model_map_id:'fixture-model-0',text:'fixture text',history:'fixture history',continuation:true,has_attachments:true});
    await page.getByText(/已调用 1 次 · 已用 ¥0.000007/).waitFor();await page.getByText(/最近正式请求的分类分布：简单 1/).waitFor();
    assert.equal(writes.length,2);
    await resultBox.getByText('实际服务上游：未开始（预览不调用回答模型）',{exact:true}).waitFor();
    const records = page.locator('section').filter({has:page.getByRole('heading',{name:'运营预算与最近分类',exact:true})});
    await records.locator('details').nth(1).locator('summary').click();
    await records.getByText('实际服务上游：测试供应商 / Fixture',{exact:true}).waitFor();
    await records.locator('details').nth(1).getByText('建议链：OpenAI 格式 / Fixture',{exact:true}).waitFor();
    await records.locator('details').nth(2).locator('summary').click();
    await records.getByText('实际服务上游：未确认／未开始',{exact:true}).waitFor();
    for(const mode of ['uncertain','fail','malformed']) {
      previewMode=mode;await button('预览已保存配置').click();await accept();await idle();
      if(mode==='uncertain') await resultBox.getByText(/语义分类无法确定/).waitFor();
      else await alert(mode==='fail'?'Fixture preview failed':'服务器未返回有效预览结果').waitFor();
    }
    assert.equal(previews.length,4);previewMode='success';
    console.log('PASS: paid preview consent, no duplicate or auto retry, saved snake_case payload, observe suggested/original order, estimated costs, unknown/error results and live budget');

    for(const [mode, message] of [['reject','配置被拒绝'],['unavailable','服务未保存']]) {
      writeMode=mode;await reason.fill(mode+' draft');await field('分类器模型 ID（上游模型名）').fill(mode+'-classifier');await publish();await alert(message).waitFor();await idle();
      assert.equal(await reason.inputValue(),mode+' draft');assert.equal(await button('保存智能分流').isEnabled(),true);
    }
    writeMode='conflict';await reason.fill('preserved conflict draft');await publish();await alert('版本冲突').waitFor();await idle();
    assert.equal(await button('保存智能分流').isDisabled(),true);assert.equal(await reason.isEnabled(),true);
    await button('重新读取').click();await cancel();await idle();assert.equal(await reason.inputValue(),'preserved conflict draft');
    readMode='fail';await button('重新读取').click();await accept();await alert('Fixture read unavailable').waitFor();await idle();assert.equal(await reason.inputValue(),'preserved conflict draft');
    readMode='success';await button('重新读取').click();await accept();await saved();assert.equal(await field('分类器模型 ID（上游模型名）').inputValue(),'fixture-classifier');
    writeMode='success';await reason.fill('after conflict');await publish();await saved();assert.equal(writes.at(-1).expected_revision,'routing-other-admin');
    for(const mode of ['uncertain','malformed']) {
      writeMode=mode;await reason.fill(mode+' receipt');await publish();await alert('保存结果未确认').waitFor();await idle();
      assert.equal(await button('保存智能分流').isDisabled(),true);const count=writes.length;
      await button('重新读取').click();await accept();await saved();assert.equal(writes.length,count);
    }
    writeMode='success';
    console.log('PASS: 400 and 503 retain editable drafts, 409 and uncertain/malformed receipts block stale writes; cancellation/failed refresh retain drafts, explicit refresh reconciles');

    await field('策略 1 模式').selectOption('enforce');await reason.fill('enable enforce');await publish();await saved();
    await reason.fill('ordinary draft stays');await field('分类器模型 ID（上游模型名）').fill('unsaved-classifier');
    const savedClassifier=config.classifier.model, beforeStop=writes.length;
    await button('快速关停全部').click();await cancel();await idle();assert.equal(writes.length,beforeStop);
    await button('快速关停全部').click();await accept();await idle();
    assert.equal(config.classifier.model,savedClassifier);assert(config.policies.every(p=>p.mode==='off'));
    assert.equal(await reason.inputValue(),'ordinary draft stays');assert.equal(await field('分类器模型 ID（上游模型名）').inputValue(),'unsaved-classifier');
    assert.equal(await field('策略 1 模式').inputValue(),'enforce');assert.equal(await button('快速关停全部').isDisabled(),true);
    assert.equal(await button('预览已保存配置').isDisabled(),true);await page.getByRole('status').filter({hasText:'草稿中的启用模式再次发布会重新启用'}).waitFor();
    await button('刷新').click();await page.getByRole('status').filter({hasText:'全局刷新未覆盖草稿'}).waitFor();await idle();assert.equal(await reason.inputValue(),'ordinary draft stays');
    await button('重新读取').click();await accept();await saved();
    assert.equal(await field('策略 1 模式').inputValue(),'off');
    console.log('PASS: confirmed quick shutdown publishes saved state only and keeps ordinary draft/reason/modes; global refresh preserves drafts');

    referenceFail=true;await button('刷新').click();await alert('模型或上游读取失败').waitFor();assert.equal(await button('保存智能分流').isDisabled(),true);
    referenceFail=false;await button('重试模型与上游').click();await idle();
    const originalModels=[...fixture.config.models],originalProviders=[...fixture.providers];
    fixture.config.models.length=0;fixture.providers.length=0;
    await button('刷新').click();await page.getByText(/暂无已保存模型，请先到/).waitFor();await page.getByText(/暂无已有上游，请先到/).waitFor();
    assert.equal(await button('保存智能分流').isDisabled(),true);
    fixture.config.models.push(...originalModels);fixture.providers.push(...originalProviders);await button('刷新').click();await idle();
    await page.setViewportSize({width:390,height:844});
    await page.waitForFunction(()=>document.documentElement.scrollWidth<=innerWidth+1);
    const unlabeled=await page.locator('.routing-page input,.routing-page select,.routing-page textarea').evaluateAll(els=>els.filter(el=>!el.labels?.length&&!el.getAttribute('aria-label')).length);
    assert.equal(unlabeled,0);await field('策略 1 模式').focus();await page.keyboard.press('ArrowDown');await page.keyboard.press('Tab');
    assert.equal(await field('策略 1 模式').inputValue(),'observe');
    await page.screenshot({path:path.resolve(__dirname,'../.build-check/complexity-routing-mobile.png'),fullPage:true});
    assert.deepEqual(pageErrors,[]);assert.deepEqual(serverErrors,[]);assert.deepEqual(externalRequests,[]);
    console.log('PASS: reference failures/retry, missing catalog empty states, accessible labels/keyboard, 390px no document overflow; zero page/server errors or external requests');
  } finally {
    releaseRead?.();releaseWrite?.();releasePreview?.();await browser?.close();server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
  }
})().catch(error=>{console.error(error);process.exitCode=1;});
