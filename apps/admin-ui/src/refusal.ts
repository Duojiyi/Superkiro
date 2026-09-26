// What the server's refusals mean, in the console's words. A refusal (409 for every rule the
// configuration breaks, 400–422 for a malformed request) changed nothing and can be corrected;
// anything else — a timeout, a lost reply, a server error — may have been applied and must be
// checked before trying again. Pure functions, so they can be tested without a browser.

export type PublishOutcome = {ok: true} | {ok: false; message: string; conflict?: boolean; uncertain?: boolean};

/** The server's words (after its "Invalid …:" prefix), and what they mean here; `ids` are the models it names. */
const REFUSALS: Array<[RegExp, (ids: string) => string]> = [
  [/Configuration changed/i, () => '配置刚被更新（别人发布过，或在另一个窗口发布过）'],
  [/Visible model target has no enabled compatible key/i, ids => `${ids ? `${ids} 的` : '在售模型的'}主线路没有可用的 Key：先在“供应商与 Key”里启用供应商、给 Key 授权这个上游模型，或先把模型隐藏`],
  [/Only hidden or retired mappings can be removed/i, ids => `只能删除已隐藏或已下架的模型${ids ? `：${ids} 还在售` : ''}`],
  [/Only scheduled prices can be cancelled/i, ids => `只能取消还没生效的价格${ids ? `：${ids} 已经生效` : ''}`],
  [/Invalid model ID/i, ids => `模型 ID 无效${ids ? `：${ids}` : ''}（只能用字母、数字和 . _ : / -，最多 128 个字符）`],
  [/Key still serves visible models/i, ids => `这个 Key 还在为在售模型服务${ids ? `：${ids}` : ''}。先隐藏这些模型，或给它们换线路`],
  [/retroactive publication forbidden/i, () => '价格无效，或生效时间早于现在：已有价格的模型只能定在将来生效'],
  [/Published prices immutable/i, () => '这个价格版本已经存在（同一版本 ID，或同一模型同一时刻），请换一个版本 ID 或生效时间'],
  [/Requests are still settling/i, () => '还有请求正在结算，请稍等几秒再发布'],
  [/Publication reason required/i, () => '请填写变更原因（最多约 160 字）'],
  [/Empty publication/i, () => '没有要发布的修改'],
  [/Face value must be/i, () => '积分面值须在 0.0001–1000 元之间，美元汇率须大于 0、不超过 1000'],
  [/Invalid or duplicate rate card/i, () => '价格表的 ID 或名称无效，或有重复'],
  [/Invalid group or unknown rate card/i, () => '分组信息无效（名称、对外套餐名、扣费倍率或显示用量上限），或它的价格表不存在'],
  [/Invalid or duplicate model mapping/i, () => '模型条目无效或重复：检查 ID、上下文与最大输出、扣费倍率、显示倍率、别名（最多 32 个）、备用线路（最多 8 条）、显示名称（最多 64 字节）和说明（最多 256 字节）'],
  [/Cannot move mapping between groups/i, () => '已有模型不能改分组；要放到别的分组，请在那个分组上架'],
  [/Unknown model group/i, () => '模型所在的分组不存在'],
  [/Unknown target provider/i, () => '线路指向的供应商不存在'],
  [/Provider not accessible to model group/i, () => '这个分组不能使用线路上的供应商，或上游模型名无效'],
  [/Ambiguous model ID or alias/i, () => '同一分组里有重复的模型 ID 或别名'],
  [/Margins and model multipliers combine/i, () => '版本倍率 × 分组倍率 × 模型倍率超过了 100 倍'],
  [/Invalid or oversized body/i, () => '提交的内容无效或太大'],
  // A 测试 the server could not start, or a Key or provider that is gone.
  [/No enabled Key of this provider may call this model/i, () => '这个供应商没有启用的 Key 能调用这个模型：先在“供应商与 Key”里给一个启用的 Key 授权它'],
  [/The Key's secret is unavailable/i, () => '读不到这个 Key 的密钥：编辑这个 Key，重新填写密钥'],
  [/^Unknown key$/i, () => '这个 Key 已不存在（可能刚被删除），请刷新'],
  [/^Unknown provider$/i, () => '这个供应商已不存在（可能刚被删除），请刷新'],
];

/**
 * A refusal in words the owner can act on. `name` turns an ID the server names (a model ID or
 * an entry's ID) into what the console calls that model; unknown messages are shown as they are.
 */
export function explainRefusal(text: string, name: (id: string) => string = id => id): string {
  for (const [pattern, explain] of REFUSALS) {
    const match = pattern.exec(text);
    if (!match) continue;
    const rest = text.slice(match.index + match[0].length).replace(/^[\s:：]+/, '');
    const ids = rest ? rest.split(/[\s,，、]+/).filter(Boolean).map(name).join('、') : '';
    return explain(ids);
  }
  return text;
}

/** The status of a failed request, when the server answered with one (0 when it did not). */
const statusOf = (error: unknown) => {
  const status = error && typeof error === 'object' ? (error as {status?: unknown}).status : undefined;
  return typeof status === 'number' ? status : 0;
};

/** Whether the server answered and refused: nothing was changed. */
export const isRefusal = (error: unknown) => [400, 401, 403, 404, 409, 413, 422].includes(statusOf(error));

/** What a failed publication means for the owner: refused (and why), a conflict, or not confirmed. */
export function publishFailure(error: unknown, action: string, name?: (id: string) => string): {ok: false; message: string; conflict?: boolean; uncertain?: boolean} {
  const text = error instanceof Error ? error.message : String(error);
  if (statusOf(error) === 409 && /configuration changed|reload/i.test(text)) return {ok: false, conflict: true, message: '配置刚被更新，请重新加载后再发布（已填的内容会保留）'};
  if (statusOf(error) === 409) return {ok: false, message: `服务器拒绝了这次${action}：${explainRefusal(text, name)}`};
  if (isRefusal(error)) return {ok: false, message: explainRefusal(text, name)};
  return {ok: false, uncertain: true, message: text};
}
