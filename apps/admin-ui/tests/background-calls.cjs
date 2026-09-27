// Kiro's hidden background calls (simple-task): which model each group's go to and why, on 分组与权益
// and beside 模型与定价's list, and choosing one in the model editor. Final build + loopback fixture,
// never production; the fixture answers the stats as the server does.
const {chromium}=require(process.env.PLAYWRIGHT_MODULE||'playwright');
const recordToasts=require('./toasts.cjs');
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
const published=()=>fixture.writes.filter(write=>write.endpoint==='commercial-config');
(async()=>{
  let browser;
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    browser=await chromium.launch({headless:true,...(process.env.CHROME_PATH?{executablePath:process.env.CHROME_PATH}:{})});
    const page=await browser.newPage({viewport:{width:1440,height:1000}}),errors=[];
    page.setDefaultTimeout(10000);
    const toasts=await recordToasts(page);
    const origin=`http://127.0.0.1:${server.address().port}`;
    page.on('pageerror',error=>{errors.push(error.message);console.error('Browser error:',error.message);});
    const nativeDialogs=[];page.on('dialog',dialog=>{nativeDialogs.push(dialog.message());void dialog.dismiss();});
    await page.route('**/*',route=>new URL(route.request().url()).origin===origin?route.continue():route.abort());
    const button=name=>page.getByRole('button',{name,exact:true});
    const nav=name=>page.getByRole('navigation').getByRole('button',{name,exact:true}).click();
    const dialog=page.getByRole('alertdialog');
    await page.goto(origin+'/admin/');await page.getByLabel('密码',{exact:true}).fill('fixture-password');await button('登录').click();
    await button('刷新').waitFor();

    // 分组与权益: each group's background model and why; a group with nothing to serve them says so.
    await nav('分组与权益');
    const groupRow=name=>page.getByRole('row').filter({has:page.getByRole('cell',{name,exact:true})});
    await groupRow('PRO').getByText('gpt-6-astra',{exact:true}).waitFor();
    const pro=await groupRow('PRO').innerText();
    assert(pro.includes('分组里最便宜的在售模型'),pro);
    assert((await groupRow('PRO+').innerText()).includes('gpt-5'));
    const power=await groupRow('Power').innerText();
    assert(power.includes('没有可用的模型')&&power.includes('后台调用会失败'),power);
    await page.getByRole('columnheader',{name:/Kiro 后台调用/}).waitFor();
    // 去模型与定价 shows that model there.
    await groupRow('PRO').getByRole('button',{name:'去模型与定价',exact:true}).click();
    await page.locator('tr[data-model="gpt-6-astra"].is-marked').waitFor();
    console.log('PASS: 分组与权益 names each group\'s background-call model and why (cheapest, or none at all), and links to it on 模型与定价');

    // 模型与定价: the same beside the list, the model tagged; choosing another in the editor.
    const note=page.getByRole('group',{name:'Kiro 后台调用'});
    await note.getByText('PRO → gpt-6-astra（最便宜）').waitFor();
    assert((await page.locator('tr[data-model="gpt-6-astra"]').innerText()).includes('后台调用'));
    assert(!(await page.locator('tr[data-model="claude-sonnet"]').innerText()).includes('后台调用'));
    await page.locator('tr[data-model="claude-sonnet"]').getByRole('button',{name:'编辑',exact:true}).click();
    const toggle=page.getByRole('checkbox',{name:'Kiro 后台调用用这个模型',exact:true});
    await page.locator('.fast-alias').getByText('现在：gpt-6-astra（分组里最便宜的在售模型）').waitFor();
    await toggle.check();
    await page.locator('.fast-alias').getByText('PRO 的后台调用由它处理，每次按它的价格扣费').waitFor();
    assert.equal(await page.getByLabel('别名',{exact:true}).inputValue(),'simple-task','the alias is what does it');
    await note.getByText('PRO → claude-sonnet（别名）').waitFor();
    await note.getByText('发布后',{exact:true}).waitFor();
    await page.locator('tr[data-model="claude-sonnet"]').getByText('后台调用',{exact:true}).waitFor();
    const bar=page.getByRole('region',{name:'发布'});
    await bar.getByLabel('变更原因',{exact:true}).fill('后台调用改用 claude-sonnet');await bar.getByRole('button',{name:'发布',exact:true}).click();
    await dialog.waitFor();const facts=await dialog.innerText();
    for(const expected of ['新增别名 simple-task','PRO 的 Kiro 后台调用：gpt-6-astra → claude-sonnet，每次按它的价格扣费'])assert(facts.includes(expected),`${expected}\n${facts}`);
    await dialog.locator('[data-confirm="accept"]').click();await toasts.shown('已发布');
    assert.deepEqual(published().at(-1).body.models.map(model=>[model.id,model.aliases]),[['fixture-model-0',['simple-task']]]);
    // The server's answer, read again after the publication.
    await note.getByText('PRO → claude-sonnet（别名）').waitFor();
    assert.equal(await note.getByText('发布后',{exact:true}).count(),0);
    await nav('分组与权益');
    await groupRow('PRO').getByText('设了别名 simple-task').waitFor();
    assert.deepEqual(errors,[]);assert.deepEqual(nativeDialogs,[],'no browser-native dialogs');
    console.log('PASS: 模型与定价 shows each group\'s background model beside the list and on its row; ticking it in the editor sets the alias simple-task, the confirmation says what it moves, and the server\'s answer follows');
  }finally{
    await browser?.close();server.close();
  }
})().catch(error=>{console.error(error);process.exit(1);});
