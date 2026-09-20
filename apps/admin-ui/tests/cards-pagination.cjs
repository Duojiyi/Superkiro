// A membership change between offset pages must never look like a complete card list.
const assert=require('node:assert/strict'),fs=require('node:fs'),path=require('node:path'),vm=require('node:vm'),ts=require('typescript');
const source=ts.transpileModule(fs.readFileSync(path.join(__dirname,'../src/api.ts'),'utf8'),{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
const full=Array.from({length:500},(_,i)=>({id:`card-${i}`}));
async function read(pages){
  const exports={},urls=[];
  vm.runInNewContext(source,{exports,Headers,AbortController,setTimeout,clearTimeout,fetch:async url=>{
    if(url.endsWith('/session'))return {ok:true,json:async()=>({success:true,role:'admin',csrfToken:'fixture'})};
    urls.push(url);assert(pages.length,'unexpected extra page');return {ok:true,json:async()=>pages.shift()};
  }});
  const api=new exports.AdminApiClient();await api.checkAuth();
  try{return {result:await api.getCards(),urls};}finally{api.clearSession();}
}
(async()=>{
  const page=(cards,revision='members-1')=>({success:true,count:cards.length,cards,revision});
  const stable=await read([page(full),page([{id:'last'}])]);
  assert.equal(stable.result.count,501);assert.equal(stable.urls.length,2);assert(stable.urls[1].includes('offset=500'));
  assert.equal((await read([page(full),page([])])).result.count,500);
  await assert.rejects(read([page(full),page([{id:'last'}],'members-2')]),/读取期间发生变化/);
  await assert.rejects(read([page(full),page([{id:'card-0'}])]),/重复/);
  await assert.rejects(read([page(full),{success:true,cards:[]}]),/未提供卡密分页版本/);
  await assert.rejects(read([{success:true,cards:full}]),/未提供卡密分页版本/);
  await assert.rejects(read([{success:true,cards:[]}]),/未提供卡密分页版本/);
  assert.equal((await read([page([])])).result.count,0);
  console.log('PASS: stable pagination, exact-page boundary, membership changes and duplicates fail closed');
})().catch(error=>{console.error(error);process.exitCode=1;});
