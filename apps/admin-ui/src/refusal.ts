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
  [/Invalid group or unknown rate card/i, () => '分组信息无效（名称、对外套餐名、分组倍率或显示用量上限），或它的价格表不存在'],
  [/Invalid or duplicate model mapping/i, () => '模型条目无效或重复：检查 ID、上下文与最大输出、模型倍率、显示倍率、别名（最多 32 个）、备用线路（最多 8 条）、显示名称（最多 64 字节）和说明（最多 256 字节）'],
  [/Cannot move mapping between groups/i, () => '已有模型不能改分组；要放到别的分组，请在那个分组上架'],
  [/Unknown model group/i, () => '模型所在的分组不存在'],
  [/Unknown target provider/i, () => '线路指向的供应商不存在'],
  [/Provider not accessible to model group/i, () => '这个分组不能使用线路上的供应商，或上游模型名无效'],
  [/Ambiguous model ID or alias/i, () => '同一分组里有重复的模型 ID 或别名'],
  [/Margins and model multipliers combine/i, () => '版本倍率 × 分组倍率 × 模型倍率超过了 100 倍'],
  // Prices computed from official ones, and the pricing settings.
  [/Official pricing is at a stale face value or rate/i, ids => `${ids ? `${ids} 的` : '有'}官方价版本还是按旧的积分面值或官方价 $1 折合的人民币算的：改面值要在同一次发布里一起重算（定价设置会自动带上），请重新加载后再试`],
  [/Official pricing does not match the credits of/i, ids => `${ids ? `${ids} 的` : ''}积分和它的官方价算出来的不一致：请重新加载后再定价`],
  [/Official pricing does not match the cost of/i, ids => `${ids ? `${ids} 的` : ''}成本和它的官方价、成本倍率算出来的不一致：请重新加载后再定价`],
  [/Invalid official pricing/i, ids => `官方价无效${ids ? `：${ids}` : ''}（四项都要在 0–10,000 美元之间且不能都是 0，倍率大于 0、不超过 100，按官方价定价的版本不能再设版本倍率）`],
  [/The official dollar rate must be positive/i, () => '官方价 $1 折合的人民币需大于 0、不超过 1000'],
  [/Multipliers must be positive and at most 100/i, () => '计费倍率和成本倍率需大于 0、不超过 100；单独设成本倍率的供应商最多 200 个'],
  [/Official prices: at most/i, () => '官方价表最多 1000 项：模型名 1–256 字节，价格 0–10,000 美元，备注最多 256 字节'],
  [/Route costs: at most/i, () => '线路成本最多 1000 条：名称是“供应商/上游模型”，成本倍率大于 0、不超过 100，计费基准 0–10,000 美元'],
  [/Invalid or oversized body/i, () => '提交的内容无效或太大'],
  // 套餐: each bound a plan is held to, the catalog's rules, and issuing from one; `ids` are the plans.
  [/Plan IDs are 1-64 of a-z, 0-9 and -/i, ids => `套餐 ID 只能用 1–64 个小写字母、数字和 -${ids ? `：${ids}` : ''}`],
  [/Plan names are 1-32 bytes/i, ids => `套餐名称要 1–32 字节（约 10 个汉字），不能有换行${ids ? `：${ids}` : ''}`],
  [/Plan points must be 1-10000000/i, ids => `套餐积分要在 1–10,000,000 之间${ids ? `：${ids}` : ''}`],
  [/Plan prices must be 0-100000 yuan, to the fen/i, ids => `套餐售价要在 0–100,000 元之间，精确到分${ids ? `：${ids}` : ''}`],
  [/Plan validity must be 1-3650 days/i, ids => `套餐有效期要在 1–3650 天之间${ids ? `：${ids}` : ''}`],
  [/Plans allow exactly 1 device, as cards bind one/i, ids => `每张卡只绑定 1 台设备，套餐的设备数只能是 1${ids ? `：${ids}` : ''}`],
  [/Plans allow 1-10 devices/i, ids => `套餐设备数要在 1–10 台之间${ids ? `：${ids}` : ''}`],
  [/Plan concurrency must be 1-20/i, ids => `套餐并发要在 1–20 之间${ids ? `：${ids}` : ''}`],
  [/Plan Kiro types are PRO, PRO_PLUS, PRO_MAX, POWER or CUSTOM/i, ids => `Kiro 显示的档位只能是 PRO、PRO_PLUS、PRO_MAX、POWER 或 CUSTOM${ids ? `：${ids}` : ''}`],
  [/Unknown default group of plan/i, ids => `套餐的默认分组不存在（可能刚被改动），请刷新后重选${ids ? `：${ids}` : ''}`],
  [/Duplicate plan/i, ids => `同一个套餐在这次发布里出现了两次${ids ? `：${ids}` : ''}`],
  [/Plans cards were issued from can only be taken off sale/i, ids => `已经发过卡的套餐不能删除，只能下架${ids ? `：${ids}` : ''}`],
  [/At most 100 plans/i, () => '最多只能有 100 个套餐'],
  [/Unknown plan/i, ids => `这个套餐不存在（可能刚被删除），请刷新${ids ? `：${ids}` : ''}`],
  [/planId and templateId name different plans/i, () => '发卡请求里的套餐前后不一致，请刷新后重试'],
  [/plan is not on sale/i, () => '这个套餐已下架，不能再发卡：在“套餐”里重新上架，或换一个套餐'],
  [/cards have one device; issue from a plan with max_devices 1/i, () => '每张卡只能绑定 1 台设备：请换一个设备数为 1 的套餐'],
  [/issuance requires an enabled group, the plan's credits, and maxDevices=1/i, () => '这个分组现在不能发卡（或发卡参数与套餐不符），请刷新后重选分组'],
  // A 测试 the server could not start, or a Key or provider that is gone.
  [/No enabled Key of this provider may call this model/i, () => '这个供应商没有启用的 Key 能调用这个模型：先在“供应商与 Key”里给一个启用的 Key 授权它'],
  [/The Key's secret is unavailable/i, () => '读不到这个 Key 的密钥：编辑这个 Key，重新填写密钥'],
  [/^Unknown key$/i, () => '这个 Key 已不存在（可能刚被删除），请刷新'],
  [/^Unknown provider$/i, () => '这个供应商已不存在（可能刚被删除），请刷新'],
  // A provider's edit or deletion; `ids` are the models, or the Keys, that still use it.
  [/Provider still routes models/i, ids => `还有模型的线路用这个供应商${ids ? `：${ids}` : ''}。先在“模型与定价”把它们换到别的供应商（主线路和备用线路都算），或删除这些模型`],
  [/Provider still has Keys/i, ids => `这个供应商还有 Key${ids ? `：${ids}` : ''}。先删除这些 Key`],
  [/Invalid provider name/i, () => '供应商名称不能为空，不超过 256 字节，不含控制字符'],
  [/base_url must be a valid URL/i, () => '请填写完整的上游地址，例如 https://api.example.com'],
  [/base_url must use HTTPS/i, () => '上游地址须使用 HTTPS（本机 localhost、127.0.0.1 可用 HTTP）'],
  [/^Nothing to update$/i, () => '没有要保存的修改'],
  // Archiving the ledger.
  [/No ledger entries match the archival cutoff/i, () => '这个日期之前没有可以归档的账本记录：换一个更晚的日期'],
  [/Billing state is not persisted/i, () => '服务器没有把账本保存到磁盘，没有可以归档的内容'],
  [/beforeTsSecs must not be in the future/i, () => '归档日期不能晚于今天'],
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
