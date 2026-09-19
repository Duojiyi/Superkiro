// Test-only authenticated API. Never imported by the application or served by production.
const assert = require('node:assert/strict');
module.exports = function fixtureApi() {
  const now = 1789790400;
  const writes = [];
  const groups = ['PRO', 'PRO+', 'PRO Max', 'Power'].map((name, i) => ({id: `fixture-group-${i}`, name, virtual_plan_name: name, virtual_usage_limit: [1000,2000,5000,10000][i], rate_card_id: 'fixture-rate', margin_multiplier: 1}));
  const models = ['claude-sonnet', 'gpt-5', 'gemini-pro'].map((name, i) => ({id: `fixture-model-${i}`, exposed_model_id: name, target_provider_id: 'fixture-provider', target_model: name, group_id: groups[i].id, context_window: 200000, max_output: 8192, credit_multiplier: 1, visible: true, supports_tools: true, supports_vision: true, supports_reasoning: true}));
  const cards = ['active', 'unactivated', 'frozen', 'banned', 'expired', 'active'].map((status, i) => ({id: `fixture-card-${i}`, status, creditTotal: 2000000000, creditUsed: i*100000000, availableCredits: 2000000000-i*100000000, pointsTotal: 2000, pointsAvailable: 2000-i*100, boundDevices: status === 'unactivated' ? [] : [`fixture-device-${i}`], maxDevices: 1, activatedAt: now-86400, validUntil: now+2592000, groupId: groups[i%4].id, note: '本地视觉测试数据'}));
  const traces = Array.from({length: 24}, (_, i) => ({id: `fixture-trace-${i}`, card_id: cards[i%6].id, ts: now-i*3600, exposed_model: models[i%3].exposed_model_id, status: i<18 ? 'success' : i<21 ? 'error' : i<23 ? 'client_aborted' : 'in_progress', ttft_ms: 240+i*31, input_tokens: 1200+i*25, output_tokens: 480, tokens_per_second: 42, credits_charged: i<18 ? 1500000 : 0, attempt_chain: [{provider_id: 'fixture-provider', key_id: 'fixture-key', success: i<18, error: i<18 ? null : 'fixture upstream timeout', latency_ms: 580}]}));
  const config = {revision:'fixture-rev-2', groups, models, rate_cards:[{id:'fixture-rate', name:'测试价格表'}], versions: models.map((m,i) => ({id:`fixture-price-${i}`, model:m.exposed_model_id, rate_card_id:'fixture-rate', pricing_mode:'fixed', effective_from_secs:now-86400, fixed_input_credit_per_m:3000000, fixed_output_credit_per_m:15000000, fixed_cache_read_credit_per_m:300000, fixed_cache_creation_credit_per_m:3750000})), audit:[{operator:'fixture-admin', reason:'本地测试：更新模型价格', previous_revision:'fixture-rev-1', revision:'fixture-rev-2', created_at_secs:now}]};
  let authenticated = false;
  return {writes, async handle(req,res) {
    const url = new URL(req.url, 'http://127.0.0.1');
    const endpoint = url.pathname.replace('/api/v1/admin/', '');
    const reply = (value, status=200) => {res.writeHead(status, {'Content-Type':'application/json'}); res.end(JSON.stringify(value));};
    if (endpoint==='session' && req.method==='POST') {
      assert.equal(req.headers['x-admin-key'],'fixture-admin-key'); authenticated=true;
      return reply({accessToken:'fixture-session',tokenType:'Bearer',expiresIn:3600,expiresAt:now+3600});
    }
    if (!authenticated || req.headers.authorization!=='Bearer fixture-session') return reply({error:'Fixture authentication required'},401);
    let body={};
    if(req.method==='POST') {let raw=''; for await(const chunk of req) raw+=chunk; body=JSON.parse(raw||'{}'); writes.push({endpoint,body});}
    if(endpoint==='me') return reply({success:true,role:'admin'});
    if(endpoint==='stats') return reply({success:true,totalCards:cards.length,activeCards:2,unactivatedCards:1,frozenCards:1,bannedCards:1,totalCredits:12000000000,usedCredits:1500000000,remainingCredits:10500000000,totalPoints:12000,usedPoints:1500,remainingPoints:10500});
    if(endpoint==='cards') return reply({success:true,count:cards.length,cards});
    if(endpoint==='traces') return reply({success:true,traces});
    if(endpoint==='commercial-config') return reply({success:true,config});
    if(endpoint==='providers') return reply({success:true,providers:[{id:'fixture-provider',name:'测试供应商 / Fixture',base_url:'https://fixture.invalid/v1',enabled:true}],keys:[{id:'fixture-key',provider_id:'fixture-provider',allowed_models:models.map(m=>m.target_model),weight:10,enabled:true,health_state:'healthy'},{id:'fixture-backup',provider_id:'fixture-provider',allowed_models:['claude-sonnet'],weight:1,enabled:true,health_state:'cooldown',cooldown_until:4102444800}]});
    if(endpoint==='financials') return reply({success:true,dashboard:{total_requests:18,total_credits_charged:27000000,revenue_micro_cny:0,provider_cost_micro_cny:4200000,gross_profit_micro_cny:0,gross_margin_percentage:0},modelRankings:models.map(m=>({model_id:m.exposed_model_id,provider_cost_micro_cny:1400000}))});
    if(endpoint==='announcements') return reply({success:true,announcements:[{id:'fixture-notice',title:'本地测试：服务维护通知',content:'这是隔离的视觉测试公告，不会向真实用户发布。',level:'info',enabled:true,created_at:now}]});
    if(endpoint==='cards/batch') {
      assert.equal(body.maxDevices,1); assert.equal('creditTotal' in body,false); assert.ok(groups.some(g=>g.id===body.groupId));
      const points=Number(body.templateId.replace('tier-','')); assert.ok([1000,2000,5000,10000].includes(points));
      const generated=Array.from({length:body.count},(_,i)=>({cardId:`fixture-issued-${cards.length+i}`,rawCode:`FIXTURE-NOT-VALID-${points}-${i}`,groupId:body.groupId,creditTotal:points*1000000,status:'unactivated'}));
      generated.forEach(c=>cards.push({id:c.cardId,status:c.status,creditTotal:c.creditTotal,creditUsed:0,availableCredits:c.creditTotal,pointsTotal:points,pointsAvailable:points,boundDevices:[],maxDevices:1,groupId:c.groupId}));
      return reply({success:true,cards:generated});
    }
    if(endpoint==='cards/status') {const card=cards.find(c=>c.id===body.cardId); assert.ok(card); card.status=body.action==='freeze'?'frozen':body.action==='unfreeze'?'active':'banned'; return reply({success:true,cardId:card.id,newStatus:card.status});}
    if(endpoint==='exports/ledger.csv') {res.writeHead(200,{'Content-Type':'text/csv'}); return res.end('id,points\nfixture-ledger,27\n');}
    throw new Error(`Unhandled fixture endpoint: ${req.method} ${endpoint}`);
  }};
};
