'use strict';
const $ = id => document.getElementById(id);
const tokenKey = 'kiro-bridge-token';
let token = new URLSearchParams(location.hash.slice(1)).get('token') || '';
try { if (token) sessionStorage.setItem(tokenKey, token); else token = sessionStorage.getItem(tokenKey) || ''; } catch (_) {}
history.replaceState(null, '', location.pathname + location.search);
// Card secrets remain in memory until activation. Persistent credentials belong to the native bridge.
let verifiedCard = null, authorizationInfo = null, localStatus = {}, diagnostic = null;
let currentView = 'connect', busy = false, mutationUncertain = false, usageData = null, usageUnit = 'points';
let diagnosticTime = '', memorySamples = [], lastSample = 0;
const timeoutMs = 125000;
const finite = value => typeof value === 'number' && Number.isFinite(value) && value >= 0;
const number = value => finite(value) ? value.toLocaleString('zh-CN', {maximumFractionDigits: 6}) : '—';
const plans = ['PRO', 'PRO+', 'PRO Max', 'Power'];
const plan = () => plans.includes(authorizationInfo?.virtualPlanName) ? authorizationInfo.virtualPlanName : '套餐待查询';
const configured = () => localStatus.authenticated === true && localStatus.has_snapshot === true;
const expired = () => authorizationInfo?.isExpired === true || (finite(authorizationInfo?.validUntil) && authorizationInfo.validUntil * 1000 <= Date.now());
const native = () => window.pywebview?.api;
function notice(text, error = false) { $('message').textContent = text; $('notice').classList.toggle('error', error); $('notice').hidden = false; }
function safeError(error) {
  // Never echo raw bridge errors: they can contain card keys, addresses or upstream credentials.
  const text = String(error?.message || error);
  if (/expired|过期|到期/i.test(text)) return '卡密已到期或不可用，请重新验证或更换卡密。';
  if (/running|close Kiro|terminate|正在运行/i.test(text)) return '请先保存文件并退出 Kiro，再重试。';
  if (/certificate|TLS|hostname|证书/i.test(text)) return '证书校验失败，请在连接诊断中检查。不要关闭证书校验。';
  if (/Timeout|超时/i.test(text)) return '连接超时，操作结果尚不确定。请检查诊断，不要重复写入配置。';
  if (/session|Unauthorized|403|会话/i.test(text)) return '本地会话已失效，请重新打开 Superkiro。';
  if (/card|卡密/i.test(text)) return '卡密验证失败，请检查输入及网络后重试。';
  return '无法完成请求，请检查本地服务和网络后重试。';
}
async function api(path, method = 'GET', data) {
  if (!token) throw new Error('本地会话缺失');
  let response;
  try { response = await fetch(path, {method, headers: {'X-Kiro-Session-Token': token, 'Content-Type': 'application/json'}, body: data === undefined ? undefined : JSON.stringify(data), signal: AbortSignal.timeout(timeoutMs)}); }
  catch (error) { if (method === 'POST' && ['/api/activate', '/api/restore', '/api/unbind'].includes(path)) mutationUncertain = true; throw error; }
  let result;
  try { result = await response.json(); } catch (_) { if (method === 'POST') mutationUncertain = true; throw new Error('无效响应'); }
  if (!response.ok || result.success === false) throw new Error(response.status === 403 ? 'Unauthorized session' : result.error || '请求失败');
  return result;
}
function updateControls() {
  document.querySelectorAll('button').forEach(button => {
    const navigation = button.dataset.view || button.classList.contains('text') && !['trim-memory'].includes(button.id) || button.closest('.window-controls') || button.id === 'dismiss-message';
    button.disabled = busy && !navigation;
  });
  $('takeover').disabled = busy || mutationUncertain;
  $('remember-card').disabled = true;
  $('window-maximize').hidden = localStatus.platform !== 'darwin' && !native()?.maximize;
  $('window-maximize').disabled = !native()?.maximize;
  document.body.classList.toggle('working', busy);
  $('activation-form').setAttribute('aria-busy', String(busy));
}
async function operation(action) {
  if (busy) return;
  busy = true; updateControls();
  try { await action(); } catch (error) { notice(safeError(error), true); }
  finally { busy = false; updateControls(); }
}
function fitWindow() {
  const name = currentView === 'connect' ? 'connect' : ['doctor', 'report', 'restore-failed'].includes(currentView) ? 'doctor' : ['usage', 'settings'].includes(currentView) ? currentView : 'status';
  if (native()?.screen) Promise.resolve(native().screen(name, token)).catch(() => {});
}
function view(name) {
  if (!$(name)?.matches('main > section')) return;
  currentView = name;
  document.body.dataset.screen = name;
  document.querySelectorAll('main > section').forEach(section => section.hidden = section.id !== name);
  $('navigation').hidden = name === 'connect';
  const active = ['report', 'restore-failed'].includes(name) ? 'doctor' : ['missing', 'connecting', 'expired'].includes(name) ? 'status' : name;
  document.querySelectorAll('nav button').forEach(button => { if (button.dataset.view === active) button.setAttribute('aria-current', 'page'); else button.removeAttribute('aria-current'); });
  fitWindow();
  window.scrollTo(0, 0);
}
function navigate(name) {
  if (name === 'status') name = busy && document.body.dataset.connecting === 'true' ? 'connecting' : expired() ? 'expired' : localStatus.kiro_installed === false ? 'missing' : 'status';
  view(name);
  if (name === 'usage') void loadUsage();
  if (name === 'settings') void sampleMemory();
}
function gateway() {
  const value = $('gateway-input').value.trim();
  if (!value) return '';
  let url; try { url = new URL(value); } catch (_) { throw new Error('网关地址无效'); }
  if (!(url.protocol === 'https:' || url.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)) || url.username || url.password || url.search || url.hash || /[\s\\"'`]/.test(value)) throw new Error('网关地址无效');
  return value.replace(/\/+$/, '');
}
function confirmAction(title, detail, label, needsCard = false, tray = false) {
  if ($('confirm-dialog').open) return Promise.resolve(null);
  $('confirm-title').textContent = title; $('confirm-detail').textContent = detail; $('confirm-submit').textContent = label;
  $('confirm-card').value = ''; $('confirm-card').hidden = !needsCard; $('confirm-card').required = needsCard; $('confirm-label').hidden = !needsCard;
  // No tray capability is assumed. Closing currently minimizes instead of destroying the native process.
  $('confirm-tray').hidden = !tray;
  return new Promise(resolve => {
    let result = null;
    const opener = document.activeElement;
    $('confirm-form').onsubmit = event => { event.preventDefault(); result = {card: $('confirm-card').value.trim()}; if (needsCard && !result.card) return; $('confirm-dialog').close(); };
    $('confirm-cancel').onclick = () => $('confirm-dialog').close();
    $('confirm-tray').onclick = () => { result = {tray: true}; $('confirm-dialog').close(); };
    $('confirm-dialog').onclose = () => { $('confirm-card').value = ''; resolve(result); opener?.focus(); };
    $('confirm-dialog').showModal(); $('confirm-cancel').focus();
  });
}
function renderAuthorization() {
  const info = authorizationInfo;
  const expiry = finite(info?.validUntil) ? new Date(info.validUntil * 1000).toLocaleDateString('zh-CN') + ' 到期' : info?.status === 'unactivated' ? '首次启用后计时' : '有效期待查询';
  $('plan-summary').textContent = `${plan()} / ${expiry}`;
  $('settings-plan').textContent = plan(); $('expired-plan').textContent = plan();
  $('expired-detail').textContent = `${expiry} · 剩余积分 ${number(info?.remainingPoints)}（当前不可用）`;
  $('total-points').textContent = number(info?.totalPoints); $('remaining-points').textContent = number(info?.remainingPoints);
  $('balance-bar').value = finite(info?.totalPoints) && info.totalPoints > 0 && finite(info?.remainingPoints) ? Math.min(1, info.remainingPoints / info.totalPoints) : 0;
}
function renderStatus() {
  document.body.dataset.platform = ['darwin', 'win32', 'linux'].includes(localStatus.platform) ? localStatus.platform : 'win32';
  const ready = configured() && localStatus.model_service_available === true;
  $('connection-state').textContent = ready ? '● 连接正常' : configured() ? '◉ 配置已应用 · 待验证' : '◯ 未连接';
  $('overview-title').textContent = ready ? 'Kiro 已就绪' : configured() ? '连接配置已应用' : '连接你的 Kiro';
  $('overview-subtitle').textContent = ready ? 'Kiro 模型服务已通过验证' : configured() ? '请在 Kiro 中确认模型列表与真实对话可用' : localStatus.kiro_installed ? '已检测到 Kiro · 等待启用' : '尚未检测到 Kiro';
  $('takeover').textContent = configured() ? localStatus.process_state === 'Running' ? 'Kiro 已在运行' : '打开 Kiro ↗' : '启用连接';
  $('overview-secondary').textContent = configured() ? '还原 Kiro 配置' : '检查安装位置';
  $('overview-secondary').removeAttribute('data-view');
  $('activation-note').hidden = configured();
  $('overview-secondary').onclick = () => configured() ? void restore('restore') : navigate('settings');
  document.querySelector('.overview-wave').classList.toggle('ready', ready);
  document.querySelectorAll('.overview-wave i').forEach((bar, i) => bar.style.height = ready ? `${5 + 42 * Math.pow(Math.sin(i / 6), 6)}px` : '4px');
  $('install-state').textContent = localStatus.kiro_installed ? `已自动识别${localStatus.kiro_version ? ' · ' + localStatus.kiro_version : ''}` : '未检测到安装';
  $('install-path').textContent = localStatus.kiro_path || localStatus.install_path || (localStatus.kiro_installed ? '已检测到 Kiro，桥接暂未提供完整路径' : '尚未选择 Kiro 应用');
  $('close-behavior').textContent = '最小化（托盘暂未接入）';
  $('window-close').title = '最小化（托盘暂未接入）'; $('window-close').setAttribute('aria-label', '最小化窗口');
  renderAuthorization(); updateControls();
}
async function status() {
  localStatus = await api('/api/status');
  if (localStatus.authorization && typeof localStatus.authorization === 'object') authorizationInfo = localStatus.authorization;
  renderStatus(); return localStatus;
}
$('activation-form').onsubmit = event => {
  event.preventDefault();
  void operation(async () => {
    verifiedCard = null; $('login-error').hidden = true; $('card-input-field').removeAttribute('aria-invalid');
    const card = $('card-input-field').value.trim();
    if (!card) return;
    try {
      const result = await api('/api/verify-card', 'POST', {gateway_url: gateway(), card_key: card});
      if (result.success !== true || !result.authorization) throw new Error('卡密验证失败');
      authorizationInfo = result.authorization; verifiedCard = {card, gateway: result.gateway_url || gateway()};
      $('card-input-field').value = ''; renderAuthorization();
      $('balance-time').textContent = '验证时余额 · ' + new Date().toLocaleTimeString('zh-CN', {hour:'2-digit', minute:'2-digit'});
      try { await status(); } catch (_) { notice('卡密已验证；本地状态读取失败，请重新检测。', true); }
      navigate('status');
    } catch (error) {
      $('login-error').textContent = safeError(error); $('login-error').hidden = false;
      $('card-input-field').setAttribute('aria-invalid', 'true'); $('login-submit').textContent = '重新登录';
    }
  });
};
async function activate() {
  if (busy || mutationUncertain) { if (mutationUncertain) notice('上次操作结果未确认，请检查诊断并重新打开客户端后再操作。', true); return; }
  if (!verifiedCard) { view('connect'); notice('请重新验证卡密，再手动启用连接。'); return; }
  if (expired()) { view('expired'); return; }
  if (authorizationInfo?.remainingPoints === 0) { await confirmAction('余额不足', '当前积分不足。请联系卡密提供方或在自助门户充值后重试。', '知道了'); return; }
  if (!await confirmAction('启用连接？', '将备份原始配置、安装本机证书并重启 Kiro。请先保存文件。只有你确认后才会修改本机配置。', '启用连接')) return;
  await operation(async () => {
    document.body.dataset.connecting = 'true'; view('connecting');
    const started = Date.now();
    const timer = setInterval(() => { $('elapsed').textContent = `已等待 ${Math.floor((Date.now() - started) / 1000)} 秒 · 请求超时阈值 ${timeoutMs / 1000} 秒`; }, 1000);
    try {
      const result = await api('/api/activate', 'POST', {gateway_url: verifiedCard.gateway, card_key: verifiedCard.card, close_kiro_confirmed: true});
      if (result.success !== true) throw new Error('无法确认结果');
      verifiedCard = null;
      await status(); view('status'); notice('连接配置已应用。请在 Kiro 中验证真实对话。');
      void loadUsage();
    } catch (error) {
      view('doctor'); $('doctor-title').textContent = mutationUncertain ? '连接超时或结果未确认' : '连接未完成';
      $('doctor-summary').textContent = safeError(error); $('doctor-banner').classList.add('alert');
      notice(safeError(error), true);
    } finally { clearInterval(timer); document.body.dataset.connecting = 'false'; }
  });
}
$('takeover').onclick = () => {
  if (!configured()) return void activate();
  if (localStatus.process_state === 'Running') return notice('Kiro 已在运行，请切换至 Kiro 窗口。');
  void operation(async () => { const result = await api('/api/launch', 'POST', {}); if (result.success !== true) throw new Error('启动未确认'); await status(); notice('已发送 Kiro 启动请求。'); });
};
$('repair').onclick = activate;
async function restore(intent) {
  if (busy) return;
  if (mutationUncertain) { navigate('doctor'); notice('上次写入结果未确认，请完成诊断并重新打开客户端后再还原。', true); return; }
  const titles = {restore:'还原 Kiro 配置？', switch:'切换卡密？', exit:'退出 Superkiro？', unbind:'解除设备绑定？'};
  const labels = {restore:'还原配置', switch:'还原并切换卡密', exit:'还原并退出', unbind:'解除绑定并还原'};
  const answer = await confirmAction(titles[intent], '请先保存文件并关闭 Kiro。将只恢复 Superkiro 修改的配置与凭据。失败时保留当前窗口和恢复入口。', labels[intent], intent === 'unbind');
  if (!answer) return;
  await operation(async () => {
    try {
      // Always ask the native bridge to restore: a partial activation can have a backup without authentication.
      const result = await api(intent === 'unbind' ? '/api/unbind' : '/api/restore', 'POST', intent === 'unbind' ? {card_key:answer.card} : {});
      if (result.success !== true) throw new Error('恢复未确认');
      verifiedCard = null; authorizationInfo = null; usageData = null; memorySamples = [];
      $('card-input-field').value = ''; localStatus = {}; renderStatus(); view('connect');
      if (intent === 'exit') { if (!native()?.close) { notice('配置已恢复；当前预览环境不能退出原生程序。'); return; } await native().close(token); }
      else notice('Kiro 配置已还原。重新连接需要验证卡密。');
    } catch (error) { $('restore-error').textContent = safeError(error); view('restore-failed'); }
  });
}
let usageLoading = false;
async function loadUsage() {
  if (usageLoading) return;
  usageLoading = true;
  try {
    const data = await api('/api/usage');
    const credit = data.usage?.usageBreakdownList?.find(item => item.dimensionType === 'CREDIT');
    if (!finite(credit?.currentUsageWithPrecision) || !finite(credit?.usageLimitWithPrecision)) throw new Error('用量响应无效');
    usageData = data;
    authorizationInfo = {...authorizationInfo, totalPoints:credit.usageLimitWithPrecision, remainingPoints:Math.max(0, credit.usageLimitWithPrecision - credit.currentUsageWithPrecision)};
    // Only the gateway's explicit virtual plan name is a display plan; never infer an official entitlement.
    if (plans.includes(data.usage?.virtualPlanName)) authorizationInfo.virtualPlanName = data.usage.virtualPlanName;
    renderAuthorization(); $('balance-time').textContent = '余额更新 · ' + new Date().toLocaleTimeString('zh-CN', {hour:'2-digit', minute:'2-digit'});
    $('settled-points').textContent = number(credit.currentUsageWithPrecision);
    $('settled-tokens').textContent = number(data.settledUsage?.totalTokens);
    $('today-points').textContent = finite(data.settledUsage?.todayPoints) ? number(data.settledUsage.todayPoints) + ' 积分' : '—';
    $('today-tokens').textContent = number(data.settledUsage?.todayTokens);
    const empty = credit.currentUsageWithPrecision === 0 && !data.settledUsage?.daily?.length;
    $('usage-content').hidden = empty; $('usage-empty').hidden = !empty;
    $('usage-empty-title').textContent = '还没有用量记录'; $('usage-empty-detail').textContent = '首次调用结算后，这里会显示积分与 Tokens 消耗。';
    $('usage-time').textContent = new Date().toLocaleTimeString('zh-CN', {hour:'2-digit', minute:'2-digit'});
    renderUsageChart();
  } catch (_) {
    $('usage-content').hidden = true; $('usage-empty').hidden = false;
    $('usage-empty-title').textContent = '用量加载失败'; $('usage-empty-detail').textContent = '无法读取已结算用量。请检查连接后点击右上角刷新。';
    $('balance-time').textContent = '余额刷新失败 · 上次快照';
  } finally { usageLoading = false; }
}
function renderUsageChart() {
  $('usage-chart').replaceChildren(); $('usage-models').replaceChildren();
  const details = usageData?.settledUsage;
  const days = Array.isArray(details?.daily) ? details.daily.slice(-7) : [];
  const key = {points:'points', tokens:'tokens', usd:'usd'}[usageUnit];
  const valid = days.length && days.every(day => finite(day[key]));
  if (usageUnit === 'usd' && !finite(details?.referencePrice) || !valid) {
    $('usage-chart').textContent = usageUnit === 'usd' ? '美元参考价未配置，暂不提供估算。' : '桥接暂未提供每日已结算明细。';
  } else {
    const max = Math.max(1, ...days.map(day => day[key]));
    for (const day of days) { const bar = document.createElement('div'); bar.className = 'chart-column'; bar.style.height = `${day[key] / max * 100}%`; const value = document.createElement('span'), date = document.createElement('small'); value.textContent = number(day[key]); date.textContent = String(day.date || '').slice(-5); bar.append(value,date); $('usage-chart').append(bar); }
  }
  for (const model of Array.isArray(details?.models) ? details.models : []) {
    const row = document.createElement('div'); row.className = 'row';
    for (const text of [String(model.name || '未命名模型'), number(model.tokens), number(model.points)]) { const cell = document.createElement('span'); cell.textContent = text; row.append(cell); } $('usage-models').append(row);
  }
  if (!$('usage-models').children.length) $('usage-models').textContent = '桥接暂未提供按模型明细。';
}
const itemNames = {'Kiro Installation':'Kiro 安装位置', 'Gateway Connectivity':'网关连通性', 'Settings Configuration':'连接配置', 'Extension Patch':'扩展补丁完整性', 'Authentication Token':'本地授权凭据'};
const levelNames = {pass:'通过', warning:'需检查', fail:'失败'};
async function runDoctor() {
  await operation(async () => {
    $('doctor-title').textContent = '正在检查'; $('doctor-summary').textContent = '等待本地桥返回检查结果。';
    try {
      const data = await api('/api/doctor?gateway_url=' + encodeURIComponent(gateway()));
      if (!Array.isArray(data.items) || !data.items.length) throw new Error('无诊断结果');
      // Allowlist only names and levels, not arbitrary detail strings (which can contain secrets).
      diagnostic = data.items.map((item, i) => ({name:itemNames[item.name] || `检查项 ${i + 1}`, level:levelNames[item.level] ? item.level : 'unknown'}));
      diagnosticTime = new Date().toLocaleString('zh-CN');
      $('doctor-items').replaceChildren();
      for (const item of diagnostic) { const row = document.createElement('li'); row.className = item.level; for (const value of [item.name, levelNames[item.level] || '未知']) { const span = document.createElement('span'); span.textContent = value; row.append(span); } $('doctor-items').append(row); }
      const failed = diagnostic.some(item => item.level !== 'pass');
      $('doctor-banner').classList.toggle('alert', failed);
      $('doctor-title').textContent = failed ? '连接需要检查' : '基础检查已通过';
      $('doctor-summary').textContent = `${diagnostic.filter(item => item.level === 'pass').length}/${diagnostic.length} 项通过 · 请在 Kiro 中验证模型对话。`;
      $('doctor-time').textContent = '检测于 ' + diagnosticTime;
    } catch (error) { $('doctor-title').textContent = '诊断未完成'; $('doctor-summary').textContent = safeError(error); throw error; }
  });
}
function reportText() {
  return ['Superkiro · 脱敏诊断报告', '检测时间  ' + (diagnosticTime || '尚未检测'), '套餐  ' + plan(), '安装状态  ' + (localStatus.kiro_installed === true ? '已检测到' : '未确认'), ...(diagnostic || []).map(item => item.name + '  ' + (levelNames[item.level] || '未知')), '模型服务  ' + (localStatus.model_service_available === true ? '已验证' : '待验证'), '隐私  不包含卡密、密钥、网关地址、路径或原始日志'].join('\n');
}
let sampling = false;
async function sampleMemory() {
  if (sampling) return;
  sampling = true;
  try {
    const data = await api('/api/memory/sample');
    if (!finite(data.total_memory_mb)) throw new Error('无内存数据');
    const text = `${number(data.total_memory_mb)} MB`;
    $('memory-value').textContent = text + ' · 当前占用'; $('memory-overview').textContent = text;
    $('maintenance-state').textContent = data.automatic_maintenance_enabled === true ? '自动维护已开启' : '自动维护尚未接入';
    $('memory-detail').textContent = '统计 Kiro 相关进程占用；采样于 ' + new Date().toLocaleTimeString('zh-CN') + '。';
    memorySamples.push(data.total_memory_mb); memorySamples = memorySamples.slice(-40); lastSample = Date.now();
    for (const id of ['memory-chart', 'memory-mini']) { $(id).replaceChildren(); for (const sample of memorySamples) { const bar = document.createElement('i'); bar.style.height = Math.max(2, sample / Math.max(1,...memorySamples) * 90) + '%'; $(id).append(bar); } }
  } catch (_) { $('memory-detail').textContent = '内存采样失败；未执行维护，旧采样不代表当前占用。'; }
  finally { sampling = false; }
}
async function pickInstall() {
  if (!native()?.pick_install_path) { notice('当前原生桥尚未提供安装位置选择。请安装 Kiro 后重新检测。', true); return; }
  await operation(async () => { const result = await native().pick_install_path(token); if (!result || result.cancelled) return; if (result.success !== true) throw new Error('路径校验失败'); await status(); navigate('status'); });
}
async function minimize() { if (native()?.minimize) await native().minimize(token); else notice('当前为浏览器预览，无法控制原生窗口。'); }
async function openExternal(url) {
  // External navigation is explicit and never includes card keys or bridge tokens.
  if (!url) { notice('当前桥接未配置自助门户地址，请联系卡密提供方。'); return; }
  let parsed; try { parsed = new URL(url); } catch (_) { return; }
  if (parsed.protocol !== 'https:' || parsed.username || parsed.password || parsed.search || parsed.hash) { notice('外部地址未通过安全检查。', true); return; }
  window.open(parsed.href, '_blank', 'noopener,noreferrer');
}
for (const wave of document.querySelectorAll('.metal-wave')) {
  const login = wave.classList.contains('login-wave'), count = login ? 33 : 57;
  for (let i = 0; i < count; i++) { const bar = document.createElement('i'); const x = (i - (count - 1) / 2) / 8; bar.style.height = login ? `${18 + 108 * Math.exp(-x*x/1.5)}px` : '4px'; wave.append(bar); }
}
document.querySelectorAll('[data-view]').forEach(button => button.onclick = () => navigate(button.dataset.view));
document.querySelectorAll('[data-action]').forEach(button => button.onclick = () => {
  const action = button.dataset.action;
  if (['restore','switch','exit'].includes(action)) return void restore(action);
  if (action === 'pick-install') return void pickInstall();
  if (action === 'detect') return void operation(async () => { await status(); notice(localStatus.kiro_installed ? '已检测到 Kiro。' : '尚未检测到 Kiro。'); if (currentView === 'missing' && localStatus.kiro_installed) navigate('status'); });
  if (action === 'portal') return void openExternal(localStatus.portal_url);
});
document.querySelectorAll('[data-unit]').forEach(button => button.onclick = () => { usageUnit = button.dataset.unit; document.querySelectorAll('[data-unit]').forEach(item => item.setAttribute('aria-pressed', String(item === button))); renderUsageChart(); });
document.querySelectorAll('[data-close]').forEach(button => button.onclick = () => button.closest('dialog').close());
$('window-minimize').onclick = () => void minimize();
$('window-close').onclick = () => void minimize();
$('window-maximize').onclick = () => { if (native()?.maximize) void native().maximize(token); };
$('dismiss-message').onclick = () => $('notice').hidden = true;
$('run-doctor').onclick = runDoctor;
$('refresh-usage').onclick = loadUsage;
$('sample-memory').onclick = sampleMemory;
$('trim-memory').onclick = async () => { if (busy || !await confirmAction('整理工作集？', '仅请求系统整理工作集，可能暂时增加磁盘读取。不会强制终止正在编辑的进程。', '整理工作集')) return; await operation(async () => { await api('/api/memory/trim','POST',{}); await sampleMemory(); notice('工作集整理请求已完成。'); }); };
$('unbind').onclick = () => restore('unbind');
$('preview-report').onclick = () => { $('report-content').textContent = reportText(); view('report'); };
$('copy-report').onclick = () => void operation(async () => { await navigator.clipboard.writeText(reportText()); notice('已复制脱敏报告摘要。'); });
$('export-report').onclick = () => {
  const url = URL.createObjectURL(new Blob([reportText()], {type:'text/plain;charset=utf-8'}));
  const anchor = document.createElement('a'); anchor.href = url; anchor.download = 'Superkiro-diagnostic.txt'; anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
  notice('已发起脱敏报告下载。请确认系统下载结果。');
};
$('connection-settings').onclick = () => $('connection-dialog').showModal();
$('login-help').onclick = () => void confirmAction('登录帮助', '输入卡密后仅查询授权。验证成功仍需手动启用连接。卡密不会写入网页存储；记住卡密功能尚未接入原生凭据库。', '知道了');
$('about').onclick = () => void confirmAction('Superkiro', `桌面 Metal 界面 · ${localStatus.app_version || '版本信息待原生桥提供'}。记住卡密、系统托盘及自动维护暂未接入；关闭按钮仅最小化窗口。`, '知道了');
$('download-kiro').onclick = () => void openExternal('https://kiro.dev/downloads/');
window.addEventListener('pywebviewready', () => { fitWindow(); updateControls(); });
async function heartbeat() { try { await api('/api/heartbeat', 'POST', {}); } catch (_) { notice('本地服务已断开，请重新打开 Superkiro。', true); } }
setInterval(heartbeat, 2000);
setInterval(() => { if (document.hidden || busy || !configured()) return; if (['status','usage'].includes(currentView)) void loadUsage(); if (['status','settings'].includes(currentView) && Date.now()-lastSample > 30000) void sampleMemory(); }, 30000);
view('connect');
void status().then(() => { if (configured()) { navigate('status'); void loadUsage(); void sampleMemory(); } }).catch(error => notice(safeError(error), true));
void heartbeat();
