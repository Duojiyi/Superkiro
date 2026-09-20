import { invoke, isTauri } from '@tauri-apps/api/core';
export interface Authorization { virtualPlanName?: string; remainingPoints?: number; totalPoints?: number; validUntil?: number; isExpired?: boolean; status?: string }
export interface Status { gateway_url?: string; authenticated?: boolean; has_snapshot?: boolean; recovery_pending?: boolean; kiro_installed?: boolean; kiro_version?: string; kiro_install_path?: string; process_state?: string; model_service_available?: boolean | null; portal_url?: string; platform?: string; app_version?: string; authorization?: Authorization; tray_available?: boolean; memory_maintenance?: Maintenance }
export interface Usage { usage?: { availableCredits?:number|null; usageBreakdownList?: {dimensionType: string; currentUsageWithPrecision: number; usageLimitWithPrecision: number}[]; virtualPlanName?: string; validUntil?: number; isExpired?: boolean }; settledUsage?: {windowStart?: string|number; windowEnd?: string|number; timezone?: string; totalTokens?: number; todayPoints?: number; todayTokens?: number; referencePrice?: number; daily?: {date: string; points?: number; tokens?: number; usd?: number}[]; models?: {name: string; tokens?: number; points?: number}[]} }
export interface Memory { total_memory_mb?: number; total_process_count?: number; ide_memory_mb?: number; agent_memory_mb?: number; success_count?: number; failed_count?: number }
export const finite = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v) && v >= 0;
export const number = (v: unknown) => finite(v) ? v.toLocaleString('zh-CN', {maximumFractionDigits: 1}) : '—';
export const configured = (s: Status) => s.authenticated === true && s.has_snapshot === true;
export const expired = (a: Authorization | null) => a?.isExpired === true || (finite(a?.validUntil) && a.validUntil * 1000 <= Date.now());
export function safeError(error: unknown) {
  const text = error instanceof Error ? error.message : String(error);
  if(/MacBundleNameUnsupported|macOS.*Kiro\.app/.test(text))return 'macOS 暂仅支持保留官方包名 Kiro.app 的安装，请恢复官方包名后重新选择。';
  const stage = /^\[connection:(preflight|launch-prepare|authenticate|close|apply|launch)\]/.exec(text)?.[1];
  const auth = /\[auth:([a-z-]+)\]/.exec(text)?.[1];
  const authMessages: Record<string,string> = {
    'invalid-card':'卡密或凭据无效，请重新验证。', 'access-denied':'授权被拒绝，请检查卡密状态及权限。',
    expired:'授权已到期，请重新验证或更换卡密。', 'device-binding':'设备绑定不匹配，请先解除原设备绑定。',
    throttled:'请求过于频繁，请稍后重试。', 'locked-out':'认证暂时锁定，请稍后重试。',
    'invalid-request':'认证请求无效，请检查输入。', 'server-error':'授权服务暂时不可用，请稍后重试。',
    'auth-rejected':'网关拒绝授权，请检查卡密和设备状态。',
  };
  if(auth && Object.hasOwn(authMessages,auth)) {
    const retry = /\[retry-after:(\d{1,5})\]/.exec(text)?.[1];
    return (stage ? `[connection:${stage}] ` : '') + authMessages[auth] + (retry && Number(retry)<=86400 ? ` 请在 ${Number(retry)} 秒后重试。` : '');
  }
  if (stage) {
    const messages: Record<string,string> = {
      preflight:'连接前检查失败，请查看诊断中的安装、网关和配置检查结果。',
      'launch-prepare':'Kiro 启动准备失败，请检查安装完整性和本机权限。',
      authenticate:'连接授权失败，请检查卡密有效性、余额及设备绑定状态。',
      close:'未能关闭 Kiro，请保存文件并手动退出 Kiro。',
      apply:'连接配置应用失败，请检查文件占用与权限；如有待恢复配置，请先还原。',
      launch:'未能启动 Kiro，请查看诊断并确认配置状态后再启动。',
    };
    return `[connection:${stage}] ${messages[stage]}` + (/timeout|超时/i.test(text) ? ' 操作结果未确认，请勿重复修改配置。' : '');
  }
  if (/expired|过期|到期/i.test(text)) return '卡密已到期或不可用，请重新验证或更换卡密。';
  if (/running|close Kiro|terminate|正在运行/i.test(text)) return '请先保存文件并退出 Kiro，再重试。';
  if (/No Kiro processes|no process/i.test(text)) return '没有运行中的 Kiro 进程，未执行内存优化。';
  if (/Cannot read|read.*failed|读取/i.test(text)) return '读取本地文件或状态失败，请检查安装路径与文件权限后重试。';
  if (/write|permission|access denied|写入|权限/i.test(text)) return '写入配置失败，请检查文件占用与权限。备份仍需保留。';
  if (/credential|keyring|安全存储/i.test(text)) return '系统安全存储操作失败，请检查系统凭据服务。';
  if (/restore|恢复|还原/i.test(text)) return '还原配置未完成，请保留备份并查看诊断。';
  if (/TLS configuration/i.test(text)) return 'TLS 配置检查失败，请检查受信任证书与 CA 配置。不要关闭证书校验。';
  if (/certificate|TLS|hostname|证书/i.test(text)) return '证书校验失败，请在诊断中检查，不要关闭证书校验。';
  if (/timeout|超时/i.test(text)) return '连接超时，操作结果未确认。请查看诊断，不要重复修改配置。';
  if (/preview/i.test(text)) return '当前为浏览器预览，未连接 Tauri 宿主。没有执行本机操作。';
  return '请求未完成，请检查本地服务、卡密及网络后重试。';
}
export async function api<T>(path: string, method = 'GET', body: object = {}): Promise<T> {
  if (!isTauri()) throw new Error('preview');
  let timer:ReturnType<typeof setTimeout>|undefined;
  const result = await Promise.race([invoke<T & {success?: boolean; error?: string}>('api', {path, method, body}),new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error('timeout')),path==='/api/heartbeat'?5000:method==='GET'?15000:125000);})]).finally(()=>clearTimeout(timer));
  if (result?.success === false) throw new Error(result.error || '请求失败');
  return result;
}
export async function native<T = unknown>(method: string, args: unknown[] = []): Promise<T> {
  if (!isTauri()) throw new Error('preview');
  return invoke<T>('native', {method, args});
}
export function gateway(value: string) {
  value = value.trim(); if (!value) return '';
  const url = new URL(value);
  if (!(url.protocol === 'https:' || url.protocol === 'http:' && ['localhost','127.0.0.1','[::1]'].includes(url.hostname)) || url.username || url.password || url.search || url.hash || /[\s\\"'`]/.test(value)) throw new Error('无效网关');
  return value.replace(/\/+$/, '');
}
export const names: Record<string,string> = {'Kiro Installation':'Kiro 安装位置','Gateway Connectivity':'网关连通性','Settings Configuration':'连接配置','Extension Patch':'扩展补丁完整性','Authentication Token':'本地授权凭据','网关 TLS 代理路径':'网关 TLS 代理路径','真实 IDE 交互验收':'真实 IDE 交互验收'};
export const levels: Record<string,string> = {pass:'通过', warning:'需检查', fail:'失败', unknown:'未知'};
export function sanitizeChecks(items: {name: string; level: string}[]) { return items.map((item,i) => ({name:names[item.name] || `检查项 ${i+1}`, level: Object.hasOwn(levels,item.level) ? item.level : 'unknown'})); }
export type Check = ReturnType<typeof sanitizeChecks>[number];
// Accept only contract fields; never retain or render the host's raw error/path.
export function operationFailureSummary(value: unknown): string {
  if(!value || typeof value!=='object')return '最近失败操作：记录格式不可用。';
  const op=value as Record<string,unknown>;
  if(op.state!=='failed')return '当前操作记录未标记失败，不代表从未发生失败。';
  const stage=typeof op.stage==='string'&&['preflight','launch-prepare','authenticate','close','apply','launch','unknown'].includes(op.stage)?op.stage:'unknown';
  const code=typeof op.code==='string'&&['timeout','tls','auth-rejected','network','permission','unknown'].includes(op.code)?op.code:'unknown';
  const http=typeof op.http_status==='number'&&Number.isInteger(op.http_status)&&op.http_status>=100&&op.http_status<=599?` · HTTP ${op.http_status}`:'';
  const time=typeof op.finished_at==='number'&&Number.isSafeInteger(op.finished_at)&&op.finished_at>0&&op.finished_at<=253402300799?new Date(op.finished_at*1000).toISOString():'未提供';
  return `最近失败操作：阶段 ${stage} · 错误码 ${code}${http} · 完成时间 ${time}（历史记录，不代表当前连接状态）`;
}
export function report(checks: Check[], time: string, status: Status, failure = '') { return ['Superkiro · 脱敏诊断报告', `检测时间  ${time || '尚未检测'}`, `安装状态  ${status.kiro_installed === true ? '已检测到' : '未确认'}`, ...(failure?[failure]:[]), ...checks.map(c=>`${c.name}  ${levels[c.level]}`), `模型服务  ${status.model_service_available === true ? '已验证' : '待验证'}`, '隐私  不包含卡密、密钥、网关地址、路径或原始日志'].join('\n'); }


export interface Maintenance { enabled?: boolean; mode?: 'automatic'|'monitor-only'; threshold_mb?: number; cooldown_seconds?: number; last_sample_mb?: number|null; last_error?: string|null; last_trim?: Memory|null }
export function maintenanceText(value: Maintenance | undefined) {
  const label = !value || typeof value.enabled !== 'boolean' ? '维护状态未知' : !value.enabled ? '自动维护未开启' : value.mode === 'automatic' ? '自动维护已开启' : value.mode === 'monitor-only' ? '仅监测，不自动整理' : '维护模式未知';
  const policy = value?.enabled && value.mode === 'automatic' && finite(value.threshold_mb) && finite(value.cooldown_seconds) ? `超过 ${number(value.threshold_mb)} MB 且冷却 ${number(value.cooldown_seconds)} 秒后才整理；不杀进程、不删缓存。` : '不杀进程、不删缓存；维护结果以实际采样为准。';
  const result = value?.last_error ? '最近维护异常，请重新检测；未确认优化成功。' : value?.last_trim && finite(value.last_trim.success_count) && value.last_trim.success_count > 0 ? `最近一次整理：${number(value.last_trim.success_count)} 成功，${number(value.last_trim.failed_count)} 失败；不代表当前正在维护。` : '尚无已确认的整理结果。';
  return {label, detail: `${policy} ${result}`};
}

export const recoveryPending = (s: Status) => !configured(s) && (s.recovery_pending === true || s.has_snapshot === true);

