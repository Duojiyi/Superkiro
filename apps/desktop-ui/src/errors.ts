import type { Status } from './bridge';

export const errorMessages: Record<string, string> = {
  'SK-KIRO-001': '当前 Kiro 版本不受支持，请升级至 1.1.14 或更高版本。',
  'SK-AUTH-001': '卡密或凭据无效，请重新验证。',
  'SK-AUTH-002': '授权已到期，请重新验证或更换卡密。',
  'SK-AUTH-003': '授权被拒绝，请检查卡密状态及设备授权。',
  'SK-AUTH-004': '认证请求过于频繁或暂时锁定，请稍后重试。',
  'SK-BIND-001': '设备绑定不匹配，请先解除原设备绑定。',
  'SK-BIND-002': '换绑仍在冷却期，请等待冷却结束后重试。',
  'SK-BIND-003': '换绑次数已用尽，请联系支持方；等待不会恢复次数。',
  'SK-BIND-004': '云端解绑已成功，但本地清理未完成。请保留备份并还原本地配置，无需重复解绑。',
  'SK-NET-001': '请求超时，请检查网络。',
  'SK-NET-002': '网络连接失败，请检查网络和服务可用性。',
  'SK-NET-003': '证书校验失败，请检查系统时间和受信任证书，不要关闭证书校验。',
  'SK-CONNECT-001': '连接前检查失败，请确认 Kiro 安装及连接设置。',
  'SK-CONNECT-002': '未能关闭 Kiro，请保存文件并手动退出后重试。',
  'SK-CONNECT-003': '连接配置应用失败，请检查文件占用与权限；有待恢复配置时请先还原。',
  'SK-CONNECT-004': 'Kiro 启动失败，请确认安装完整性及配置状态。',
  'SK-CONNECT-005': 'Kiro 仍未关闭，可能正在询问是否保存更改。请回到 Kiro 处理提示；也可以在 Kiro 中选择「文件 > 退出」，Kiro 默认会保留未保存的内容并在下次打开时恢复。然后重试。',
  'SK-CONNECT-006': 'Kiro 在你的另一个 Windows 会话中仍在运行，这里无法关闭它。请到那个会话中关闭 Kiro 后重试。',
  'SK-CONNECT-007': 'Kiro 的一个配置文件（Profile）的设置无法安全修改，本次未改动任何文件。请在 Kiro 中切换到该配置文件，运行「Preferences: Open User Settings (JSON)」修正并保存，然后重试。',
  'SK-CONNECT-008': 'Kiro 正在更新或刚完成更新，本次未修改任何配置。请打开 Kiro 一次，等更新完成后再重试。',
  'SK-CONNECT-009': 'Kiro 的 settings.json 还有其他硬链接，接入会把它们分开，因此未做任何修改。请删除多余的硬链接（或改用符号链接）后重试。',
  'SK-RESTORE-001': '本地配置还原未完成，请保留备份并重试还原。',
  'SK-RESTORE-002': 'Kiro 的扩展文件仍是修改后的版本，且用于还原它的备份已丢失，重试无法还原。请从官网重新安装 Kiro（会替换该文件），再重新还原。',
  'SK-RESTORE-003': 'Kiro 的 settings.json 有语法错误，无法安全修改，本次未改动任何文件。请在 Kiro 中运行「Preferences: Open User Settings (JSON)」（或用记事本打开该文件），修正出错的那一行并保存，然后重试。',
  'SK-LOCAL-001': '本地操作权限不足，请检查文件占用及系统权限。',
  'SK-LOCAL-002': '已有操作正在处理，请等待状态确认，不要重复操作。',
  'SK-LOCAL-003': '系统安全存储操作失败，请检查系统凭据服务。',
  'SK-UPDATE-001': '无法下载新版本，当前版本未改动。请检查网络后重试，或从官网下载新版。',
  'SK-UPDATE-002': '新版本的文件校验未通过，已停止安装，当前版本未改动。请稍后重试，或从官网下载新版。',
  'SK-UPDATE-003': '无法替换当前的程序文件，当前版本未改动。请把 Superkiro 移到你有写入权限的文件夹（例如桌面或“文档”；macOS 请移到“应用程序”）后重新打开，或从官网下载新版。',
  'SK-UPDATE-004': '新版本未能启动，已恢复为当前版本。请稍后重试，或从官网下载新版。',
  'SK-UPDATE-005': '新版本启动时被拦截或等待过久（常见于安全软件询问是否允许运行），当前版本未改动。请在安全软件中允许 Superkiro 后重试。',
  'SK-PREVIEW-001': '当前为浏览器预览，未连接桌面宿主，没有执行本机操作。',
  'SK-UNKNOWN-001': '请求未完成，请复制反馈信息联系支持方。',
};
const stages = ['preflight', 'launch-prepare', 'authenticate', 'close', 'apply', 'launch', 'restore', 'unbind', 'native', 'unknown'];
export class ClientError extends Error {
  constructor(public code: string, public feedback_id: string, public stage: string,
    public outcome: 'failed' | 'partial' | 'unknown', public retry_after_seconds: number | null,
    public occurred_at: string, public line: number | null = null, public column: number | null = null,
    public profile: string | null = null) {
    super(errorMessages[code] + (profile !== null ? ` 出错的配置文件：「${profile}」。` : '') +
      (line !== null && column !== null ? ` 出错位置：第 ${line} 行第 ${column} 列。` : '') +
      (outcome === 'unknown' ? ' 操作结果未确认，请等待状态同步，不要重复修改配置。' : '') +
      (retry_after_seconds !== null ? ` 请在 ${retry_after_seconds} 秒后重试。` : ''));
    this.name = 'ClientError';
  }
}
// Where settings.json has to be fixed, and in which profile; any other value is dropped.
const position = (value: unknown) => typeof value === 'number' && Number.isInteger(value) && value >= 1 && value <= 10_000_000 ? value : null;
const profileName = (value: unknown) => typeof value === 'string' && value.trim() !== '' && [...value].length <= 128 &&
  ![...value].some(c => c.charCodeAt(0) < 0x20 || (c.charCodeAt(0) >= 0x7f && c.charCodeAt(0) < 0xa0)) ? value : null;
