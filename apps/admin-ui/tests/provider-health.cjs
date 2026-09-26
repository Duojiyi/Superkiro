// 测试, live Key health and the provider format: final build + loopback fixture, never production.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const http=require('node:http'),fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const fixture=require('./fixture-api.cjs')();
const root=path.resolve(__dirname,'../dist');
const server=http.createServer(async(req,res)=>{
  try{
    if(req.url.startsWith('/api/'))return await fixture.handle(req,res);
    const file=path.resolve(root,decodeURIComponent(req.url.split('?')[0]).replace(/^\/admin\/?/,'')||'index.html');
    if(!file.startsWith(root+path.sep)||!fs.existsSync(file)){res.writeHead(404);return res.end();}
    res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':'text/html');res.end(fs.readFileSync(file));
  }catch(error){res.writeHead(500);res.end(JSON.stringify({error:error.message}));}
});
const key=id=>fixture.keys.find(row=>row.id===id);
const writes=endpoint=>fixture.writes.filter(write=>write.endpoint===endpoint);
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1440,height:1000}}),errors=[];
    page.setDefaultTimeout(10000);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const refresh=async()=>{await button('刷新').click();await page.locator('.btn-refresh:not([disabled])').waitFor();};
    // The OpenAI-format Key has stopped working; the backup Key is cooling down after an overload.
    Object.assign(key('fixture-openai-key-1'),{health_state:'unhealthy',last_error:'http_401',last_error_at:Math.floor(Date.now()/1000)-7200});
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();
    const attention=page.locator('.attention-list');
    await attention.getByText('1 个 Key 冷却中（',{exact:false}).waitFor();await attention.getByText('1 个 Key 不可用',{exact:false}).waitFor();
    assert.equal(await page.locator('#nav-badge-providers [aria-hidden="true"]').textContent(),'2');
    await nav('供应商与 Key');
    // The format comes from the server's `format` field.
    const card=name=>page.locator('.provider-card').filter({hasText:name});
    await card('OpenAI 格式 / Fixture').locator('.provider-name').getByText('OpenAI',{exact:true}).waitFor();
    await card('测试供应商 / Fixture').locator('.provider-name').getByText('Anthropic',{exact:true}).waitFor();
    const keyRow=(provider,id)=>card(provider).getByRole('row').filter({hasText:id});
    await keyRow('OpenAI 格式 / Fixture','fixture-openai-key-1').getByText('不可用',{exact:true}).waitFor();
    await keyRow('OpenAI 格式 / Fixture','fixture-openai-key-1').getByText('2 小时前 · HTTP 401 · Key 无效或被拒绝').waitFor();
    // The server names the failure's kind (http_529); the console says it in words.
    await keyRow('测试供应商 / Fixture','fixture-backup').getByText('5 分钟前 · HTTP 529 · 上游过载').waitFor();
    assert.equal(await keyRow('测试供应商 / Fixture','fixture-key').getByRole('button',{name:'恢复',exact:true}).count(),0,'a healthy Key needs no 恢复');
    // 恢复 clears the state; the badge follows.
    await keyRow('测试供应商 / Fixture','fixture-backup').getByRole('button',{name:'恢复',exact:true}).click();
    const box=page.getByRole('alertdialog');await box.waitFor();const facts=await box.innerText();
    assert(facts.includes('现在：冷却中')&&facts.includes('最近错误：HTTP 529 · 上游过载'),facts);
    await box.locator('[data-confirm="accept"]').click();
    await page.locator('.toast').filter({hasText:'已恢复 Key fixture-backup'}).waitFor();
    assert.deepEqual(writes('providers/keys/reset').map(write=>write.body),[{provider_id:'fixture-provider',key_id:'fixture-backup'}]);
    await keyRow('测试供应商 / Fixture','fixture-backup').getByText('正常',{exact:true}).waitFor();
    await keyRow('测试供应商 / Fixture','fixture-backup').getByText('5 分钟前 · HTTP 529 · 上游过载').waitFor();// the last error stays on record
    assert.equal(await page.locator('#nav-badge-providers [aria-hidden="true"]').textContent(),'1');
    console.log('PASS: live Key health with the last error, 恢复 clears it and the badge follows; format tags from the server’s format');

    // 测试 in a Key's model list: through that Key, nothing saved.
    key('fixture-key').allowed_models=[...key('fixture-key').allowed_models,'overloaded-model'];await refresh();
    await keyRow('测试供应商 / Fixture','fixture-key').getByRole('button',{name:'编辑',exact:true}).click();
    const picker=page.getByRole('group',{name:'可用模型'});
    const item=name=>picker.locator('li').filter({has:page.getByRole('checkbox',{name,exact:true})});
    await item('claude-sonnet').getByRole('button',{name:'测试',exact:true}).click();
    await item('claude-sonnet').getByRole('status').filter({hasText:'成功 · 首字 410 ms'}).waitFor();
    assert.equal(await item('claude-sonnet').getByRole('status').textContent(),'成功 · 首字 410 ms','the Key tested is the one in the editor: not named again');
    assert.deepEqual(writes('providers/keys/probe').at(-1).body,{provider_id:'fixture-provider',model:'claude-sonnet',key_id:'fixture-key'});
    await item('overloaded-model').getByRole('button',{name:'测试',exact:true}).click();
    await item('overloaded-model').getByRole('status').filter({hasText:'失败：HTTP 529 overloaded_error: Overloaded'}).waitFor();
    assert.equal(writes('providers/keys').length,0,'testing saves nothing');
    // 测试 on a model's row: through its primary route, the server choosing the Key.
    await page.locator('#key-editor').getByRole('button',{name:'关闭编辑器',exact:true}).click();
    await nav('模型与定价');
    const modelRow=page.getByRole('row').filter({has:page.getByRole('button',{name:'claude-sonnet 的更多操作',exact:true})});
    await modelRow.getByRole('button',{name:'测试',exact:true}).click();
    await modelRow.getByRole('status').filter({hasText:'成功 · 首字 410 ms · Key fixture-key'}).waitFor();
    assert.deepEqual(writes('providers/keys/probe').at(-1).body,{provider_id:'fixture-provider',model:'claude-sonnet'});
    // 模型与定价 names the provider format too.
    assert((await page.locator('.mapping-editor').getByLabel('供应商',{exact:true}).locator('option').allTextContents()).includes('OpenAI 格式 / Fixture · OpenAI'));
    // No enabled Key may call the model: the server refuses the test, in words.
    key('fixture-openai-key-1').enabled=false;await refresh();
    const astra=page.getByRole('row').filter({has:page.getByRole('button',{name:'gpt-6-astra 的更多操作',exact:true})});
    await astra.getByRole('button',{name:'测试',exact:true}).click();
    await astra.getByRole('status').filter({hasText:'测试没有完成：这个供应商没有启用的 Key 能调用这个模型'}).waitFor();
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 测试 in a Key’s model list (through that Key) and on a model row (through its route): success with first-token time, failures with the error, nothing saved');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
