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
    return {...card, ...cardPlanFields(card), effectiveStatus: card.status === 'active' && card.validUntil != null && t >= card.validUntil ? 'expired' : card.status,
      rebindsUsed: card.rebindsUsed ?? 0, maxRebinds: card.maxRebinds ?? 5, rebindCooldownUntil: card.rebindCooldownUntil > t ? card.rebindCooldownUntil : null,
      activationDurationSecs: card.activatedAt == null ? card.activationDurationSecs ?? 2592000 : null};
  };
  // What the support actions and adjustments wrote to each card's history, oldest first.
  const log = [];
  // The saved billing state's size, as GET /stats reports it (billing's warning level and ceiling),
  // and how far back the ledger has been archived.
  // 公告 as the server keeps them; the view adds where each stands now.
  const notices = [{id: 'fixture-notice', title: '本地测试：服务维护通知', content: '这是隔离的视觉测试公告，不会向真实用户发布。', level: 'info', enabled: true, created_at: realNow, expires_at: null, audience: [], edits: []}];
  const noticeView = (notice, t) => ({...notice, starts_at: notice.starts_at ?? notice.created_at,
    status: !notice.enabled ? 'withdrawn' : notice.expires_at != null && t >= notice.expires_at ? 'ended' : t < (notice.starts_at ?? notice.created_at) ? 'scheduled' : 'active'});
  // The saved state: its size, when it was last saved, and why the latest save failed (null: it did not).
  const storage = {bytes: 13212876, warning: 33554432, ceiling: 268435456, archivedBefore: 0, savedAt: realNow - 180, persistenceError: null};
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
  // 套餐 (billing template.rs): the four tiers cards were issued from before the catalog, which a
  // state that stores no plans still has, and what a card issued before it is taken to be.
  const TIERS = [['tier-1000', 'PRO', 1000, 30, 'PRO', 10], ['tier-2000', 'PRO+', 2000, 55, 'PRO_PLUS', 20], ['tier-5000', 'PRO Max', 5000, 130, 'PRO_MAX', 30], ['tier-10000', 'Power', 10000, 250, 'POWER', 40]];
  const seedPlans = () => {
    const ids = config.groups.map(group => group.id), group = ids.includes('group-pro-plus') ? 'group-pro-plus' : [...ids].sort()[0] ?? 'group-pro-plus';
    return TIERS.map(([id, name, points, price_cny, kiro_plan_type, sort_order]) => ({id, name, points, price_cny, validity_days: 30, max_devices: 1, concurrency: 2, default_group_id: group, kiro_plan_type, on_sale: true, sort_order}));
  };
  // The catalog in force, by sort order then ID; config.plans once a publication has stored them.
  const catalog = () => [...(config.plans ?? seedPlans())].sort((a, b) => a.sort_order - b.sort_order || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const legacyTier = card => TIERS.find(([, , points]) => points * 1e6 === card.creditTotal);
  // A card's plan: the one it was issued from (kept as it was then), else the tier its credits name.
  const soldAs = card => card.plan ? {id: card.plan.id, name: card.plan.name, points: card.plan.points, price: card.plan.priceMicroCny}
    : legacyTier(card) ? {id: legacyTier(card)[0], name: legacyTier(card)[1], points: legacyTier(card)[2], price: legacyTier(card)[3] * 1e6} : null;
  const cardPlanFields = card => {const sold = soldAs(card), tier = legacyTier(card);
    return {planId: sold?.id ?? null, planName: sold?.name ?? null, kiroPlanType: card.plan?.kiroPlanType ?? tier?.[4] ?? 'CUSTOM', plan: card.plan ?? null};};
  const cardsByPlan = () => Object.fromEntries(catalog().map(plan => [plan.id, cards.filter(card => soldAs(card)?.id === plan.id).length]));
  // What GET commercial-config answers: the configuration with its catalog and the cards issued from each plan.
  const configView = () => ({...config, plans: catalog(), cards_by_plan: cardsByPlan()});
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
    // Sales: cards issued (a day before they were activated, as their history says, or when issued here) and activated in
    // the period, by plan, each at the price it was sold at (a card issued before the catalog, at its tier's).
    const row = (id, name, points, price) => ({templateId: id, planId: id, name, points, priceMicroCny: price, issuedCards: 0, activatedCards: 0, issuedValueMicroCny: 0, activatedValueMicroCny: 0});
    const byPlan = catalog().map(plan => row(plan.id, plan.name, plan.points, Math.round(plan.price_cny * 1e6)));
    const sales = {issuedCards: 0, issuedValueMicroCny: 0, activatedCards: 0, activatedValueMicroCny: 0, unpricedIssuedCards: 0, unpricedActivatedCards: 0, byPlan};
    for (const card of cards) {
      const issued = within(card.issuedAt ?? (card.activatedAt ?? t) - DAY) && !(card.status === 'voided' && card.activatedAt == null), activated = card.activatedAt != null && within(card.activatedAt);
      if (!issued && !activated) continue;
      sales.issuedCards += issued ? 1 : 0; sales.activatedCards += activated ? 1 : 0;
      const sold = soldAs(card);
      if (!sold) {sales.unpricedIssuedCards += issued ? 1 : 0; sales.unpricedActivatedCards += activated ? 1 : 0; continue;}
      let plan = byPlan.find(entry => entry.planId === sold.id);
      if (!plan) {plan = row(sold.id, sold.name, sold.points, sold.price); byPlan.push(plan);}
      if (issued) {plan.issuedCards++; plan.issuedValueMicroCny += sold.price; sales.issuedValueMicroCny += sold.price;}
      if (activated) {plan.activatedCards++; plan.activatedValueMicroCny += sold.price; sales.activatedValueMicroCny += sold.price;}
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
      modelRankings: rankings, byProvider, sales, liability,
      planPrices: catalog().map(plan => ({templateId: plan.id, planId: plan.id, name: plan.name, points: plan.points, priceMicroCny: Math.round(plan.price_cny * 1e6), onSale: plan.on_sale})),
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
  // Each field of a published plan is bounded as billing's Plan::problem bounds it.
  const PLAN_FIELDS = ['id', 'name', 'points', 'price_cny', 'validity_days', 'max_devices', 'concurrency', 'default_group_id', 'kiro_plan_type', 'on_sale', 'sort_order'];
  const planProblem = plan => {
    const whole = (value, min, max) => Number.isInteger(value) && value >= min && value <= max, fen = plan.price_cny * 100;
    if (typeof plan.id !== 'string' || !/^[a-z0-9-]{1,64}$/.test(plan.id)) return 'Plan IDs are 1-64 of a-z, 0-9 and -';
    if (typeof plan.name !== 'string' || !plan.name.trim() || Buffer.byteLength(plan.name) > 32 || /[\u0000-\u001f\u007f-\u009f]/.test(plan.name)) return 'Plan names are 1-32 bytes';
    if (!whole(plan.points, 1, 10000000)) return 'Plan points must be 1-10000000';
    if (typeof plan.price_cny !== 'number' || !(plan.price_cny >= 0 && plan.price_cny <= 100000) || Math.abs(Math.round(fen) - fen) > 1e-6) return 'Plan prices must be 0-100000 yuan, to the fen';
    if (!whole(plan.validity_days, 1, 3650)) return 'Plan validity must be 1-3650 days';
    if (!whole(plan.max_devices, 1, 10)) return 'Plans allow 1-10 devices';
    if (!whole(plan.concurrency, 1, 20)) return 'Plan concurrency must be 1-20';
    if (!['PRO', 'PRO_PLUS', 'PRO_MAX', 'POWER', 'CUSTOM'].includes(plan.kiro_plan_type)) return 'Plan Kiro types are PRO, PRO_PLUS, PRO_MAX, POWER or CUSTOM';
    return null;
  };
  // Official pricing, checked as crates/billing/src/commercial.rs checks it.
  const OFFICIAL = ['input_usd_per_m', 'output_usd_per_m', 'cache_creation_usd_per_m', 'cache_read_usd_per_m'];
  const CREDITS = ['fixed_input_credit_per_m', 'fixed_output_credit_per_m', 'fixed_cache_creation_credit_per_m', 'fixed_cache_read_credit_per_m'];
  const COSTS = ['input_price_per_m', 'output_price_per_m', 'cache_creation_price_per_m', 'cache_read_price_per_m'];
  const OPTIONAL_SETTINGS = ['official_usd_cny', 'default_price_multiplier', 'default_cost_multiplier', 'provider_cost_multipliers', 'official_prices', 'route_costs'];
  const text = (value, max) => typeof value === 'string' && !!value.trim() && Buffer.byteLength(value) <= max && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
  const number = value => typeof value === 'number' && Number.isFinite(value);
  const positive = value => number(value) && value > 0 && value <= 1000;
  const multiplier = value => number(value) && value > 0 && value <= 100;
  const usd = value => number(value) && value >= 0 && value <= 10000;
  const officialRate = settings => settings.official_usd_cny ?? 1;
  const officialMismatch = (v, o, routeCost) => {
    const official = OFFICIAL.map(field => o[field]), basis = o.cost_basis_usd_per_m ?? official;
    if (v.pricing_mode !== 'fixed' || v.currency !== 'CNY' || v.margin_multiplier !== 1 || v.per_call_credit !== 0 || ![...official, ...basis].every(usd) || basis.length !== 4
      || !official.some(value => value > 0) || !multiplier(o.price_multiplier) || !multiplier(o.cost_multiplier) || !positive(o.usd_cny) || !positive(o.credit_face_value_cny)) return 'Invalid official pricing';
    if (official.some((value, i) => routeCost ? v[CREDITS[i]] !== 0 : Math.abs(v[CREDITS[i]] - Math.round(value * o.price_multiplier * o.usd_cny / o.credit_face_value_cny * 1000000)) > 1)) return 'Official pricing does not match the credits of';
    if (basis.some((value, i) => {const expected = value * o.cost_multiplier * o.usd_cny, cost = v[COSTS[i]]; return !(Math.abs(cost - expected) <= Math.max(1e-9 * Math.max(Math.abs(cost), Math.abs(expected)), 1e-12));})) return 'Official pricing does not match the cost of';
    return '';
  };
  // A request field the server cannot deserialize is a 400, as serde reports it.
  const malformed = message => ({status: 400, body: {success: false, error: message}});
  const publish = body => {
    if (!text(body.reason, 500)) return refuse('Publication reason required (max 500 bytes)');
    if (!['groups', 'models', 'rate_cards', 'versions', 'removed_models', 'cancelled_versions', 'plans', 'removed_plans'].some(key => (body[key] || []).length) && !body.settings) return refuse('Empty publication');
    if (body.expected_revision !== config.revision) return refuse('Configuration changed; reload before publishing');
    const t = Math.floor(Date.now() / 1000);
    let settings = config.settings, repriced = false;
    if (body.settings) {
      const next = body.settings;
      if (!number(next.credit_face_value_cny) || !number(next.usd_cny_rate)) return malformed('missing field `credit_face_value_cny` or `usd_cny_rate`');
      if (!positive(next.credit_face_value_cny) || next.credit_face_value_cny < 0.0001 || !positive(next.usd_cny_rate)) return refuse('Face value must be 0.0001-1000 and the exchange rate positive and at most 1000');
      // Left out, the newer settings keep their value: a console that does not know them never wipes them. Unknown fields are dropped.
      settings = {credit_face_value_cny: next.credit_face_value_cny, usd_cny_rate: next.usd_cny_rate};
      for (const field of OPTIONAL_SETTINGS) {const value = next[field] ?? config.settings[field]; if (value != null) settings[field] = value;}
      if (Object.values(settings.official_prices ?? {}).some(price => !price || OFFICIAL.some(field => !number(price[field])))) return malformed('missing field in official_prices');
      if (Object.values(settings.route_costs ?? {}).some(cost => !cost || typeof cost !== 'object')) return malformed('invalid type in route_costs');
      if (settings.official_usd_cny !== undefined && !positive(settings.official_usd_cny)) return refuse('The official dollar rate must be positive and at most 1000');
      const byProvider = Object.entries(settings.provider_cost_multipliers ?? {});
      if ([settings.default_price_multiplier, settings.default_cost_multiplier].some(value => value !== undefined && !multiplier(value)) || byProvider.length > 200 || byProvider.some(([id, value]) => !text(id, 128) || !multiplier(value)))
        return refuse('Multipliers must be positive and at most 100, for at most 200 providers');
      const prices = Object.entries(settings.official_prices ?? {});
      if (prices.length > 1000 || prices.some(([name, price]) => !text(name, 256) || !OFFICIAL.every(field => usd(price[field])) || (price.note != null && (typeof price.note !== 'string' || Buffer.byteLength(price.note) > 256 || /[\u0000-\u001f\u007f-\u009f]/.test(price.note)))))
        return refuse('Official prices: at most 1000, named in 1-256 bytes, priced 0-10000, notes of at most 256 bytes');
      const routes = Object.entries(settings.route_costs ?? {});
      if (routes.length > 1000 || routes.some(([route, cost]) => {const cut = route.indexOf('/'); return !text(route, 256) || cut < 1 || cut === route.length - 1 || (cost.cost_multiplier != null && !multiplier(cost.cost_multiplier)) || (cost.basis_usd_per_m != null && !(Array.isArray(cost.basis_usd_per_m) && cost.basis_usd_per_m.length === 4 && cost.basis_usd_per_m.every(usd)));}))
        return refuse('Route costs: at most 1000, named <provider>/<upstream model> in at most 256 bytes, multipliers positive and at most 100, prices 0-10000');
      // An official price's time is when its prices last changed; the server stamps it.
      if (settings.official_prices) settings.official_prices = Object.fromEntries(prices.map(([name, price]) => {
        const old = config.settings.official_prices?.[name];
        return [name, {...Object.fromEntries(OFFICIAL.map(field => [field, price[field]])), ...(price.note != null ? {note: price.note} : {}),
          updated_at_secs: old && OFFICIAL.every(field => old[field] === price[field]) ? old.updated_at_secs : t}];
      }));
      if (settings.route_costs) settings.route_costs = Object.fromEntries(routes.map(([route, cost]) => [route, {...(cost.cost_multiplier != null ? {cost_multiplier: cost.cost_multiplier} : {}), ...(cost.basis_usd_per_m != null ? {basis_usd_per_m: cost.basis_usd_per_m} : {})}]));
      repriced = settings.credit_face_value_cny !== config.settings.credit_face_value_cny || officialRate(settings) !== officialRate(config.settings);
      settings.rate_updated_at_secs = t;
    }
    // Plans: the first publication that changes one stores the catalog, the seed with it; only those listed are checked.
    let nextPlans;
    if ((body.plans || []).length || (body.removed_plans || []).length) {
      const plans = (config.plans ?? seedPlans()).map(plan => ({...plan})), listed = new Set(), groupIds = [...config.groups, ...(body.groups || [])].map(group => group.id);
      for (const plan of body.plans || []) {
        assert.deepEqual(Object.keys(plan).sort(), [...PLAN_FIELDS].sort(), 'a plan is published with every field');
        const problem = planProblem(plan);
        if (problem) return refuse(`${problem}: ${plan.id}`);
        if (listed.has(plan.id)) return refuse(`Duplicate plan: ${plan.id}`);
        listed.add(plan.id);
        if (!groupIds.includes(plan.default_group_id)) return refuse(`Unknown default group of plan: ${plan.id}`);
        const index = plans.findIndex(old => old.id === plan.id);
        if (index >= 0) plans[index] = {...plan}; else plans.push({...plan});
      }
      for (const id of body.removed_plans || []) {
        const index = plans.findIndex(plan => plan.id === id);
        if (index < 0) return refuse(`Unknown plan: ${id}`);
        if (cards.some(card => soldAs(card)?.id === id)) return refuse(`Plans cards were issued from can only be taken off sale: ${id}`);
        plans.splice(index, 1);
      }
      if (plans.length > 100) return refuse('At most 100 plans');
      nextPlans = plans.sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    }
    for (const m of body.models || []) for (const id of [m.exposed_model_id, ...(m.aliases || [])]) if (!/^[A-Za-z0-9._:/-]{1,128}$/.test(String(id))) return refuse(`Invalid model ID: ${id}`);
    for (const m of body.models || []) {const old = config.models.find(row => row.id === m.id); if (old && old.group_id !== m.group_id) return refuse('Cannot move mapping between groups');}
    const nextModels = [...config.models.filter(row => !(body.models || []).some(next => next.id === row.id)), ...(body.models || [])];
    for (const id of body.removed_models || []) {const m = nextModels.find(row => row.id === id); if (!m || (m.visible !== false && m.retired !== true)) return refuse(`Only hidden or retired mappings can be removed: ${id}`);}
    const models = nextModels.filter(row => !(body.removed_models || []).includes(row.id));
    const exposed = new Set();
    for (const m of models) for (const name of [m.exposed_model_id, ...(m.aliases || [])]) {const key = `${m.group_id}\n${name}`; if (exposed.has(key)) return refuse('Ambiguous model ID or alias'); exposed.add(key);}
    // Only the entries in this publication are checked: a shown, unretired one needs its primary target to serve.
    const stranded = [...new Set((body.models || []).filter(m => m.visible !== false && m.retired !== true && models.some(row => row.id === m.id) && !serves(m.target_provider_id, m.target_model)).map(m => m.exposed_model_id))];
    if (stranded.length) return refuse(`Visible model target has no enabled compatible key: ${stranded.join(', ')}`);
    // A scheduled price is withdrawn before this publication's prices, which may replace it.
    for (const id of body.cancelled_versions || []) {const v = config.versions.find(row => row.id === id); if (!v || v.effective_from_secs <= t) return refuse(`Only scheduled prices can be cancelled: ${id}`);}
    let versions = config.versions.filter(row => !(body.cancelled_versions || []).includes(row.id));
    const priced = new Set(versions.map(row => `${row.rate_card_id}\n${row.model}`)), added = new Set();
    for (const v of body.versions || []) {
      const fields = ['id', 'rate_card_id', 'model', 'currency', 'pricing_mode', ...COSTS, ...CREDITS, 'per_call_credit', 'margin_multiplier', 'effective_from_secs'];
      const missing = [...fields.filter(field => v[field] === undefined || v[field] === null),
        ...(v.official ? [...OFFICIAL, 'price_multiplier', 'cost_multiplier', 'usd_cny', 'credit_face_value_cny'].filter(field => !number(v.official[field])) : [])][0];
      if (missing) return malformed(`missing field \`${missing}\``);
      if (v.official?.cost_basis_usd_per_m != null && !(Array.isArray(v.official.cost_basis_usd_per_m) && v.official.cost_basis_usd_per_m.length === 4 && v.official.cost_basis_usd_per_m.every(number))) return malformed('invalid length for `cost_basis_usd_per_m`');
      if (!text(v.id, 128) || !text(v.model, 256) || !positive(v.margin_multiplier) || !COSTS.every(field => number(v[field]) && v[field] >= 0 && v[field] <= 1000000)
        || ![...CREDITS, 'per_call_credit'].every(field => Number.isSafeInteger(v[field]) && v[field] >= 0 && v[field] <= 1000000000000) || !config.rate_cards.some(card => card.id === v.rate_card_id))
        return refuse('Invalid pricing; retroactive publication forbidden');
      if (v.official) {
        // `<provider>/<upstream model>`: what that route costs, charging nothing.
        const routeCost = providers.some(p => v.model.startsWith(`${p.id}/`));
        const problem = officialMismatch(v, v.official, routeCost);
        if (problem) return refuse(`${problem}: ${v.model}`);
      }
      let at = v.effective_from_secs;
      if (at < t) {
        // A model's first price may start now, and so may an official one when this publication changes what it is computed at.
        if (priced.has(`${v.rate_card_id}\n${v.model}`) && !(repriced && v.official)) return refuse('Invalid pricing; retroactive publication forbidden');
        at = t;
      }
      if (versions.some(row => row.id === v.id || (row.rate_card_id === v.rate_card_id && row.model === v.model && row.effective_from_secs === at))) return refuse('Published prices immutable; use new ID and timestamp');
      versions = [...versions, {...v, effective_from_secs: at}];
      added.add(v.id);
    }
    // Every official price in force or scheduled is at the face value and rate in force: those this publication adds, and after it changes either, all of them.
    const live = v => v.effective_from_secs > t || !versions.some(w => w.rate_card_id === v.rate_card_id && w.model === v.model && w.effective_from_secs > v.effective_from_secs && w.effective_from_secs <= t);
    const stale = [...new Set(versions.filter(v => live(v) && v.official && (repriced || added.has(v.id)) && (v.official.credit_face_value_cny !== settings.credit_face_value_cny || v.official.usd_cny !== officialRate(settings))).map(v => v.model))];
    if (stale.length) return refuse(`Official pricing is at a stale face value or rate: ${stale.join(', ')}`);
    const groupsAfter = body.groups ? [...config.groups.filter(row => !body.groups.some(next => next.id === row.id)), ...body.groups] : config.groups;
    const most = values => values.reduce((max, value) => Math.max(max, Number(value ?? 1)), 1);
    if (most(versions.filter(live).map(v => v.margin_multiplier)) * most(groupsAfter.map(g => g.margin_multiplier)) * most(models.map(m => m.credit_multiplier)) > 100) return refuse('Margins and model multipliers combine to more than 100x');
    config.settings = settings;
    config.versions = versions;
    if (body.rate_cards) config.rate_cards = [...config.rate_cards.filter(row => !body.rate_cards.some(next => next.id === row.id)), ...body.rate_cards];
    config.groups = groupsAfter;
    config.models = models;
    if (nextPlans) config.plans = nextPlans;
    config.revision = `fixture-rev-${Number(config.revision.split('-').pop()) + 1}`;
    return {status: 200, body: {success: true, config: configView()}};
  };
  let authenticated = false, deadline = 0, loginAt = 0;
  const IDLE = 1800, MAX = 8 * 3600;
  const nowSecs = () => Math.floor(Date.now() / 1000);
  return {writes, cards, traces, providers, keys, config, storage, ledger, notices, publish, expire() {authenticated=false;}, get sessionDeadline() {return deadline;}, async handle(req,res) {
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
    if(endpoint==='stats') return reply({success:true,stateBytes:storage.bytes,stateWarningBytes:storage.warning,stateCeilingBytes:storage.ceiling,
      lastSavedAtSecs:storage.savedAt,persistenceReady:!storage.persistenceError,persistenceError:storage.persistenceError,totalCards:cards.length,activeCards:2,unactivatedCards:1,frozenCards:1,bannedCards:1,totalCredits:12000000000,usedCredits:1500000000,remainingCredits:10500000000,totalPoints:12000,usedPoints:1500,remainingPoints:10500,activity:activity()});
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
    if(endpoint==='traces') {
      // As the server searches: [fromSecs, toSecs), the card, the model asked for, a provider that answered or was attempted, the status; newest first, with totals over every match.
      const bound=name=>{const value=url.searchParams.get(name);return value===null?undefined:/^\d+$/.test(value)?Number(value):NaN;};
      const from=bound('fromSecs'),to=bound('toSecs');
      if(Number.isNaN(from)||Number.isNaN(to))return reply({success:false,error:'fromSecs and toSecs must be whole seconds'},400);
      if(from!==undefined&&to!==undefined&&from>=to)return reply({success:false,error:'fromSecs must be before toSecs'},400);
      const text=name=>(url.searchParams.get(name)??'').trim()||undefined;
      const status=text('status');
      if(status&&!['success','error','client_aborted','in_progress'].includes(status))return reply({success:false,error:'status must be success, error, client_aborted or in_progress'},400);
      const cardId=text('cardId')??text('card_id'),model=text('model'),provider=text('provider'),raw=url.searchParams.get('limit')??'';
      const limit=Math.min(500,Math.max(1,/^\d+$/.test(raw)?Number(raw):100));
      const found=traces.filter(t=>(from===undefined||t.ts>=from)&&(to===undefined||t.ts<to)&&(!cardId||t.card_id===cardId)&&(!model||t.exposed_model===model)
        &&(!provider||t.provider_id===provider||(t.attempt_chain??[]).some(attempt=>attempt.provider_id===provider))&&(!status||t.status===status));
      const totals={count:found.length,failures:found.filter(t=>t.status==='error').length,creditsCharged:found.reduce((sum,t)=>sum+(t.credits_charged??0),0),costMicroCny:found.reduce((sum,t)=>sum+(t.provider_cost_micro_cny??0),0)};
      const page=found.slice(0,limit);
      return reply({success:true,count:page.length,traces:page,totals});
    }
    if(endpoint==='traces/content') {const record=traceContent(url.searchParams.get('invocation_id'));return record?reply(record):reply({__type:'ResourceNotFoundException',message:'没有这次请求的内容（只保留 24 小时）'},404);}
    if(endpoint==='commercial-config') {
      if(req.method==='POST') {
        // CommercialUpdate denies unknown fields.
        assert.ok(Object.keys(body).every(key=>['expected_revision','reason','settings','groups','models','rate_cards','versions','removed_models','cancelled_versions','plans','removed_plans'].includes(key)));
        const result=publish(body);return reply(result.body,result.status);
      }
      return reply({success:true,config:configView()});
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
    // 公告, as the server keeps them: shown from a start to an end (or until withdrawn), to every
    // customer or the cards of some groups, edited in place with each edit kept.
    const t=Math.floor(Date.now()/1000),AHEAD=3650*86400;
    const noticeFail=(message,status=400)=>reply({success:false,error:message},status);
    const windowProblem=(start,end)=>start>t+AHEAD?'startsAtSecs must be within 3650 days':end!=null&&(end<=start||end>t+AHEAD)?'endsAtSecs must be after the start and within 3650 days':null;
    const audienceOf=ids=>{if(ids.length>50)return {problem:'audience names at most 50 groups'};const list=[];
      for(const id of ids.map(value=>String(value).trim())){if(!config.groups.some(group=>group.id===id))return {problem:`Unknown group in audience: ${id}`};if(!list.includes(id))list.push(id);}return {list};};
    const validText=(value,max)=>typeof value==='string'&&!!value.trim()&&[...value.trim()].length<=max;
    if(endpoint==='announcements'&&req.method==='GET') {
      const all=url.searchParams.get('all')==='true';
      return reply({success:true,announcements:notices.filter(notice=>all||(notice.enabled&&(notice.expires_at==null||t<notice.expires_at))).sort((a,b)=>b.created_at-a.created_at).map(notice=>noticeView(notice,t))});
    }
    if(endpoint==='announcements') {
      if(!validText(body.title,256)||!validText(body.content,20000)||(body.ttlSecs!=null&&!(body.ttlSecs>=60&&body.ttlSecs<=31*86400)))return reply({__type:'InvalidRequestException',message:'announcement fields are invalid'},400);
      const start=Math.max(body.startsAtSecs??t,t);
      if(body.endsAtSecs!=null&&body.ttlSecs!=null)return noticeFail('Give at most one of endsAtSecs and ttlSecs');
      const end=body.endsAtSecs??(body.ttlSecs!=null?start+body.ttlSecs:null);
      const problem=windowProblem(start,end);if(problem)return noticeFail(problem);
      const audience=audienceOf(body.audience??[]);if(audience.problem)return noticeFail(audience.problem);
      const notice={id:`ann-${Date.now()}${notices.length}`,title:body.title,content:body.content,level:['warning','critical'].includes(body.level)?body.level:'info',enabled:true,created_at:t,
        ...(start>t?{starts_at:start}:{}),expires_at:end,audience:audience.list,edits:[]};
      notices.push(notice);return reply({success:true,announcement:noticeView(notice,t)});
    }
    if(endpoint==='announcements/edit') {
      const fields=['id','title','content','level','startsAtSecs','endsAtSecs','ttlSecs','audience'],unknown=Object.keys(body).find(key=>!fields.includes(key));
      if(unknown)return noticeFail(`Invalid request body: unknown field \`${unknown}\`, expected one of ${fields.map(field=>`\`${field}\``).join(', ')}`);
      if(!validText(body.id,128))return noticeFail('announcement id is required');
      if((body.title!==undefined&&!validText(body.title,256))||(body.content!==undefined&&!validText(body.content,20000)))return noticeFail('title must be 1 to 256 characters and content 1 to 20000');
      if(body.level!==undefined&&!['info','warning','critical'].includes(body.level))return noticeFail('level must be info, warning or critical');
      if('endsAtSecs' in body&&body.ttlSecs!=null)return noticeFail('Give at most one of endsAtSecs and ttlSecs');
      if(body.ttlSecs!=null&&!(body.ttlSecs>=60&&body.ttlSecs<=31*86400))return noticeFail('ttlSecs must be between 60 and 2678400');
      const audience=body.audience===undefined?null:audienceOf(body.audience);if(audience?.problem)return noticeFail(audience.problem);
      const notice=notices.find(item=>item.id===body.id.trim());
      if(!notice)return noticeFail('announcement not found',404);
      if(!notice.enabled)return noticeFail('A withdrawn announcement cannot be edited',409);
      const startOf=item=>item.starts_at??item.created_at,next={...notice};
      for(const field of ['title','content','level'])if(body[field]!==undefined)next[field]=body[field];
      if(body.startsAtSecs!=null&&body.startsAtSecs!==startOf(notice))next.starts_at=Math.max(body.startsAtSecs,t);
      if('endsAtSecs' in body)next.expires_at=body.endsAtSecs;
      if(body.ttlSecs!=null)next.expires_at=startOf(next)+body.ttlSecs;
      if(audience)next.audience=audience.list;
      const problem=windowProblem(startOf(next),next.expires_at);if(problem)return noticeFail(problem);
      const changed=[['title',next.title!==notice.title],['content',next.content!==notice.content],['level',next.level!==notice.level],['starts_at',startOf(next)!==startOf(notice)],
        ['expires_at',next.expires_at!==notice.expires_at],['audience',JSON.stringify(next.audience)!==JSON.stringify(notice.audience)]].filter(([,change])=>change).map(([field])=>field);
      if(changed.length)Object.assign(notice,next,{edits:[...notice.edits,{operator:'admin',at_secs:t,changed}]});
      return reply({success:true,announcement:noticeView(notice,t)});
    }
    if(endpoint==='announcements/withdraw') {
      const notice=typeof body.id==='string'?notices.find(item=>item.id===body.id):undefined;
      if(!body.id)return reply({__type:'InvalidRequestException',message:'announcement id is required'},400);
      if(!notice||!notice.enabled)return reply({__type:'ResourceNotFoundException',message:'announcement not found'},404);
      notice.enabled=false;return reply({success:true,id:notice.id});
    }
    if(endpoint==='cards/batch') {
      // As the server issues: from a plan (planId, or its older name templateId), refused as InvalidRequestException.
      const invalid=message=>reply({__type:'InvalidRequestException',message},400);
      assert.equal(body.maxDevices,1); assert.equal('creditTotal' in body,false);
      if(!(body.count>=1&&body.count<=1000))return invalid('count must be between 1 and 1000');
      if(!body.groupId)return invalid('Select an issuance group explicitly');
      if(body.planId&&body.templateId&&body.planId!==body.templateId)return invalid('planId and templateId name different plans');
      const planId=body.planId??body.templateId??'standard-monthly';
      const plan=catalog().find(p=>p.id===planId)??(planId==='standard-monthly'?catalog().find(p=>p.id==='tier-2000'):undefined);
      if(!plan)return invalid('unknown plan');
      if(!plan.on_sale)return invalid('plan is not on sale');
      if(plan.max_devices!==1)return invalid('cards have one device; issue from a plan with max_devices 1');
      const group=config.groups.find(g=>g.id===body.groupId);
      if(!group||group.issuance_enabled===false)return invalid("issuance requires an enabled group, the plan's credits, and maxDevices=1");
      const kept={id:plan.id,name:plan.name,points:plan.points,priceMicroCny:Math.round(plan.price_cny*1e6),validityDays:plan.validity_days,maxDevices:plan.max_devices,concurrency:plan.concurrency,kiroPlanType:plan.kiro_plan_type};
      const t=Math.floor(Date.now()/1000);
      const generated=Array.from({length:body.count},(_,i)=>({cardId:`fixture-issued-${cards.length+i}`,rawCode:`FIXTURE-NOT-VALID-${plan.points}-${i}`,groupId:body.groupId,creditTotal:plan.points*1000000,maxDevices:1,virtualPlanName:plan.name,status:'unactivated',planId:plan.id,plan:kept}));
      generated.forEach(c=>cards.push({id:c.cardId,codeRecoverable:true,status:c.status,creditTotal:c.creditTotal,creditUsed:0,availableCredits:c.creditTotal,pointsTotal:plan.points,pointsAvailable:plan.points,boundDevices:[],maxDevices:1,groupId:c.groupId,note:body.note,
        activationDurationSecs:plan.validity_days*86400,issuedAt:t,plan:kept}));
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