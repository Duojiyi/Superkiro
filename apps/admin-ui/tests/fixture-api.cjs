// Test-only authenticated API. Never imported by the application or served by production.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
module.exports = function fixtureApi() {
  const now = 1789790400;
  const writes = [];
  const groups = ['PRO', 'PRO+', 'PRO Max', 'Power'].map((name, i) => ({id: `fixture-group-${i}`, name, virtual_plan_name: name, virtual_usage_limit: [1000,2000,5000,10000][i], rate_card_id: 'fixture-rate', margin_multiplier: 1}));
  const models = ['claude-sonnet', 'gpt-5', 'gemini-pro'].map((name, i) => ({id: `fixture-model-${i}`, exposed_model_id: name, target_provider_id: 'fixture-provider', target_model: name, group_id: groups[i].id, context_window: 200000, max_output: 8192, credit_multiplier: 1, visible: true, supports_tools: true, supports_vision: true, supports_reasoning: true}));
  const cards = ['active', 'unactivated', 'frozen', 'banned', 'expired', 'active'].map((status, i) => ({id: `fixture-card-${i}`, codeRecoverable: i !== 1, status, creditTotal: 2000000000, creditUsed: i*100000000, availableCredits: 2000000000-i*100000000, pointsTotal: 2000, pointsAvailable: 2000-i*100, boundDevices: status === 'unactivated' ? [] : [`fixture-device-${i}`], maxDevices: 1, activatedAt: now-86400, validUntil: now+2592000, groupId: groups[i%4].id, note: '本地视觉测试数据'}));
  const cardRevision = () => crypto.createHash('sha256').update(cards.map(card => card.id).join('\n')).digest('hex');
  // Requests relative to the real clock, so "近 24 小时" and the 24-hour content archive
  // behave as in production: one every 30 minutes, the newest a minute ago.
  const realNow = Math.floor(Date.now() / 1000);
  const errorClasses = ['upstream_start_failed', 'stream_incomplete', 'empty_completion'];
  const traces = Array.from({length: 64}, (_, i) => {
    const status = i >= 18 && i <= 20 ? 'error' : i >= 21 && i <= 22 ? 'client_aborted' : i === 23 ? 'in_progress' : 'success';
    const card = cards[i % 6];
    const ok = status === 'success';
    return {id: `fixture-trace-${i}`, card_id: card.id, ts: realNow - 60 - i * 1800, invocation_id: `${card.id}:fixture-inv-${i}`,
      exposed_model: models[i % 3].exposed_model_id, status, ttft_ms: ok ? 240 + i * 31 : null, tokens_per_second: ok ? 30 + (i % 20) : null,
      error_class: status === 'error' ? errorClasses[i - 18] : null, provider_id: 'fixture-provider',
      input_tokens: ok ? 1200 + i * 25 : 0, output_tokens: ok ? 480 : 0, credits_charged: ok ? 1500000 : 0, provider_cost_micro_cny: ok ? 21000 : 0,
      attempt_chain: status === 'error'
        ? [{provider_id: 'fixture-provider', key_id: 'fixture-key', success: false, error: 'HTTP 529 overloaded_error: Overloaded', latency_ms: 1840}, {provider_id: 'fixture-provider', key_id: 'fixture-backup', success: false, error: 'fixture upstream timeout', latency_ms: 580}]
        : [{provider_id: 'fixture-provider', key_id: 'fixture-key', success: status !== 'client_aborted', error: status === 'client_aborted' ? 'client closed the stream' : null, latency_ms: 580 + i * 10}]};
  });
  // What GET /stats reports as `activity`, computed from those traces at the time of the request.
  const activity = () => {
    const now = Math.floor(Date.now() / 1000), done = traces.filter(t => t.status !== 'in_progress');
    const window = start => {
      const inWindow = done.filter(t => t.ts > start), timed = inWindow.map(t => t.ttft_ms).filter(v => typeof v === 'number').sort((a, b) => a - b);
      return {requests: inWindow.length, succeeded: inWindow.filter(t => t.status === 'success').length, failed: inWindow.filter(t => t.status === 'error').length,
        clientAborted: inWindow.filter(t => t.status === 'client_aborted').length, creditsCharged: inWindow.reduce((sum, t) => sum + t.credits_charged, 0),
        inputTokens: inWindow.reduce((sum, t) => sum + t.input_tokens, 0), outputTokens: inWindow.reduce((sum, t) => sum + t.output_tokens, 0),
        providerCostMicroCny: inWindow.reduce((sum, t) => sum + t.provider_cost_micro_cny, 0), activeCards: new Set(inWindow.filter(t => t.credits_charged > 0).map(t => t.card_id)).size,
        timedRequests: timed.length, ttftMedianMs: timed.length ? timed[Math.floor((timed.length - 1) / 2)] : null, ttftP90Ms: timed.length ? timed[Math.ceil(timed.length * 0.9) - 1] : null};
    };
    const first = (Math.floor(now / 3600) - 23) * 3600;
    const last24h = window(now - 86400);
    return {last24h, last7d: window(now - 7 * 86400), tracesCoverFromSecs: Math.min(...traces.map(t => t.ts)),
      hourly: Array.from({length: 24}, (_, i) => {const hour = done.filter(t => t.ts >= first + i * 3600 && t.ts < first + (i + 1) * 3600); return {startSecs: first + i * 3600, requests: hour.length, failed: hour.filter(t => t.status === 'error').length};}),
      providers: [{providerId: 'fixture-provider', requests: last24h.requests, failed: last24h.failed, ttftMedianMs: last24h.ttftMedianMs}]};
  };
  // Request content is kept for some requests only; the rest answer like an expired archive.
  const traceContent = invocationId => {
    const trace = traces.find(t => t.invocation_id === invocationId);
    const index = trace ? Number(trace.id.split('-').pop()) : -1;
    if (!trace || index % 3 === 2 || Math.floor(Date.now() / 1000) - trace.ts > 86400) return null;
    const tools = [{toolSpecification: {name: 'readFile', description: 'Read a file from the workspace', inputSchema: {json: {type: 'object', properties: {path: {type: 'string'}}}}}},
      {toolSpecification: {name: 'fsWrite', description: 'Write a file', inputSchema: {json: {type: 'object'}}}}];
    return {success: true, invocationId, cardId: trace.card_id, model: trace.exposed_model, receivedAt: trace.ts, expiresAt: trace.ts + 86400,
      request: {conversationState: {conversationId: `fixture-conversation-${index}`, history: [
        {userInputMessage: {content: '帮我看看 src/App.tsx 里的登录逻辑', userInputMessageContext: {tools}}},
        {assistantResponseMessage: {content: '我先读取这个文件。', toolUses: [{name: 'readFile', toolUseId: `tooluse-${index}-1`, input: {path: 'src/App.tsx'}}]}},
        {userInputMessage: {content: '', userInputMessageContext: {toolResults: [{toolUseId: `tooluse-${index}-1`, status: 'success', content: [{text: 'export default function App() {\n  return <Login />;\n}'}]}]}}},
        {assistantResponseMessage: {content: '登录逻辑在 login() 函数里：先建立会话，再检查 CSRF 令牌。'}},
      ], currentMessage: {userInputMessage: {content: `那退出登录呢？（本地测试请求 #${index}）`, images: [{format: 'png', source: {bytes: '[图片已省略：120 KB]'}}], userInputMessageContext: {tools}}}}},
      notes: {omittedImages: 1, omittedHistoryEntries: index === 0 ? 12 : 0, truncated: false},
      reply: trace.status === 'in_progress' ? null : {status: trace.status, error: trace.status === 'error' ? 'fixture upstream timeout' : null, providerId: 'fixture-provider', targetModel: trace.exposed_model,
        stopReason: trace.status === 'success' ? 'end_turn' : null, text: trace.status === 'success' ? `退出登录在 logout() 中：\n1. 清除本地会话\n2. 通知服务器撤销会话（本地测试回复 #${index}）` : '',
        reasoning: trace.status === 'success' ? '用户想了解退出登录的实现，先定位 logout 函数。' : '', toolCalls: trace.status === 'success' && index % 2 === 0 ? [{id: `call-${index}`, name: 'readFile', arguments: '{"path":"src/api.ts"}'}] : [],
        inputTokens: trace.input_tokens, outputTokens: trace.output_tokens, cacheReadTokens: 800, cacheWriteTokens: 0, ttftMs: trace.ttft_ms, tokensPerSecond: trace.tokens_per_second, truncated: false}};
  };
  const config = {settings:{credit_face_value_cny:0.01,usd_cny_rate:7.2,rate_updated_at_secs:now},revision:'fixture-rev-2', groups, models, rate_cards:[{id:'fixture-rate', name:'测试价格表'}], versions: models.map((m,i) => ({id:`fixture-price-${i}`, model:m.exposed_model_id, rate_card_id:'fixture-rate', pricing_mode:'fixed', effective_from_secs:now-86400, fixed_input_credit_per_m:3000000, fixed_output_credit_per_m:15000000, fixed_cache_read_credit_per_m:300000, fixed_cache_creation_credit_per_m:3750000})), audit:[{operator:'fixture-admin', reason:'本地测试：更新模型价格', previous_revision:'fixture-rev-1', revision:'fixture-rev-2', created_at_secs:now}]};
  let authenticated = false;
  return {writes, traces, expire() {authenticated=false;}, async handle(req,res) {
    const url = new URL(req.url, 'http://127.0.0.1');
    const endpoint = url.pathname.replace('/api/v1/admin/', '');
    const reply = (value, status=200) => {res.writeHead(status, {'Content-Type':'application/json'}); res.end(JSON.stringify(value));};
    if (endpoint==='session' && req.method==='POST') {
      let raw=''; for await(const chunk of req) raw+=chunk; assert.deepEqual(JSON.parse(raw),{username:'admin',password:'fixture-password'}); authenticated=true; res.setHeader('Set-Cookie','fixture_session=valid; HttpOnly; SameSite=Strict; Path=/');
      return reply({success:true,expiresIn:900});
    }
    if (!authenticated || !req.headers.cookie?.includes('fixture_session=valid')) return reply({error:'Fixture authentication required'},401);
    let body={};
    if(req.method==='POST') {assert.equal(req.headers['x-csrf-token'],'fixture-csrf'); let raw=''; for await(const chunk of req) raw+=chunk; body=JSON.parse(raw||'{}'); writes.push({endpoint,body});}
    if(endpoint==='me' || (endpoint==='session' && req.method==='GET')) return reply({success:true,role:'admin',username:'admin',csrfToken:'fixture-csrf',expiresAt:Math.floor(Date.now()/1000)+900,twoFactorEnabled:false,totpRequired:false});
    if(endpoint==='cards/reveal') return reply({success:true,rawCode:'FIXTURE-RECOVERED-CODE'});
    if(endpoint==='session/revoke') {authenticated=false; res.setHeader('Set-Cookie','fixture_session=; Max-Age=0; Path=/'); return reply({success:true});}
    if(endpoint==='stats') return reply({success:true,totalCards:cards.length,activeCards:2,unactivatedCards:1,frozenCards:1,bannedCards:1,totalCredits:12000000000,usedCredits:1500000000,remainingCredits:10500000000,totalPoints:12000,usedPoints:1500,remainingPoints:10500,activity:activity()});
    if(endpoint==='cards') return reply({success:true,count:cards.length,cards,revision:cardRevision()});
    if(endpoint==='traces') {const cardId=url.searchParams.get('card_id'),limit=Number(url.searchParams.get('limit')||100);return reply({success:true,traces:traces.filter(t=>!cardId||t.card_id===cardId).slice(0,limit)});}
    if(endpoint==='traces/content') {const record=traceContent(url.searchParams.get('invocation_id'));return record?reply(record):reply({__type:'ResourceNotFoundException',message:'没有这次请求的内容（只保留 24 小时）'},404);}
    if(endpoint==='commercial-config') {
      if(req.method==='POST') {assert.deepEqual(Object.keys(body).sort(),['expected_revision','reason','settings']);assert.equal(body.expected_revision,config.revision);assert.deepEqual(Object.keys(body.settings).sort(),['credit_face_value_cny','usd_cny_rate']);config.settings={...body.settings,rate_updated_at_secs:now+1};config.revision='fixture-rev-3';}
      return reply({success:true,config});
    }
    if(endpoint==='providers') return reply({success:true,providers:[{id:'fixture-provider',name:'测试供应商 / Fixture',base_url:'https://fixture.invalid/v1',enabled:true}],keys:[{id:'fixture-key',provider_id:'fixture-provider',allowed_models:models.map(m=>m.target_model),weight:10,enabled:true,health_state:'healthy'},{id:'fixture-backup',provider_id:'fixture-provider',allowed_models:['claude-sonnet'],weight:1,enabled:true,health_state:'cooldown',cooldown_until:4102444800}]});
    if(endpoint==='financials') return reply({success:true,basis:'retained_usage_ledger_estimate_not_cash_revenue',settings:config.settings,actualRevenueMicroCny:null,actualGrossProfitMicroCny:null,estimates:{retainedLedgerOnly:true,usageFaceValueMicroCny:1000000,configuredProviderCostMicroCny:420000,faceValueLessCostMicroCny:null,faceValueMarginPercentage:null,costedRequests:17,uncostedRequests:1},dashboard:{total_requests:18,total_credits_charged:27000000,revenue_micro_cny:0,provider_cost_micro_cny:4200000,gross_profit_micro_cny:0,gross_margin_percentage:0},modelRankings:models.map(m=>({model_id:m.exposed_model_id,provider_cost_micro_cny:1400000}))});
    if(endpoint==='announcements') return reply({success:true,announcements:[{id:'fixture-notice',title:'本地测试：服务维护通知',content:'这是隔离的视觉测试公告，不会向真实用户发布。',level:'info',enabled:true,created_at:now}]});
    if(endpoint==='cards/batch') {
      assert.equal(body.maxDevices,1); assert.equal('creditTotal' in body,false); assert.ok(groups.some(g=>g.id===body.groupId));
      const points=Number(body.templateId.replace('tier-','')); assert.ok([1000,2000,5000,10000].includes(points));
      const generated=Array.from({length:body.count},(_,i)=>({cardId:`fixture-issued-${cards.length+i}`,rawCode:`FIXTURE-NOT-VALID-${points}-${i}`,groupId:body.groupId,creditTotal:points*1000000,status:'unactivated'}));
      generated.forEach(c=>cards.push({id:c.cardId,status:c.status,creditTotal:c.creditTotal,creditUsed:0,availableCredits:c.creditTotal,pointsTotal:points,pointsAvailable:points,boundDevices:[],maxDevices:1,groupId:c.groupId,note:body.note}));
      return reply({success:true,cards:generated});
    }
    if(endpoint==='cards/status') {const card=cards.find(c=>c.id===body.cardId); assert.ok(card); card.status=body.action==='freeze'?'frozen':body.action==='unfreeze'?'active':'banned'; return reply({success:true,cardId:card.id,newStatus:card.status});}
    if(endpoint==='exports/ledger.csv') {res.writeHead(200,{'Content-Type':'text/csv'}); return res.end('id,points\nfixture-ledger,27\n');}
    throw new Error(`Unhandled fixture endpoint: ${req.method} ${endpoint}`);
  }};
};