const locates = (code: string) => code === 'SK-RESTORE-003' || code === 'SK-CONNECT-007';
export function toClientError(error: unknown): ClientError {
  if (error instanceof ClientError) return error;
  const value = error && typeof error === 'object' ? error as Record<string, unknown> : {};
  const code = typeof value.code === 'string' && Object.hasOwn(errorMessages, value.code) ? value.code : 'SK-UNKNOWN-001';
  const id = typeof value.feedback_id === 'string' && /^[A-Za-z0-9-]{8,96}$/.test(value.feedback_id)
    ? value.feedback_id : `UI-${crypto.randomUUID()}`;
  const rawTime = typeof value.occurred_at === 'string' ? value.occurred_at : '';
  const date = /^\d{10,11}$/.test(rawTime) ? new Date(Number(rawTime) * 1000) : /^\d{4}-\d{2}-\d{2}T[\d:.]+Z$/.test(rawTime) ? new Date(rawTime) : new Date();
  return new ClientError(code, id, typeof value.stage === 'string' && stages.includes(value.stage) ? value.stage : 'unknown',
    value.outcome === 'partial' || value.outcome === 'unknown' ? value.outcome : 'failed',
    typeof value.retry_after_seconds === 'number' && Number.isInteger(value.retry_after_seconds) && value.retry_after_seconds >= 0 && value.retry_after_seconds <= 86400 ? value.retry_after_seconds : null,
    Number.isFinite(date.getTime()) ? date.toISOString() : new Date().toISOString(),
    locates(code) ? position(value.line) : null, locates(code) ? position(value.column) : null, locates(code) ? profileName(value.profile) : null);
}
export function feedbackText(error: ClientError, status: Status): string {
  const version = (value: unknown) => typeof value === 'string' && /^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]{1,40})?$/.test(value) ? value : '未知';
  return ['Superkiro 错误反馈', `错误码：${error.code}`, `反馈编号：${error.feedback_id}`, `发生时间：${error.occurred_at}`,
    `操作阶段：${error.stage}`, `结果：${error.outcome}`, `客户端版本：${version(status.app_version)}`, `Kiro 版本：${version(status.kiro_version)}`,
    '仅含脱敏元数据；反馈编号不是云端请求编号。'].join('\n');
}
