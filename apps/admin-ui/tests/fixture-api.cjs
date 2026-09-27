// Test-only authenticated API. Never imported by the application or served by production.
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
module.exports = function fixtureApi() {
  const now = 1789790400;
  const writes = [];
  const groups = ['PRO', 'PRO+', 'PRO Max', 'Power'].map((name, i) => ({id: `fixture-group-${i}`, name, virtual_plan_name: name, virtual_usage_limit: [1000,2000,5000,10000][i], rate_card_id: 'fixture-rate', margin_multiplier: 1}));
  // Every field the server returns for a model entry, so drafts and publications look like production's.
  const entry = fields => ({sort_order: 0, aliases: [], fallback_chain: [], display_name: null, description: null, rate_multiplier: null, retired: false, ...fields});
  const models = ['claude-sonnet', 'gpt-5', 'gemini-pro'].map((name, i) => entry({id: `fixture-model-${i}`, exposed_model_id: name, target_provider_id: 'fixture-provider', target_model: name, group_id: groups[i].id, context_window: 200000, max_output: 8192, credit_multiplier: 1, visible: true, supports_tools: true, supports_vision: true, supports_reasoning: true}));
  // An OpenAI-format provider's model, as onboarded in production: 272K context, priced in USD.
  models.push(entry({id: 'fixture-model-3', exposed_model_id: 'gpt-6-astra', target_provider_id: 'fixture-openai', target_model: 'gpt-6-astra', group_id: groups[0].id, context_window: 272000, max_output: 128000, credit_multiplier: 1, visible: true, supports_tools: true, supports_vision: true, supports_reasoning: true, sort_order: 1}));
  // Card dates follow the clock (activated a day ago, 29 days left), so no card runs out as the calendar moves on.
  const realNow = Math.floor(Date.now() / 1000);
  const cards = ['active', 'unactivated', 'frozen', 'banned', 'expired', 'active'].map((status, i) => ({id: `fixture-card-${i}`, codeRecoverable: i !== 1, status, creditTotal: 2000000000, creditUsed: i*100000000, availableCredits: 2000000000-i*100000000, pointsTotal: 2000, pointsAvailable: 2000-i*100, boundDevices: status === 'unactivated' ? [] : [`fixture-device-${i}`], maxDevices: 1, activatedAt: realNow-86400, validUntil: realNow+2592000-86400, groupId: groups[i%4].id, note: '本地视觉测试数据', rebindsUsed: 0, maxRebinds: 5}));
  // A card as the server's card views show it: its status as the customer meets it (an active card
  // past its date is expired), its rebind allowance, and for a card not yet activated its validity.
  const view = card => {
    const t = Math.floor(Date.now() / 1000);
    return {...card, effectiveStatus: card.status === 'active' && card.validUntil != null && t >= card.validUntil ? 'expired' : card.status,
      rebindsUsed: card.rebindsUsed ?? 0, maxRebinds: card.maxRebinds ?? 5, rebindCooldownUntil: card.rebindCooldownUntil > t ? card.rebindCooldownUntil : null,
      activationDurationSecs: card.activatedAt == null ? card.activationDurationSecs ?? 2592000 : null};
  };
  // What the support actions and adjustments wrote to each card's history, oldest first.
  const log = [];
  // The saved billing state's size, as GET /stats reports it (billing's warning level and ceiling),
  // and how far back the ledger has been archived.
  const storage = {bytes: 13212876, warning: 33554432, ceiling: 268435456, archivedBefore: 0};
  const cardRevision = () => crypto.createHash('sha256').update(cards.map(card => card.id).join('\n')).digest('hex');
  // Requests relative to the real clock, so "近 24 小时" and the 24-hour content archive
  // behave as in production: one every 30 minutes, the newest a minute ago.
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
      providers: [{providerId: 'fixture-provider', requests: last24h.requests, failed: last24h.failed, ttftMedianMs: last24h.ttftMedianMs}], ...attemptActivity(now)};
  };
  // As billing's attempt_activity: every attempt by provider and Key over the last hour, 24 hours and
  // 7 days (a failed one is taken over when another provider answered later), requests by the model
  // customers asked for (not the card's own refusals), and billed use by model over 7 days.
  const CARD_LIMITS = ['insufficient_balance', 'concurrency_limit', 'usage_limit'];
  const attemptActivity = now => {
    const starts = [now - 3600, now - 86400, now - 7 * 86400];
    const attemptWindow = () => ({attempts: 0, failures: 0, takenOver: 0, failuresByKind: {}});
    const healthWindow = () => ({requests: 0, failures: 0, lastFailureAt: null, topFailureKind: null, failuresByKind: {}});
    const providers = new Map(), keysById = new Map(), models = new Map();
    const count = (entry, within, attempt, takenOver) => ['last1h', 'last24h', 'last7d'].forEach((name, i) => {
      if (!within[i]) return;
      const w = entry[name]; w.attempts++;
      if (!attempt.success) {w.failures++; w.takenOver += takenOver ? 1 : 0; const kind = attempt.error || 'unknown'; w.failuresByKind[kind] = (w.failuresByKind[kind] || 0) + 1;}
    });
    for (const trace of traces.filter(t => t.ts > starts[2] && t.ts <= now)) {
      const within = starts.map(start => trace.ts > start), chain = trace.attempt_chain || [];
      chain.forEach((attempt, index) => {
        const takenOver = chain.slice(index + 1).some(later => later.success && later.provider_id !== attempt.provider_id);
        if (!providers.has(attempt.provider_id)) providers.set(attempt.provider_id, {providerId: attempt.provider_id, last1h: attemptWindow(), last24h: attemptWindow(), last7d: attemptWindow()});
        count(providers.get(attempt.provider_id), within, attempt, takenOver);
        if (!attempt.key_id) return;
        if (!keysById.has(attempt.key_id)) keysById.set(attempt.key_id, {keyId: attempt.key_id, providerId: attempt.provider_id, last1h: attemptWindow(), last24h: attemptWindow(), last7d: attemptWindow()});
        count(keysById.get(attempt.key_id), within, attempt, takenOver);
      });
      if (trace.status === 'in_progress' || !trace.exposed_model || CARD_LIMITS.includes(trace.error_class)) continue;
      const failure = trace.status === 'error' ? trace.error_class || [...chain].reverse().find(attempt => !attempt.success)?.error || 'unknown' : null;
      if (!models.has(trace.exposed_model)) models.set(trace.exposed_model, {model: trace.exposed_model, last1h: healthWindow(), last24h: healthWindow(), last7d: healthWindow()});
      ['last1h', 'last24h', 'last7d'].forEach((name, i) => {
        if (!within[i]) return;
        const w = models.get(trace.exposed_model)[name]; w.requests++;
        if (!failure) return;
        w.failures++; w.lastFailureAt = Math.max(w.lastFailureAt ?? 0, trace.ts); w.failuresByKind[failure] = (w.failuresByKind[failure] || 0) + 1;
        w.topFailureKind = Object.entries(w.failuresByKind).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))[0][0];
      });
    }
    const usage = new Map();
    for (const trace of traces.filter(t => t.credits_charged > 0 && t.ts > starts[2] && t.ts <= now)) {
      const entry = usage.get(trace.exposed_model) ?? {model: trace.exposed_model, requests: 0, cards: new Set()};
      entry.requests++; entry.cards.add(trace.card_id); usage.set(trace.exposed_model, entry);
    }
    const busiest = (a, b, key) => b.last7d[key] - a.last7d[key];
    return {providerAttempts: [...providers.values()].sort((a, b) => busiest(a, b, 'attempts') || a.providerId.localeCompare(b.providerId)),
      keyAttempts: [...keysById.values()].sort((a, b) => busiest(a, b, 'attempts') || a.keyId.localeCompare(b.keyId)),
      modelHealth: [...models.values()].sort((a, b) => busiest(a, b, 'requests') || a.model.localeCompare(b.model)),
      modelUsage7d: [...usage.values()].map(entry => ({model: entry.model, requests: entry.requests, cards: entry.cards.size})).sort((a, b) => b.requests - a.requests || a.model.localeCompare(b.model))};
  };
  // The usage ledger the financials and the CSV export read, relative to the real clock: today, yesterday,
  // ten and forty days ago. One of today's was served by a route without a cost (not priced by a version).
  const DAY = 86400, t0 = Math.floor(Date.now() / 1000);
  const usage = (i, ago, model, provider, credits, cost, extra = {}) => ({id: `fixture-ledger-${i}`, card_id: cards[i % 6].id, ts: t0 - ago, kind: 'usage', invocation_id: `${cards[i % 6].id}:ledger-${i}`,
    exposed_model: model, provider_id: provider, input_tokens: 40000 + i * 1000, output_tokens: 900 + i * 10, cache_read_tokens: 30000, cache_write_tokens: 2000,
    credits_charged: credits, provider_cost_micro_cny: cost, rate_card_version: cost ? 'fixture-price-v1' : null, ...extra});
  const ledger = [
    usage(0, 600, 'claude-sonnet', 'fixture-provider', 1500000, 4000), usage(1, 1200, 'claude-sonnet', 'fixture-provider', 2500000, 7000),
    usage(2, 1800, 'gpt-5', 'fixture-provider', 1000000, 3000), usage(3, 2400, 'gpt-6-astra', 'fixture-openai', 3000000, 40000),
    usage(4, 3000, 'gpt-6-astra', 'fixture-openai', 2000000, 26000), usage(5, 3600, 'gemini-pro', 'fixture-provider', 1200000, 0),
    usage(6, DAY + 3600, 'claude-sonnet', 'fixture-provider', 2000000, 6000), usage(7, DAY + 7200, 'gpt-5', 'fixture-provider', 1800000, 5000),
    usage(8, 10 * DAY, 'claude-sonnet', 'fixture-provider', 4000000, 12000), usage(9, 40 * DAY, 'gpt-5', 'fixture-provider', 6000000, 18000),
  ];
  // The tiers' list prices, the one table the server and the console take them from.
  const PLAN_PRICES = [{templateId: 'tier-1000', name: 'PRO', points: 1000, priceMicroCny: 30000000}, {templateId: 'tier-2000', name: 'PRO+', points: 2000, priceMicroCny: 55000000},
    {templateId: 'tier-5000', name: 'PRO Max', points: 5000, priceMicroCny: 130000000}, {templateId: 'tier-10000', name: 'Power', points: 10000, priceMicroCny: 250000000}];
  // GET /financials over [fromSecs, toSecs), as the server computes each block from the ledger and the cards.
  const financials = (from, to) => {
    const t = Math.floor(Date.now() / 1000), face = config.settings.credit_face_value_cny, within = ts => (from === undefined || ts >= from) && (to === undefined || ts < to);
    const entries = ledger.filter(entry => within(entry.ts)), revenue = entry => Math.round(entry.credits_charged * face);
    const total = key => entries.reduce((sum, entry) => sum + entry[key], 0);
    const credits = total('credits_charged'), cost = total('provider_cost_micro_cny'), income = entries.reduce((sum, entry) => sum + revenue(entry), 0);
    const costed = entries.filter(entry => entry.rate_card_version), uncosted = entries.length - costed.length;
    const costedIncome = costed.reduce((sum, entry) => sum + revenue(entry), 0), costedCost = costed.reduce((sum, entry) => sum + entry.provider_cost_micro_cny, 0);
    const group = key => [...entries.reduce((map, entry) => map.set(entry[key], [...(map.get(entry[key]) ?? []), entry]), new Map())];
    const rankings = group('exposed_model').map(([model, rows]) => {
      const value = rows.reduce((sum, entry) => sum + revenue(entry), 0), spent = rows.reduce((sum, entry) => sum + entry.provider_cost_micro_cny, 0);
      return {model_id: model, requests: rows.length, total_tokens: rows.reduce((sum, entry) => sum + entry.input_tokens + entry.output_tokens, 0), provider_cost_micro_cny: spent,
        credits_charged: rows.reduce((sum, entry) => sum + entry.credits_charged, 0), margin_percentage: value > 0 ? (value - spent) / value * 100 : 0};
    }).sort((a, b) => b.provider_cost_micro_cny - a.provider_cost_micro_cny);
    const byProvider = group('provider_id').map(([providerId, rows]) => ({providerId, requests: rows.length,
      uncachedInputTokens: rows.reduce((sum, entry) => sum + entry.input_tokens - entry.cache_read_tokens - entry.cache_write_tokens, 0), outputTokens: rows.reduce((sum, entry) => sum + entry.output_tokens, 0),
      cacheReadTokens: rows.reduce((sum, entry) => sum + entry.cache_read_tokens, 0), cacheWriteTokens: rows.reduce((sum, entry) => sum + entry.cache_write_tokens, 0),
      costMicroCny: rows.reduce((sum, entry) => sum + entry.provider_cost_micro_cny, 0)})).sort((a, b) => b.costMicroCny - a.costMicroCny);
    // Sales: cards issued (a day before they were activated, as their history says) and activated in the period, at their tier's price.
    const planOf = card => PLAN_PRICES.findIndex(plan => plan.points * 1e6 === card.creditTotal);
    const byPlan = PLAN_PRICES.map(plan => ({...plan, issuedCards: 0, activatedCards: 0}));
    const sales = {issuedCards: 0, issuedValueMicroCny: 0, activatedCards: 0, activatedValueMicroCny: 0, unpricedIssuedCards: 0, unpricedActivatedCards: 0, byPlan};
    for (const card of cards) {
      const plan = planOf(card), issued = (card.activatedAt ?? t) - DAY;
      if (within(issued) && !(card.status === 'voided' && card.activatedAt == null)) {sales.issuedCards++; if (plan < 0) sales.unpricedIssuedCards++; else {byPlan[plan].issuedCards++; sales.issuedValueMicroCny += byPlan[plan].priceMicroCny;}}
      if (card.activatedAt != null && within(card.activatedAt)) {sales.activatedCards++; if (plan < 0) sales.unpricedActivatedCards++; else {byPlan[plan].activatedCards++; sales.activatedValueMicroCny += byPlan[plan].priceMicroCny;}}
    }
    // Liability: what customers can still use now, whatever the period.
    const usable = cards.filter(card => card.archivedAt == null && !['expired', 'voided', 'banned'].includes(view(card).effectiveStatus));
    const owed = card => Math.max(0, card.availableCredits ?? card.creditTotal - card.creditUsed);
    const liability = {cards: usable.length, microCredits: usable.reduce((sum, card) => sum + owed(card), 0), valueMicroCny: 0,
      unactivatedCards: usable.filter(card => card.status === 'unactivated').length, unactivatedMicroCredits: usable.filter(card => card.status === 'unactivated').reduce((sum, card) => sum + owed(card), 0)};
    liability.valueMicroCny = Math.round(liability.microCredits * face);
    return {success: true, fromSecs: from ?? null, toSecs: to ?? null, basis: 'retained_usage_ledger_estimate_not_cash_revenue', settings: config.settings, actualRevenueMicroCny: null, actualGrossProfitMicroCny: null,
      estimates: {retainedLedgerOnly: true, usageFaceValueMicroCny: income, configuredProviderCostMicroCny: cost, faceValueLessCostMicroCny: uncosted ? null : income - cost,
        faceValueMarginPercentage: !uncosted && income > 0 ? (income - cost) / income * 100 : null, costedRequests: costed.length, uncostedRequests: uncosted},
      dashboard: {total_requests: entries.length, total_credits_charged: credits, revenue_micro_cny: income, provider_cost_micro_cny: cost, gross_profit_micro_cny: income - cost, gross_margin_percentage: income > 0 ? (income - cost) / income * 100 : 0},
      modelRankings: rankings, byProvider, sales, liability, planPrices: PLAN_PRICES,
      margin: {costedRequests: costed.length, costedCredits: costed.reduce((sum, entry) => sum + entry.credits_charged, 0), revenueMicroCny: costedIncome, costMicroCny: costedCost,
        grossProfitMicroCny: costedIncome - costedCost, marginPercentage: costedIncome > 0 ? (costedIncome - costedCost) / costedIncome * 100 : null,
        uncostedRequests: uncosted, uncostedCredits: entries.filter(entry => !entry.rate_card_version).reduce((sum, entry) => sum + entry.credits_charged, 0)}};
  };
  // The ledger CSV as the server writes it: every text cell quoted, the original columns first.
  const ledgerCsv = () => {
    const quote = text => `"${String(text).replace(/"/g, '""')}"`, micro = value => String(value / 1e6);
    const names = Object.fromEntries(providers.map(provider => [provider.id, provider.name]));
    const header = 'id,card_id,ts,kind,invocation_id,exposed_model,provider_id,input_tokens,output_tokens,credits_charged,provider_cost_micro_cny,time_utc,provider_name,cache_read_tokens,cache_write_tokens,credits,revenue_cny,cost_cny\n';
    return header + ledger.map(entry => [quote(entry.id), quote(entry.card_id), entry.ts, quote(entry.kind), quote(entry.invocation_id), quote(entry.exposed_model), quote(entry.provider_id),
      entry.input_tokens, entry.output_tokens, entry.credits_charged, entry.provider_cost_micro_cny, quote(new Date(entry.ts * 1000).toISOString().replace('.000', '')), quote(names[entry.provider_id] ?? entry.provider_id),
      entry.cache_read_tokens, entry.cache_write_tokens, micro(entry.credits_charged), micro(Math.round(entry.credits_charged * config.settings.credit_face_value_cny)), micro(entry.provider_cost_micro_cny)].join(',') + '\n').join('');
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
  const config = {settings:{credit_face_value_cny:0.01,usd_cny_rate:7.2,rate_updated_at_secs:now},revision:'fixture-rev-2', groups, models, rate_cards:[{id:'fixture-rate', name:'测试价格表'}], versions: models.map((m,i) => i === 3
    ? {id:'fixture-price-astra', model:m.exposed_model_id, rate_card_id:'fixture-rate', pricing_mode:'fixed', effective_from_secs:now-86400, fixed_input_credit_per_m:1250000, fixed_output_credit_per_m:10000000, fixed_cache_read_credit_per_m:125000, fixed_cache_creation_credit_per_m:1250000, per_call_credit:0, margin_multiplier:1, currency:'USD', input_price_per_m:1.25, output_price_per_m:10, cache_read_price_per_m:0.125, cache_creation_price_per_m:1.25}
    : {id:`fixture-price-${i}`, model:m.exposed_model_id, rate_card_id:'fixture-rate', pricing_mode:'fixed', effective_from_secs:now-86400, fixed_input_credit_per_m:3000000, fixed_output_credit_per_m:15000000, fixed_cache_read_credit_per_m:300000, fixed_cache_creation_credit_per_m:3750000}), audit:[{operator:'fixture-admin', reason:'本地测试：更新模型价格', previous_revision:'fixture-rev-1', revision:'fixture-rev-2', created_at_secs:now}]};
  // Providers name their API format as the server does (format: anthropic | open_ai).
  const providers = [{id:'fixture-provider',name:'测试供应商 / Fixture',base_url:'https://fixture.invalid/v1',enabled:true,format:'anthropic'},{id:'fixture-disabled',name:'停用的供应商 / Fixture',base_url:'https://disabled.invalid/v1',enabled:false,format:'anthropic'},{id:'fixture-openai',name:'OpenAI 格式 / Fixture',base_url:'https://openai.invalid/v1',enabled:true,format:'open_ai'}];
  const keys = [{id:'fixture-key',provider_id:'fixture-provider',allowed_models:models.map(m=>m.target_model),weight:10,enabled:true,health_state:'healthy'},{id:'fixture-backup',provider_id:'fixture-provider',allowed_models:['claude-sonnet'],weight:1,enabled:true,health_state:'cooldown',cooldown_until:4102444800,last_error:'http_529',last_error_at:Math.floor(Date.now()/1000)-300},{id:'fixture-disabled-key',provider_id:'fixture-disabled',allowed_models:['gpt-5'],weight:1,enabled:true,health_state:'cooldown',cooldown_until:4102444800},{id:'fixture-openai-key-1',provider_id:'fixture-openai',allowed_models:['gpt-6-astra','gpt-5.6-sol','gpt-5.6-terra'],weight:1,enabled:true,health_state:'healthy'}];
  // The server's rules for a publication (409 in its words when one is broken), applied as it applies them.
  const refuse = message => ({status: 409, body: {success: false, error: `Invalid billing state: ${message}`}});
  const serves = (providerId, model, among = keys) => providers.some(p => p.id === providerId && p.enabled !== false)
    && among.some(k => k.provider_id === providerId && k.enabled !== false && (!Array.isArray(k.allowed_models) || k.allowed_models.includes(model)));
  const publish = body => {
    if (body.expected_revision !== config.revision) return refuse('Configuration changed; reload before publishing');
    if (typeof body.reason !== 'string' || !body.reason.trim()) return refuse('Publication reason required (max 500 bytes)');
    const t = Math.floor(Date.now() / 1000);
    for (const m of body.models || []) for (const id of [m.exposed_model_id, ...(m.aliases || [])]) if (!/^[A-Za-z0-9._:/-]{1,128}$/.test(String(id))) return refuse(`Invalid model ID: ${id}`);
    // Only the entries in this publication are checked: a shown, unretired one needs its primary target to serve.
    const stranded = (body.models || []).filter(m => m.visible !== false && m.retired !== true && !serves(m.target_provider_id, m.target_model)).map(m => m.exposed_model_id);
    if (stranded.length) return refuse(`Visible model target has no enabled compatible key: ${stranded.join(', ')}`);
    const nextModels = [...config.models.filter(row => !(body.models || []).some(next => next.id === row.id)), ...(body.models || [])];
    for (const id of body.removed_models || []) {const m = nextModels.find(row => row.id === id); if (!m || (m.visible !== false && m.retired !== true)) return refuse(`Only hidden or retired mappings can be removed: ${id}`);}
    for (const id of body.cancelled_versions || []) {const v = config.versions.find(row => row.id === id); if (!v || v.effective_from_secs <= t) return refuse(`Only scheduled prices can be cancelled: ${id}`);}
    // 0 is "now", only for a model the rate card has no version of yet; otherwise nothing may start in the past.
    const versions = [];
    for (const v of body.versions || []) {
      const first = !config.versions.some(row => row.rate_card_id === v.rate_card_id && row.model === v.model);
      if (v.effective_from_secs === 0 ? !first : !(v.effective_from_secs >= t)) return refuse('Invalid pricing; retroactive publication forbidden');
      const at = v.effective_from_secs === 0 ? t : v.effective_from_secs;
      if ([...config.versions, ...versions].some(row => row.id === v.id || (row.rate_card_id === v.rate_card_id && row.model === v.model && row.effective_from_secs === at))) return refuse('Published prices immutable; use new ID and timestamp');
      versions.push({...v, effective_from_secs: at});
    }
    if (body.settings) config.settings = {...body.settings, rate_updated_at_secs: now + 1};
    config.versions = [...config.versions.filter(row => !(body.cancelled_versions || []).includes(row.id)), ...versions];
    if (body.groups) config.groups = [...config.groups.filter(row => !body.groups.some(next => next.id === row.id)), ...body.groups];
    config.models = nextModels.filter(row => !(body.removed_models || []).includes(row.id));
    config.revision = `fixture-rev-${Number(config.revision.split('-').pop()) + 1}`;
    return {status: 200, body: {success: true, config}};
  };
  let authenticated = false, deadline = 0, loginAt = 0;
  const IDLE = 1800, MAX = 8 * 3600;
  const nowSecs = () => Math.floor(Date.now() / 1000);
  return {writes, cards, traces, providers, keys, config, storage, ledger, expire() {authenticated=false;}, get sessionDeadline() {return deadline;}, async handle(req,res) {
    const url = new URL(req.url, 'http://127.0.0.1');
    const endpoint = url.pathname.replace('/api/v1/admin/', '');
    const reply = (value, status=200) => {res.writeHead(status, {'Content-Type':'application/json'}); res.end(JSON.stringify(value));};
    if (endpoint==='session' && req.method==='POST') {
      let raw=''; for await(const chunk of req) raw+=chunk; assert.deepEqual(JSON.parse(raw),{username:'admin',password:'fixture-password'}); authenticated=true; res.setHeader('Set-Cookie','fixture_session=valid; HttpOnly; SameSite=Strict; Path=/');
      loginAt = nowSecs(); deadline = loginAt + IDLE;
      return reply({success:true,expiresIn:IDLE,expiresAt:deadline});
    }
    if (authenticated && deadline && nowSecs() >= deadline) authenticated = false;
    if (!authenticated || !req.headers.cookie?.includes('fixture_session=valid')) return reply({error:'Fixture authentication required'},401);
    // Use moves the idle deadline; a background request (automatic refresh) does not.
    if (req.headers['x-admin-background'] !== '1') deadline = Math.min(nowSecs() + IDLE, loginAt + MAX);
    res.setHeader('x-admin-session-expires', String(deadline));
    let body={};
    if(req.method==='POST') {assert.equal(req.headers['x-csrf-token'],'fixture-csrf'); let raw=''; for await(const chunk of req) raw+=chunk; body=JSON.parse(raw||'{}'); writes.push({endpoint,body});}
    if(endpoint==='me' || (endpoint==='session' && req.method==='GET')) return reply({success:true,role:'admin',username:'admin',csrfToken:'fixture-csrf',expiresAt:deadline,expiresIn:deadline-nowSecs(),twoFactorEnabled:false,totpRequired:false});
    if(endpoint==='cards/reveal') return reply({success:true,rawCode:'FIXTURE-RECOVERED-CODE'});
    if(endpoint==='session/revoke') {authenticated=false; res.setHeader('Set-Cookie','fixture_session=; Max-Age=0; Path=/'); return reply({success:true});}
    if(endpoint==='stats') return reply({success:true,stateBytes:storage.bytes,stateWarningBytes:storage.warning,stateCeilingBytes:storage.ceiling,totalCards:cards.length,activeCards:2,unactivatedCards:1,frozenCards:1,bannedCards:1,totalCredits:12000000000,usedCredits:1500000000,remainingCredits:10500000000,totalPoints:12000,usedPoints:1500,remainingPoints:10500,activity:activity()});
    if(endpoint==='cards') return reply({success:true,count:cards.length,cards:cards.map(view),revision:cardRevision()});
    // One card's history, newest first: this session's changes, then realistic older events.
    if(endpoint==='cards/history') {
      const cardId=url.searchParams.get('card_id'); const card=cards.find(c=>c.id===cardId);
      if(!card) return reply({__type:'ResourceNotFoundException',message:'没有这张卡密'},404);
      const t=Math.floor(Date.now()/1000);
      const recent=log.filter(event=>event.cardId===cardId).reverse().map(({cardId:_,key:__,...event})=>({invocationId:null,detail:null,...event}));
      const older=[];
      if(card.status==='banned') older.push({ts:t-3600,action:'ban',credits:0,points:0,operator:'admin',reason:'滥用'});
      if(card.status==='frozen') older.push({ts:t-5400,action:'freeze',credits:0,points:0,operator:'admin',reason:'客户要求'});
      if(card.activatedAt) older.push({ts:t-7200,action:'adjust',credits:50000000,points:50,operator:'admin',reason:'补偿 9/25 上游中断'});
      if(card.activatedAt) older.push({ts:card.activatedAt,action:'activated',credits:0,points:0,operator:'system',reason:null});
      older.push({ts:(card.activatedAt??t)-86400,action:'issued',credits:card.creditTotal,points:card.pointsTotal,operator:'admin',reason:null});
      return reply({success:true,cardId,card:view(card),events:[...recent,...older.map(event=>({invocationId:null,detail:null,...event}))]});
    }
    if(endpoint==='traces') {const cardId=url.searchParams.get('card_id'),limit=Number(url.searchParams.get('limit')||100);return reply({success:true,traces:traces.filter(t=>!cardId||t.card_id===cardId).slice(0,limit)});}
    if(endpoint==='traces/content') {const record=traceContent(url.searchParams.get('invocation_id'));return record?reply(record):reply({__type:'ResourceNotFoundException',message:'没有这次请求的内容（只保留 24 小时）'},404);}
    if(endpoint==='commercial-config') {
      if(req.method==='POST') {
        if(body.settings){assert.deepEqual(Object.keys(body).sort(),['expected_revision','reason','settings']);assert.deepEqual(Object.keys(body.settings).sort(),['credit_face_value_cny','usd_cny_rate']);}
        assert.ok(Object.keys(body).every(key=>['expected_revision','reason','settings','groups','models','rate_cards','versions','removed_models','cancelled_versions'].includes(key)));
        const result=publish(body);return reply(result.body,result.status);
      }
      return reply({success:true,config});
    }
    if(endpoint==='providers') return reply({success:true,providers,keys});
    if(endpoint==='providers/status') {const provider=providers.find(p=>p.id===body.providerId);if(!provider)return reply({success:false,error:'Unknown provider'},404);provider.enabled=body.enabled;return reply({success:true,providerId:provider.id,enabled:provider.enabled});}
    if(endpoint==='providers/update') {const provider=providers.find(p=>p.id===body.id);if(!provider)return reply({success:false,error:'Unknown provider'},404);for(const field of ['name','base_url','format'])if(body[field]!==undefined)provider[field]=body[field];return reply({success:true,provider});}
    if(endpoint==='providers/delete') {
      const index=providers.findIndex(p=>p.id===body.id);if(index<0)return reply({success:false,error:'Unknown provider'},404);
      const users=config.models.filter(m=>m.target_provider_id===body.id||(m.fallback_chain||[]).some(f=>f.provider_id===body.id)).map(m=>m.exposed_model_id);
      if(users.length)return reply({success:false,error:`Provider still routes models: ${users.join(', ')}`},409);
      const own=keys.filter(k=>k.provider_id===body.id).map(k=>k.id);if(own.length)return reply({success:false,error:`Provider still has Keys: ${own.join(', ')}`},409);
      providers.splice(index,1);return reply({success:true});
    }
    // A Key's save, delete, health reset, and one tiny real request (测试).
    const keyOf=()=>keys.find(k=>k.id===body.key_id&&k.provider_id===body.provider_id);
    if(endpoint==='providers/keys') {
      if(!providers.some(p=>p.id===body.provider_id))return reply({success:false,error:'Unknown provider'},404);
      let key=keyOf();if(!key){if(!body.api_key)return reply({success:false,error:'API key required for new key'},400);key={id:body.key_id,provider_id:body.provider_id,weight:1,enabled:true,health_state:'healthy'};keys.push(key);}
      // A new secret puts the Key back to healthy, as the server does; its last error stays on record.
      Object.assign(key,{allowed_models:[...new Set(body.allowed_models)].sort(),...(body.weight!==undefined?{weight:body.weight}:{}),...(body.enabled!==undefined?{enabled:body.enabled}:{}),...(body.api_key?{health_state:'healthy',cooldown_until:null}:{})});
      return reply({success:true,keys:keys.filter(k=>k.provider_id===body.provider_id),published:false});
    }
    if(endpoint==='providers/keys/delete') {
      const key=keyOf();if(!key)return reply({success:false,error:'Unknown key'},404);
      const rest=keys.filter(k=>k!==key);
      const stranded=config.models.filter(m=>m.visible!==false&&m.retired!==true&&m.target_provider_id===key.provider_id&&serves(m.target_provider_id,m.target_model)&&!serves(m.target_provider_id,m.target_model,rest)).map(m=>m.exposed_model_id);
      if(stranded.length)return reply({success:false,error:`Key still serves visible models: ${stranded.join(', ')}`},409);
      keys.splice(keys.indexOf(key),1);return reply({success:true});
    }
    if(endpoint==='providers/keys/reset') {const key=keyOf();if(!key)return reply({success:false,error:'Unknown key'},404);Object.assign(key,{health_state:'healthy',cooldown_until:null});return reply({success:true});}
    if(endpoint==='providers/keys/probe') {
      const key=body.key_id?keyOf():keys.find(k=>k.provider_id===body.provider_id&&k.enabled!==false&&(!Array.isArray(k.allowed_models)||k.allowed_models.includes(body.model)));
      if(!key)return body.key_id?reply({success:false,error:'Unknown key'},404):reply({success:false,error:'No enabled Key of this provider may call this model'},409);
      if(String(body.model).includes('overloaded'))return reply({success:true,ok:false,status:529,latency_ms:1840,ttft_ms:null,error:'HTTP 529 overloaded_error: Overloaded',reply:null,key_id:key.id});
      return reply({success:true,ok:true,status:200,latency_ms:620,ttft_ms:410,error:null,reply:'OK',key_id:key.id});
    }
    if(endpoint==='financials') {
      const bound=name=>{const value=url.searchParams.get(name);return value===null?undefined:/^\d+$/.test(value)?Number(value):NaN;};
      const from=bound('fromSecs'),to=bound('toSecs');
      if(Number.isNaN(from)||Number.isNaN(to))return reply({success:false,error:'fromSecs and toSecs must be whole seconds'},400);
      if(from!==undefined&&to!==undefined&&from>=to)return reply({success:false,error:'fromSecs must be before toSecs'},400);
      return reply(financials(from,to));
    }
    if(endpoint==='announcements') return reply({success:true,announcements:[{id:'fixture-notice',title:'本地测试：服务维护通知',content:'这是隔离的视觉测试公告，不会向真实用户发布。',level:'info',enabled:true,created_at:now}]});
    if(endpoint==='cards/batch') {
      assert.equal(body.maxDevices,1); assert.equal('creditTotal' in body,false); assert.ok(groups.some(g=>g.id===body.groupId));
      const points=Number(body.templateId.replace('tier-','')); assert.ok([1000,2000,5000,10000].includes(points));
      const generated=Array.from({length:body.count},(_,i)=>({cardId:`fixture-issued-${cards.length+i}`,rawCode:`FIXTURE-NOT-VALID-${points}-${i}`,groupId:body.groupId,creditTotal:points*1000000,status:'unactivated'}));
      generated.forEach(c=>cards.push({id:c.cardId,codeRecoverable:true,status:c.status,creditTotal:c.creditTotal,creditUsed:0,availableCredits:c.creditTotal,pointsTotal:points,pointsAvailable:points,boundDevices:[],maxDevices:1,groupId:c.groupId,note:body.note}));
      return reply({success:true,cards:generated});
    }
    // Card support, as the admin API answers: refusals are {success:false, error} in the server's words
    // (400 malformed, 404 unknown card or device, 409 a rule refused), each change is written to the
    // card's history with the operator and the reason, and the reply shows the card as it now is.
    const cardFail=(status,error)=>reply({success:false,error},status);
    const support=reason=>typeof reason==='string'&&!!reason.trim()&&Buffer.byteLength(reason.trim())<=200;
    const record=(cardId,action,reason,extra={})=>log.push({cardId,ts:Math.floor(Date.now()/1000),action,credits:0,points:0,operator:'admin',reason:reason?.trim()||null,...extra});
    // The action endpoints refuse fields they do not know, as the server's deny_unknown_fields does.
    const unknownField=allowed=>Object.keys(body).find(key=>!allowed.includes(key));
    if(endpoint==='cards/status') {
      if(body.action==='unban'&&!support(body.reason))return cardFail(400,'A reason of 1 to 200 bytes is required');
      const card=cards.find(c=>c.id===body.cardId); if(!card)return cardFail(400,`Card ${body.cardId} not found`);
      const debug={active:'Active',unactivated:'Unactivated',frozen:'Frozen',banned:'Banned',voided:'Voided',expired:'Expired'}[card.status];
      if(body.action==='unban'){
        if(card.status!=='banned')return cardFail(400,`Invalid billing state: cannot unban ${debug}`);
        if(card.archivedAt!=null)return cardFail(400,'Invalid billing state: Archived cards must be unarchived before they are unbanned');
        card.status=card.activatedAt!=null?'active':'unactivated';
      } else if(body.action==='archive'||body.action==='unarchive')card.archivedAt=body.action==='archive'?Math.floor(Date.now()/1000):null;
      else card.status=body.action==='freeze'?'frozen':body.action==='unfreeze'?'active':body.action==='void'?'voided':'banned';
      record(card.id,body.action,body.reason);
      return reply({success:true,cardId:card.id,newStatus:card.status,archivedAt:card.archivedAt??null,card:view(card)});
    }
    // A balance adjustment, as its handler answers: one idempotency key per intent (a replay with the
    // same parameters answers the same), and optionally the request it makes up for.
    if(endpoint==='cards/adjust') {
      const invalid=message=>reply({__type:'InvalidRequestException',message},400);
      const key=String(req.headers['idempotency-key']??body.idempotencyKey??'').trim();
      if(!/^[A-Za-z0-9_.:-]{1,128}$/.test(key))return invalid('a valid idempotency_key is required (1-128 ASCII letters, digits, -_.:)');
      const delta=body.deltaPoints;
      if(typeof delta!=='number'||!Number.isFinite(delta)||delta===0||Math.abs(delta)>1000000||!String(body.cardId??'').trim())return invalid('delta_points must be finite, non-zero, bounded, and card_id must be valid');
      if(body.invocationId!==undefined&&!/^[A-Za-z0-9_.:-]{1,128}$/.test(String(body.invocationId).trim()))return invalid('invocationId must be 1-128 ASCII letters, digits, -_.:');
      const card=cards.find(c=>c.id===body.cardId);if(!card)return reply({success:false,error:`Card ${body.cardId} not found`},404);
      const micro=Math.round(delta*1e6),invocationId=body.invocationId?.trim()??null,replay=log.find(event=>event.key===key);
      if(replay){
        if(replay.cardId!==card.id||replay.credits!==micro||replay.reason!==body.reason||replay.invocationId!==invocationId)return reply({success:false,error:`Invalid balance adjustment: Idempotency conflict: ${key}`},409);
      } else {
        if(card.availableCredits+micro<0)return reply({success:false,error:`Card error: Insufficient credit: available ${card.availableCredits} micro-credits, needed ${-micro}`},409);
        Object.assign(card,{creditTotal:card.creditTotal+micro,availableCredits:card.availableCredits+micro,pointsTotal:card.pointsTotal+delta,pointsAvailable:Math.round((card.pointsAvailable+delta)*1e6)/1e6});
        record(card.id,'adjust',body.reason,{credits:micro,points:delta,invocationId,key});
      }
      return reply({success:true,cardId:card.id,newAvailableCredits:card.availableCredits,newAvailablePoints:card.availableCredits/1e6});
    }
    if(endpoint==='cards/devices/unbind') {
      const field=unknownField(['cardId','deviceId','reason']);if(field)return cardFail(400,`Invalid request body: unknown field \`${field}\``);
      if(!body.cardId||!String(body.deviceId??'').trim())return cardFail(400,'cardId and deviceId are required');
      if(!support(body.reason))return cardFail(400,'A reason of 1 to 200 bytes is required');
      const card=cards.find(c=>c.id===body.cardId);if(!card)return cardFail(404,`Card ${body.cardId} not found`);
      if(!card.boundDevices.includes(body.deviceId))return cardFail(404,`Device ${body.deviceId} not found for card ${card.id}`);
      card.boundDevices=card.boundDevices.filter(device=>device!==body.deviceId);
      record(card.id,'unbind',body.reason,{detail:{deviceId:body.deviceId}});
      return reply({success:true,card:view(card)});
    }
    if(endpoint==='cards/rebinds/reset') {
      const field=unknownField(['cardId','reason']);if(field)return cardFail(400,`Invalid request body: unknown field \`${field}\``);
      if(!support(body.reason))return cardFail(400,'A reason of 1 to 200 bytes is required');
      const card=cards.find(c=>c.id===body.cardId);if(!card)return cardFail(404,`Card ${body.cardId} not found`);
      const until=view(card).rebindCooldownUntil;
      // Already as asked: nothing is written.
      if((card.rebindsUsed??0)>0||until){record(card.id,'rebinds_reset',body.reason,{detail:{previousRebinds:card.rebindsUsed??0,previousCooldownUntil:until}});card.rebindsUsed=0;card.rebindCooldownUntil=null;}
      return reply({success:true,card:view(card)});
    }
    if(endpoint==='cards/validity') {
      const field=unknownField(['cardIds','days','validUntilSecs','reason']);if(field)return cardFail(400,`Invalid request body: unknown field \`${field}\``);
      const ids=Array.isArray(body.cardIds)?body.cardIds:[];
      if(!ids.length||ids.length>500)return cardFail(400,'cardIds must name 1 to 500 cards');
      const t=Math.floor(Date.now()/1000),{days,validUntilSecs:until}=body;
      if(days!==undefined&&until===undefined){if(!(Number.isInteger(days)&&days>=1&&days<=3650))return cardFail(400,'days must be between 1 and 3650');}
      else if(until!==undefined&&days===undefined){if(!(Number.isInteger(until)&&until>t&&until<=t+3650*86400))return cardFail(400,'validUntilSecs must be in the future and within 3650 days');}
      else return cardFail(400,'Give exactly one of days and validUntilSecs');
      if(!support(body.reason))return cardFail(400,'A reason of 1 to 200 bytes is required');
      const unique=[...new Set(ids.map(id=>String(id).trim()))],found=unique.map(id=>cards.find(c=>c.id===id));
      const missing=unique.filter((_,i)=>!found[i]);if(missing.length)return cardFail(404,`Card ${missing.join(', ')} not found`);
      const waiting=card=>card.activatedAt==null&&card.validUntil==null&&!['active','expired'].includes(card.status);
      const named=(test,message)=>{const hit=found.filter(test).map(card=>card.id);return hit.length?`${message}: ${hit.join(', ')}`:null;};
      const refusal=named(card=>card.status==='voided','Voided cards cannot be extended')??named(card=>card.archivedAt!=null,'Archived cards must be unarchived before they are extended')
        ??named(card=>waiting(card)?card.activationDurationSecs===0:card.validUntil==null,'Cards that never expire cannot be extended')
        ??(until!==undefined?named(waiting,'An expiry date applies only to activated cards')??named(card=>card.validUntil>until,'The new expiry is earlier than the current one of'):null);
      if(refusal)return cardFail(409,refusal);
      for(const card of found){
        if(waiting(card)){card.activationDurationSecs=(card.activationDurationSecs??2592000)+days*86400;record(card.id,'extend',body.reason,{detail:{activationDurationSecs:card.activationDurationSecs}});}
        else{card.validUntil=until??Math.max(card.validUntil,t)+days*86400;if(card.status==='expired')card.status='active';record(card.id,'extend',body.reason,{detail:{validUntil:card.validUntil}});}
      }
      return reply({success:true,count:found.length,cards:found.map(view)});
    }
    if(endpoint==='cards/note') {
      const field=unknownField(['cardId','note']);if(field)return cardFail(400,`Invalid request body: unknown field \`${field}\``);
      if(!String(body.cardId??'').trim())return cardFail(400,'cardId is required');
      const note=String(body.note??'').trim();
      if(Buffer.byteLength(note)>256||/[\x00-\x1f\x7f-\x9f]/.test(note))return cardFail(400,'note must be at most 256 bytes, without control characters');
      const card=cards.find(c=>c.id===body.cardId);if(!card)return cardFail(404,`Card ${body.cardId} not found`);
      if((card.note??'')!==note){if(note)card.note=note;else delete card.note;record(card.id,'note','');}
      return reply({success:true,card:view(card)});
    }
    if(endpoint==='cards/group') {
      const field=unknownField(['cardId','groupId','reason']);if(field)return cardFail(400,`Invalid request body: unknown field \`${field}\``);
      if(!String(body.cardId??'').trim()||!String(body.groupId??'').trim())return cardFail(400,'cardId and groupId are required');
      if(!support(body.reason))return cardFail(400,'A reason of 1 to 200 bytes is required');
      const card=cards.find(c=>c.id===body.cardId);if(!card)return cardFail(404,`Card ${body.cardId} not found`);
      const group=config.groups.find(g=>g.id===body.groupId);
      if(!group)return cardFail(409,`Unknown group: ${body.groupId}`);
      if(group.issuance_enabled===false)return cardFail(409,`Group does not take cards: ${body.groupId}`);
      if(card.groupId!==group.id){record(card.id,'group',body.reason,{detail:{previousGroupId:card.groupId,groupId:group.id}});card.groupId=group.id;}
      return reply({success:true,card:view(card)});
    }
    // Archiving the ledger, as its handler answers: a receipt, and the saved size before and after.
    if(endpoint==='ledger/archive') {
      const t=Math.floor(Date.now()/1000),before=body.beforeTsSecs;
      if(!Number.isSafeInteger(before)||before<0)return reply({__type:'SerializationException',message:'invalid type: expected u64'},400);
      if(before>t)return reply({__type:'ValidationException',message:'beforeTsSecs must not be in the future'},400);
      // The fixture's ledger reaches back 90 days; archived entries are gone from it.
      const from=Math.max(t-90*86400,storage.archivedBefore);
      if(before<=from)return reply({__type:'ValidationException',message:'No ledger entries match the archival cutoff timestamp'},400);
      const drained=Math.round((before-from)/86400*200),bytesBefore=storage.bytes,archiveId=`arc-${t}-${storage.archivedBefore?1:0}-42`;
      storage.bytes=Math.max(4194304,storage.bytes-drained*3600);storage.archivedBefore=before;
      return reply({success:true,receipt:{archive_id:archiveId,archive_file:`ledger_archive_${archiveId}.json`,drained_entries_count:drained,sha256_checksum:crypto.createHash('sha256').update(archiveId).digest('hex'),
        before_ts_secs:before,created_at_secs:t},stateBytesBefore:bytesBefore,stateBytesAfter:storage.bytes,stateCeilingBytes:storage.ceiling});
    }
    if(endpoint==='exports/ledger.csv') {res.writeHead(200,{'Content-Type':'text/csv; charset=utf-8'}); return res.end(ledgerCsv());}
    if(endpoint==='exports/ledger.json') return reply(ledger);
    throw new Error(`Unhandled fixture endpoint: ${req.method} ${endpoint}`);
  }};
};