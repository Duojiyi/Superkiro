import { parseTokenInput, formatTokens } from './tokens';
import { useEffect, useRef, useState } from 'react';
import { adminApi, AdminApiError, type AdminCardItem, type CommercialConfig } from './api';
import { confirmAction } from './components/confirm';
import { toast } from './components/toast';
import { Drawer, Modal } from './components/modal';
import { InfoTip, Tag, TopbarActions } from './components/ui';
import { IconImage, IconSpark, IconTool } from './components/icons';
import { formatCount, formatTokenCount, shortHash } from './format';
import { creditsText, currentVersion } from './priceChange';
import PriceDrawer, { type PublishOutcome } from './PriceDrawer';
import PriceVersions from './PriceVersions';
import ListModelDrawer from './ListModelDrawer';
import { rebaseDraft } from './rebase';
import { publishFailure } from './refusal';
import { authorizedModels, canRoute, isLive, modelName, modelRoute, nameList, targetProblem } from './routes';

type Row = Record<string, unknown>;

// 分组与权益 / 模型与定价: a list, the row being edited, and — only when something changed — a
// bar at the bottom with the reason and 发布. Everything is published together against the
// version read, with a reason; a publish without a confirmed result blocks the next one until
// a reload, while a refusal changes nothing and leaves the draft to correct (or, when the
// configuration changed meanwhile, to reapply onto the new one). Prices change one model at a
// time in their own drawer (调价).
export default function CommercialEditor({ kind, onDirtyChange, onBusyChange, cards, onPublished, refreshEpoch = 0, providers = [], providerKeys = [], routesKnown = false, intent }: {
  kind: 'groups' | 'models';
  onDirtyChange: (dirty: boolean) => void;
  onBusyChange: (busy: boolean) => void;
  cards?: AdminCardItem[];
  onPublished?: () => void;
  /** Changes when the operator asks the whole console to refresh. */
  refreshEpoch?: number;
  /** For the model editor's 供应商 list and each provider's authorised models. */
  providers?: Row[];
  providerKeys?: Row[];
  /** The providers and Keys above were read: each model's route can be judged. */
  routesKnown?: boolean;
  /** From another page: open 上架模型 for this provider's upstream model. */
  intent?: {list?: {providerId?: string; model?: string}};
}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [draft, setDraft] = useState(''), [loadedDraft, setLoadedDraft] = useState('');
  const [reason, setReason] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  const [selected, setSelected] = useState<string | null>(null);
  useEffect(() => {onDirtyChange(draft !== loadedDraft || !!reason.trim());}, [draft, loadedDraft, reason, onDirtyChange]);
  const pending = useRef(false), alive = useRef(true);
  const [needsReview, setNeedsReview] = useState(false);
  const [messageTone, setMessageTone] = useState<'error' | 'warning' | 'info'>('error');
  const [publishing, setPublishing] = useState(false);
  // The console was refreshed while this page had unpublished edits.
  const [serverChanged, setServerChanged] = useState(false);
  const [priceModel, setPriceModel] = useState<string | null>(null);
  // 上架模型: the drawer that lists a new model, opened on its own or from a provider's Key list.
  const [listing, setListing] = useState<{providerId?: string; model?: string} | null>(null);
  const [jsonOpen, setJsonOpen] = useState(false);
  const [query, setQuery] = useState('');
  const [newGroup, setNewGroup] = useState<Record<string, string | boolean> | null>(null);
  const [newGroupError, setNewGroupError] = useState('');
  // The last publication was refused because the configuration changed meanwhile.
  const [conflict, setConflict] = useState(false);
  const say = (text: string, tone: 'error' | 'warning' | 'info' = 'error') => {setMessage(text); setMessageTone(tone);};
  const dirty = draft !== loadedDraft || !!reason.trim();
  const validText = (value: unknown, max: number) => typeof value === 'string' && !!value.trim() && new TextEncoder().encode(value).length <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);
  const positive = (value: unknown) => typeof value === 'number' && Number.isFinite(value) && value > 0 && value <= 1000;
  let parsedDraft: Record<string, Array<Record<string, unknown>>> = {};
  let draftError = '';
  try {
    const value = JSON.parse(draft);
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('配置必须是 JSON 对象');
    for (const key of Object.keys(value)) {
      if (!['groups', 'models', 'rate_cards', 'versions', 'settings'].includes(key)) throw new Error(`不支持的配置字段：${key}`);
      if (key !== 'settings' && (!Array.isArray(value[key]) || value[key].some((row: unknown) => !row || typeof row !== 'object' || Array.isArray(row)))) throw new Error(`${key} 必须是对象数组`);
    }
    parsedDraft = value;
  } catch (error) {draftError = error instanceof SyntaxError ? '配置 JSON 格式无效，请检查逗号、引号和括号。' : String(error);}
  const rows = Array.isArray(parsedDraft[kind]) ? parsedDraft[kind].filter(row => row && typeof row === 'object' && !Array.isArray(row)) : [];
  const selectedRow = selected === null ? rows[0] : rows.find(row => row.id === selected);
  const editorRef = useRef<HTMLElement>(null);
  const originalRows = config?.[kind] ?? [];
  const originalOf = (row: Row | undefined) => row ? originalRows.find(original => original.id === row.id) : undefined;
  const isEdited = (row: Record<string, unknown>) => JSON.stringify(row) !== JSON.stringify(originalOf(row));
  const focusEditor = () => {editorRef.current?.scrollIntoView({block: 'start'}); editorRef.current?.focus({preventScroll: true});};
  const updateField = (field: string, value: unknown) => {
    if (!selectedRow || rows.filter(row => row.id === selectedRow.id).length !== 1) {say('当前条目 ID 重复或不存在，请先修正高级配置；未修改任何条目。'); return;}
    const next = rows.map(row => row.id === selectedRow?.id ? {...row, [field]: value} : row);
    setDraft(JSON.stringify({...parsedDraft, [kind]: next}, null, 2));
  };
  const numericFields = ['virtual_usage_limit', 'margin_multiplier', 'context_window', 'max_output', 'credit_multiplier', 'rate_multiplier'];
  // Shown in the customer's model list; left empty, the server derives them.
  const optionalFields = ['display_name', 'description', 'rate_multiplier'];
  const tokenFields = ['context_window', 'max_output'];
  const checkFields = ['issuance_enabled', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const fields = kind === 'groups' ? ['name', 'issuance_enabled', 'virtual_plan_name', 'virtual_usage_limit', 'rate_card_id', 'margin_multiplier'] : ['exposed_model_id', 'display_name', 'description', 'rate_multiplier', 'target_provider_id', 'target_model', 'group_id', 'context_window', 'max_output', 'credit_multiplier', 'visible', 'supports_tools', 'supports_vision', 'supports_reasoning'];
  const labels: Record<string, string> = {name: '名称', issuance_enabled: '可发新卡', virtual_plan_name: '对外套餐名', virtual_usage_limit: '显示用量上限', rate_card_id: '价格表', margin_multiplier: '扣费倍率', exposed_model_id: '模型 ID', target_provider_id: '供应商', target_model: '上游模型', group_id: '分组', context_window: '上下文', max_output: '最大输出', credit_multiplier: '扣费倍率', display_name: '显示名称', description: '说明', rate_multiplier: '显示倍率', visible: '客户可见', supports_tools: '工具', supports_vision: '图片', supports_reasoning: '推理'};
  // Accessible names the tests and screen readers already know; each contains its visible label.
  const ariaLabels: Record<string, string> = {context_window: '上下文长度', max_output: '最大输出', margin_multiplier: kind === 'groups' ? '分组扣费倍率' : '扣费倍率', credit_multiplier: '模型扣费倍率'};
  const tips: Record<string, string> = {issuance_enabled: '关闭后不能再发新卡，已发的卡不受影响', virtual_plan_name: '客户端显示的套餐名，与发卡套餐无关', virtual_usage_limit: '只在客户端显示，不是卡内积分', rate_multiplier: '只影响客户端显示，不影响扣费', target_model: '可从列表选，也可直接输入'};
  const placeholders: Record<string, string> = {display_name: '留空用模型 ID', description: '留空自动生成', rate_multiplier: '如 1.3，留空自动换算', context_window: '如 272K', max_output: '如 128K', target_model: '选择或输入'};
  // An unpublished draft survives a session end, restored only onto the configuration it
  // was made from; the draft holds no secret.
  const draftKey = `admin-commercial-draft:v1:${kind}`;
  // `keepSaved` is false for a reload the operator asked for after discarding their edits:
  // the kept draft is dropped then, never brought back.
  const apply = (next: CommercialConfig, keepSaved: boolean): 'restored' | 'stale' | 'none' => {
    const value = JSON.stringify(kind === 'groups' ? {groups: next.groups} : {models: next.models, rate_cards: next.rate_cards, versions: []}, null, 2);
    let saved: {base?: unknown; draft?: unknown; reason?: unknown} = {};
    if (keepSaved) {try {saved = JSON.parse(sessionStorage.getItem(draftKey) || '{}') ?? {};} catch {/* nothing to restore */}}
    else {try {sessionStorage.removeItem(draftKey);} catch {/* nothing kept */}}
    const restore = saved.base === value && typeof saved.draft === 'string' && typeof saved.reason === 'string';
    setConfig(next); setServerChanged(false); setConflict(false);
    setSelected(current => next[kind].some(row => String(row.id) === current) ? current : String(next[kind][0]?.id ?? '')); setReason(restore ? String(saved.reason) : ''); setLoadedDraft(value); setDraft(restore ? String(saved.draft) : value); setNeedsReview(false);
    return restore ? 'restored' : typeof saved.draft === 'string' ? 'stale' : 'none';
  };
  useEffect(() => {
    if (!loadedDraft) return;
    try {
      if (draft !== loadedDraft || reason.trim()) sessionStorage.setItem(draftKey, JSON.stringify({base: loadedDraft, draft, reason}));
      else sessionStorage.removeItem(draftKey);
    } catch {/* A draft that cannot be kept is only lost at a session end. */}
  }, [draft, loadedDraft, reason, draftKey]);
  const load = async (keepSaved = true, quiet = false) => {
    if (pending.current) return;
    pending.current = true; setBusy(true); setMessage('');
    try {
      const result = await adminApi.getCommercialConfig();
      if (result.success !== true || !result.config?.revision) throw new Error('服务器未确认配置读取成功');
      if (alive.current) {
        const draftState = apply(result.config, keepSaved);
        if (draftState === 'restored') say('已恢复未发布的修改，请核对后发布', 'info');
        else if (draftState === 'stale') say('配置已被更新，之前未发布的修改已作废', 'warning');
        else if (!keepSaved && !quiet) toast.success('已重新加载配置');
      }
    } catch (error) {
      if (alive.current) {setNeedsReview(true); say(`加载失败（${error instanceof Error ? error.message : String(error)}），修改已保留；重新加载成功前不能发布`);}
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  };
  useEffect(() => {alive.current = true; void load(); return () => {alive.current = false;};}, [kind]);
  // A console refresh reloads this page too, but never over unpublished edits.
  const seenEpoch = useRef(refreshEpoch);
  useEffect(() => {
    if (refreshEpoch === seenEpoch.current) return;
    seenEpoch.current = refreshEpoch;
    if (pending.current) return;
    if (dirty) setServerChanged(true);
    else void load(false, true);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshEpoch]);
  const editedRows = rows.filter(isEdited);
  const newVersions = Array.isArray(parsedDraft.versions) ? parsedDraft.versions.length : 0;
  // A model the server names, as the list names it.
  const nameOfId = (id: string) => {const models = config?.models ?? []; const model = [...rows, ...models].find(row => row.id === id || row.exposed_model_id === id); return model ? modelName(model, models, config?.groups ?? []) : id;};
  // Models and their rate cards are upserted by ID, so only what changed is sent: the server then
  // checks only those entries, and an unrelated model without a route cannot block a publication.
  const served = (section: string, id: unknown) => (section === 'models' ? config?.models : section === 'rate_cards' ? config?.rate_cards : undefined)?.find(row => row.id === id);
  const publication = () => kind === 'groups' ? parsedDraft : Object.fromEntries(Object.entries(parsedDraft)
    .map(([section, entries]) => [section, section === 'versions' || section === 'settings' || !Array.isArray(entries) ? entries
      : entries.filter(row => JSON.stringify(row) !== JSON.stringify(served(section, row.id)))])
    .filter(([section, entries]) => section === 'settings' || (Array.isArray(entries) && entries.length > 0)));
  const publish = async () => {
    if (!config || pending.current || needsReview) return;
    let submitted = false;
    try {
      if (draftError) throw new Error(draftError);
      if (!validText(reason.trim(), 500)) throw new Error('请填写变更原因（最多约 160 字，不含控制字符）');
      for (const [section, entries] of Object.entries(parsedDraft)) {
        if (section === 'settings') continue;
        const ids = new Set();
        for (const row of entries) {
          if (!validText(row.id, 128) || ids.has(row.id)) throw new Error(`${section} 中的 ID 不能为空、重复或超过 128 字节`);
          ids.add(row.id);
          if (section === 'groups') {
            if (!validText(row.name, 256) || ('virtual_plan_name' in row && !validText(row.virtual_plan_name, 256))) throw new Error('名称和对外套餐名不能为空，且不得超过 256 字节');
            if ('margin_multiplier' in row && !positive(row.margin_multiplier)) throw new Error('分组扣费倍率需大于 0、不超过 1000，空白不能作为 0');
            if ('virtual_usage_limit' in row && (typeof row.virtual_usage_limit !== 'number' || !Number.isFinite(row.virtual_usage_limit) || row.virtual_usage_limit < 0)) throw new Error('显示用量上限需为非负数，不能留空');
          }
          if (section === 'models') {
            if (!validText(row.exposed_model_id, 128) || !validText(row.target_model, 256) || !validText(row.target_provider_id, 256) || !validText(row.group_id, 128)) throw new Error('请填写有效的模型 ID、上游模型、供应商和分组');
            if (!positive(row.credit_multiplier)) throw new Error('模型扣费倍率需大于 0、不超过 1000，空白不能作为 0');
            if (!Number.isSafeInteger(row.context_window) || Number(row.context_window) < 1 || Number(row.context_window) > 10_000_000 || !Number.isSafeInteger(row.max_output) || Number(row.max_output) < 1 || Number(row.max_output) > Number(row.context_window)) throw new Error('上下文长度须为 1 至 10,000,000 的整数；最大输出须为正整数且不能超过上下文长度');
            const old = config.models.find(model => model.id === row.id);
            if (old && old.group_id !== row.group_id) throw new Error('已有模型不能改分组；请在 JSON 中用新的映射 ID 新增条目');
          }
          if (section === 'versions') {
            if (config.versions.some(version => version.id === row.id)) throw new Error('版本 ID 已存在，不能覆盖历史价格');
            if (!Number.isSafeInteger(row.effective_from_secs) || Number(row.effective_from_secs) <= Date.now() / 1000) throw new Error('价格生效时间需晚于现在，请在 JSON 中修正草稿时间');
            if (!positive(row.margin_multiplier)) throw new Error('版本倍率需大于 0、不超过 1000');
          }
        }
      }
      const names = editedRows.map(row => String(row.name ?? row.exposed_model_id ?? row.id));
      const changes = names.length + newVersions;
      const update = publication();
      if (kind === 'models' && !Object.keys(update).length) throw new Error('没有要发布的修改（只填了原因）');
      const confirmed = await confirmAction({
        title: changes ? `发布 ${changes} 项修改？` : `发布${kind === 'groups' ? '分组' : '模型与价格'}配置？`,
        facts: [
          ...(names.length ? [`修改：${names.slice(0, 5).join('、')}${names.length > 5 ? ` 等 ${names.length} 项` : ''}`] : []),
          ...(newVersions ? [`${newVersions} 个新价格版本按指定时间生效`] : []),
          `原因：${reason.trim()}`,
          `基于版本 ${shortHash(config.revision)}`,
        ],
        consequence: '发布后新请求立即生效。',
        confirmLabel: '发布',
      });
      if (!confirmed || !alive.current || pending.current) return;
      pending.current = true; submitted = true; setBusy(true); setPublishing(true); setMessage(''); setConflict(false);
      const result = await adminApi.publishCommercialConfig({...update, expected_revision: config.revision, reason: reason.trim()});
      if (result.success !== true) throw new AdminApiError('服务器未确认发布成功', 400);
      if (!result.config?.revision) throw new Error('服务器未返回可核对的配置版本');
      try {sessionStorage.removeItem(draftKey);} catch {/* published; nothing left to keep */}
      if (alive.current) {apply(result.config, false); toast.success('已发布'); onPublished?.();}
    } catch (error) {
      if (alive.current) {
        const text = error instanceof Error ? error.message : String(error);
        // A refusal (the server said no) changed nothing: the draft stays as typed, to correct.
        const failure = submitted ? publishFailure(error, '发布', nameOfId) : {message: text, uncertain: false, conflict: false};
        if (failure.uncertain) {setNeedsReview(true); say(`没收到发布结果（${text}）。修改已保留，请重新加载确认后再发布，不要重复提交。`);}
        else if (failure.conflict) {setConflict(true); say('配置刚被别人更新（或在另一个窗口发布过），这次什么都没有发布。点“重新加载并保留修改”：读取最新配置，再把你的修改套上去。', 'warning');}
        else say(failure.message);
      }
    } finally {if (submitted) {pending.current = false; if (alive.current) {setBusy(false); setPublishing(false);}}}
  };
  // 重新加载并保留修改: the latest configuration, with this draft's edits applied again where the
  // same fields have not been changed on the server meanwhile; the reason is kept.
  const reloadKeeping = async () => {
    if (!config || pending.current || draftError) return;
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.getCommercialConfig();
      if (result.success !== true || !result.config?.revision) throw new Error('服务器未确认配置读取成功');
      if (!alive.current) return;
      const next = result.config, keptReason = reason;
      const view = kind === 'groups' ? {groups: next.groups} : {models: next.models, rate_cards: next.rate_cards, versions: next.versions};
      const rebased = rebaseDraft(JSON.parse(loadedDraft), parsedDraft, view);
      apply(next, false);
      setDraft(JSON.stringify(rebased.draft, null, 2)); setReason(keptReason);
      const label = (row: Record<string, unknown>) => String(row.name ?? (row.exposed_model_id ? modelName(row, next.models, next.groups) : row.id));
      const lost = rebased.skipped.map(item => item.reason === 'removed' ? `${label(item.row)}（服务器上已删除）` : item.reason === 'exists' ? `新建的 ${String(item.row.id)}（服务器上已有这个 ID）`
        : `${label(item.row)} 的${labels[item.field ?? ''] ?? item.field}（服务器现为 ${shownValue(item.field ?? '', item.server)}）`);
      say(lost.length ? `已读取最新配置。${rebased.applied ? `保留了 ${rebased.applied} 项修改；` : ''}这些修改没有套用，因为别人已经改过：${nameList(lost, 8)}。请核对后再发布。`
        : `已读取最新配置，并保留了你的 ${rebased.applied} 项修改，请核对后再发布`, lost.length ? 'warning' : 'info');
    } catch (error) {
      if (alive.current) say(`重新加载失败（${error instanceof Error ? error.message : String(error)}），修改已保留，可以再试`);
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  };
  // One change published on its own: 调价 (a price version), 上架 (a new model), a row action.
  // `done` is the confirmation shown on success; `check`, what to look for when the result is
  // not confirmed.
  const publishOne = async (update: {models?: Row[]; versions?: Row[]; removed_models?: string[]; cancelled_versions?: string[]}, updateReason: string, action: string, done?: string, check?: string): Promise<PublishOutcome> => {
    if (!config || pending.current || needsReview || dirty) return {ok: false, message: '有未完成的发布或修改，请先处理'};
    pending.current = true; setBusy(true); setMessage('');
    try {
      const result = await adminApi.publishCommercialConfig({...update, expected_revision: config.revision, reason: updateReason});
      if (result.success !== true) throw new AdminApiError('服务器未确认发布成功', 400);
      if (!result.config?.revision) throw new Error('服务器未返回可核对的配置版本');
      if (alive.current) {apply(result.config, false); if (done) toast.success(done); onPublished?.();}
      return {ok: true};
    } catch (error) {
      // Refusals (the server says 409 for every rule) change nothing and can be corrected.
      const failure = publishFailure(error, action, nameOfId);
      if (failure.uncertain && alive.current) {setNeedsReview(true); say(`没收到${action}结果（${failure.message}）。请点“重新加载”后核对${check ? `${check}` : '是否已生效'}，不要重复提交。`);}
      return failure;
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  };
  const publishPrice = (version: Row, priceReason: string) => publishOne({versions: [version]}, priceReason, '调价', `已发布 ${String(version.model)} 的新价格`);
  const reload = async () => {
    if (dirty && !(await confirmAction({title: '放弃未发布的修改？', consequence: '会重新加载服务器上的配置。', confirmLabel: '放弃修改'}))) return;
    void load(false);
  };
  const rateCards = config?.rate_cards ?? [], configGroups = config?.groups ?? [], configModels = config?.models ?? [], configVersions = config?.versions ?? [];
  const rateCardName = (id: unknown) => String(rateCards.find(card => card.id === id)?.name ?? id ?? '—');
  const groupName = (id: unknown) => String(configGroups.find(group => group.id === id)?.name ?? id ?? '—');
  const cardCount = (groupId: unknown) => cards ? cards.filter(card => card.groupId === groupId && card.status !== 'voided' && card.archivedAt == null).length : null;
  const existingModel = kind === 'models' && !!selectedRow && configModels.some(model => model.id === selectedRow.id);
  const nowSecs = Date.now() / 1000;
  const publishBlocked = needsReview ? '请重新加载确认后再发布' : !config ? '配置没有加载' : !reason.trim() ? '填写变更原因后可发布' : undefined;
  const editorFields = selectedRow ? fields.filter(field => checkFields.includes(field) ? field === 'issuance_enabled' || field in selectedRow : field in selectedRow) : [];
  // The price a model's requests are charged at now (customer prices per million tokens).
  const priceOf = (row: Row) => {
    const group = configGroups.find(item => item.id === row.group_id);
    return group ? currentVersion(configVersions, group.rate_card_id, [row.exposed_model_id, row.target_model], nowSecs) : null;
  };
  // 调价 and 上架 publish on their own, so only from a page with nothing else unpublished.
  const ownBlocked = (action: string) => needsReview ? '请先重新加载确认上次发布' : dirty ? `先发布或放弃未发布的修改，再${action}` : busy ? '正在处理' : undefined;
  const priceBlocked = ownBlocked('调价'), listingBlocked = ownBlocked('上架'), canListModels = kind === 'models';
  const openListing = (preset: {providerId?: string; model?: string}) => {setJsonOpen(false); setPriceModel(null); setListing(preset);};
  // A link from 供应商与 Key opens the drawer for that provider's model, once, when the page has loaded.
  const intentUsed = useRef(false);
  useEffect(() => {
    if (kind !== 'models' || !intent?.list || intentUsed.current || !config) return;
    intentUsed.current = true;
    if (!listingBlocked) openListing(intent.list);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intent, config]);
  const searchable = rows.length > 15;
  const needle = query.trim().toLowerCase();
  const listed = searchable && needle ? rows.filter(row => [row.id, row.name, row.exposed_model_id, row.display_name, row.target_model, row.target_provider_id].some(value => String(value ?? '').toLowerCase().includes(needle))) : rows;
  const providerModels = (providerId: unknown) => authorizedModels(providerId, providerKeys);
  // Whether each target of a model can serve, once providers and Keys are known; nothing for a retired model.
  const routeOf = (row: Row) => routesKnown && kind === 'models' && row.retired !== true ? modelRoute(row, {providers, keys: providerKeys}) : null;
  const shownValue = (field: string, value: unknown) => {
    if (value === undefined || value === null || value === '') return '空';
    if (typeof value === 'boolean') return value ? '开' : '关';
    if (tokenFields.includes(field) && typeof value === 'number') return formatTokenCount(value);
    if (field === 'rate_card_id') return rateCardName(value);
    if (field === 'target_provider_id') return String(providers.find(provider => provider.id === value)?.name ?? value);
    return String(value);
  };

  const renderInput = (field: string) => {
    if (!selectedRow) return null;
    const label = labels[field];
    const aria = ariaLabels[field] ?? label;
    if (field === 'rate_card_id' && rateCards.length) {
      const value = String(selectedRow[field] ?? '');
      return <select aria-label={aria} value={value} onChange={event => updateField(field, event.target.value)}>
        {!rateCards.some(card => card.id === value) && <option value={value}>{value || '请选择'}</option>}
        {rateCards.map(card => <option key={String(card.id)} value={String(card.id)}>{String(card.name ?? card.id)}</option>)}
      </select>;
    }
    if (field === 'group_id' && configGroups.length) {
      const value = String(selectedRow[field] ?? '');
      return <select aria-label={aria} value={value} disabled={existingModel} title={existingModel ? '已有模型不能改分组' : undefined} onChange={event => updateField(field, event.target.value)}>
        {!configGroups.some(group => group.id === value) && <option value={value}>{value || '请选择'}</option>}
        {configGroups.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}</option>)}
      </select>;
    }
    // Routing is chosen, not typed: a mistyped provider or upstream model fails every request.
    if (field === 'target_provider_id' && providers.length) {
      const value = String(selectedRow[field] ?? '');
      return <select aria-label="供应商" value={value} onChange={event => updateField(field, event.target.value)}>
        {!providers.some(provider => provider.id === value) && <option value={value}>{value ? `${value}（未找到）` : '请选择'}</option>}
        {providers.map(provider => <option key={String(provider.id)} value={String(provider.id)}>{String(provider.name ?? provider.id)}{provider.api_type === 'openai' ? ' · OpenAI' : ''}{provider.enabled === false ? '（已停用）' : ''}</option>)}
      </select>;
    }
    if (field === 'target_model') {
      const options = providerModels(selectedRow.target_provider_id);
      const listId = `upstream-models-${String(selectedRow.id ?? 'new').replace(/[^\w-]/g, '_')}`;
      return <>
        <input aria-label="上游模型" list={listId} placeholder={placeholders[field]} value={String(selectedRow[field] ?? '')} onChange={event => updateField(field, event.target.value)}/>
        <datalist id={listId}>{options.map(model => <option key={model} value={model}/>)}</datalist>
      </>;
    }
    const token = tokenFields.includes(field);
    const multiplier = ['margin_multiplier', 'credit_multiplier'].includes(field);
    const input = <input aria-label={aria} placeholder={placeholders[field]} type={numericFields.includes(field) && !token ? 'number' : 'text'} step={token ? '1' : 'any'}
      value={String(selectedRow[field] ?? '')}
      onChange={event => updateField(field, optionalFields.includes(field) && !event.target.value.trim() ? null : token ? parseTokenInput(event.target.value) : numericFields.includes(field) && event.target.value.trim() ? Number(event.target.value) : event.target.value)}/>;
    return multiplier ? <span className="input-suffix">{input}<span>×</span></span> : input;
  };
  // Under an edited input: what it was.
  const original = originalOf(selectedRow);
  const was = (field: string) => original && JSON.stringify(original[field]) !== JSON.stringify(selectedRow?.[field]) ? <span className="field-was">原 {shownValue(field, original[field])}</span> : null;
  // Only an enabled provider with an enabled Key that allows the model can serve it.
  const upstreamProvider = kind === 'models' && selectedRow && selectedRow.target_model ? providers.find(provider => provider.id === selectedRow.target_provider_id) : undefined;
  const upstreamHint = !upstreamProvider ? '' : upstreamProvider.enabled === false ? '这个供应商已停用：这条线路不会被使用'
    : canRoute(upstreamProvider.id, String(selectedRow?.target_model), providerKeys) ? '' : '这个供应商的 Key 还没有授权此模型（停用的 Key 不算）';

  // 新建分组: a small form; the new row goes into the draft and is published with the bar.
  const addGroup = () => {
    if (!newGroup) return;
    const id = String(newGroup.id ?? '').trim(), name = String(newGroup.name ?? '').trim(), multiplierValue = Number(newGroup.multiplier);
    if (!validText(id, 128) || !/^[\w.-]+$/.test(id)) {setNewGroupError('ID 只能用字母、数字、- _ .，不超过 128 字节'); return;}
    if (rows.some(row => row.id === id) || configGroups.some(group => group.id === id)) {setNewGroupError('这个 ID 已存在'); return;}
    if (!validText(name, 256)) {setNewGroupError('请填写名称（不超过 256 字节）'); return;}
    if (!rateCards.some(card => card.id === newGroup.rateCard)) {setNewGroupError('请选择价格表'); return;}
    if (!String(newGroup.multiplier ?? '').trim() || !positive(multiplierValue)) {setNewGroupError('扣费倍率需大于 0、不超过 1000'); return;}
    const row = {id, name, issuance_enabled: newGroup.issuance === true, provider_binding_mode: 'shared', rate_card_id: String(newGroup.rateCard),
      margin_multiplier: multiplierValue, virtual_plan_name: name, virtual_usage_limit: 0, system_prompt_prefix: null};
    setDraft(JSON.stringify({...parsedDraft, groups: [...rows, row]}, null, 2));
    setSelected(id); setNewGroup(null); setNewGroupError('');
    say(`已加入分组“${name}”，填写原因后发布`, 'info');
  };

  const showBar = dirty || needsReview || serverChanged || publishing || !!message;
  const capability = (row: Row) => ([['supports_tools', '工具', <IconTool key="i"/>], ['supports_vision', '图片', <IconImage key="i"/>], ['supports_reasoning', '推理', <IconSpark key="i"/>]] as const)
    .filter(([field]) => row[field] === true).map(([, text, icon]) => <span key={text} className="capability">{icon}{text}</span>);

  return <div className="page-stack commercial-editor">
    <TopbarActions>
      {kind === 'groups' && <button type="button" className="btn" disabled={busy || !config || !!draftError} title={draftError ? 'JSON 无效，先修正' : undefined}
        onClick={() => {setNewGroupError(''); setNewGroup({id: '', name: '', rateCard: String(rateCards[0]?.id ?? ''), multiplier: '1', issuance: true});}}>＋ 新建分组</button>}
      {canListModels && <button type="button" className="btn btn-primary" disabled={!config || !!listingBlocked} title={listingBlocked}
        onClick={() => openListing({})}>＋ 上架模型</button>}
      <button type="button" className="btn" aria-expanded={jsonOpen} onClick={() => {setPriceModel(null); setListing(null); setJsonOpen(open => !open);}}>JSON</button>
    </TopbarActions>
    <section className="panel">
      {searchable && <div className="toolbar-row"><label className="search-field"><input aria-label={kind === 'groups' ? '搜索分组' : '搜索模型'} placeholder={kind === 'groups' ? '分组名称或 ID' : '模型、上游或供应商'} value={query} onChange={event => setQuery(event.target.value)}/></label>
        {needle && <span className="muted">匹配 {formatCount(listed.length)} 个</span>}</div>}
      <div className="table-scroll"><table className="table config-table">
        <thead><tr>{(kind === 'groups' ? ['名称', '可发卡', '对外套餐名', '用量上限', '价格表', '倍率', '卡密数', ''] : ['模型', '显示名', '上游', '分组', '上下文 / 输出', '能力', '当前价格（入/出）', '客户可见', '']).map((label, index) =>
          <th key={index} className={['用量上限', '倍率', '卡密数', '上下文 / 输出', '当前价格（入/出）'].includes(label) ? 'num' : label === '显示名' ? 'col-display' : label ? undefined : 'col-actions'}>{label || <span className="sr-only">操作</span>}</th>)}</tr></thead>
        <tbody>
          {listed.map((row, index) => {
            const active = selectedRow?.id === row.id;
            const edited = isEdited(row);
            const price = kind === 'models' ? priceOf(row) : null;
            const published = kind === 'models' && configModels.some(model => model.id === row.id);
            const route = routeOf(row);
            const cells = kind === 'groups'
              ? [<td key="n" className="cell-strong">{String(row.name ?? row.id)}{edited && <span className="edited-dot">{originalOf(row) ? '已修改' : '新建'}</span>}</td>,
                <td key="i">{row.issuance_enabled === false ? <span className="muted">—</span> : '✓'}</td>,
                <td key="p">{String(row.virtual_plan_name ?? '—')}</td>,
                <td key="u" className="num">{typeof row.virtual_usage_limit === 'number' ? formatCount(row.virtual_usage_limit) : '—'}</td>,
                <td key="r" title={String(row.rate_card_id ?? '')}>{rateCardName(row.rate_card_id)}</td>,
                <td key="m" className="num">{String(row.margin_multiplier ?? '—')}</td>,
                <td key="c" className="num">{cardCount(row.id) ?? '—'}</td>]
              : [<td key="n" className="cell-strong mono" title={row.display_name ? `显示名：${String(row.display_name)}` : undefined}>{String(row.exposed_model_id ?? row.id)}{edited && <span className="edited-dot">{originalOf(row) ? '已修改' : '新建'}</span>}</td>,
                <td key="d" className="col-display">{String(row.display_name ?? '') || <span className="muted">—</span>}</td>,
                <td key="t" title={`${String(row.target_provider_id ?? '—')} / ${String(row.target_model ?? '—')}`}><span className="mono clip clip-upstream">{String(row.target_provider_id ?? '—')} / {String(row.target_model ?? '—')}</span>
                  {route && !route.primary.ok && <span className="route-tags">{route.down
                    ? <Tag tone={isLive(row) ? 'danger' : 'neutral'} title={targetProblem(route.primary, providers)}>无可用线路</Tag>
                    : <Tag tone="warning" title={`${targetProblem(route.primary, providers)}；正由备用线路服务`}>主线路不可用</Tag>}</span>}</td>,
                <td key="g" title={String(row.group_id ?? '')}>{groupName(row.group_id)}</td>,
                <td key="w" className="num" title={`${formatTokens(row.context_window)} / ${formatTokens(row.max_output)}`}>{typeof row.context_window === 'number' ? formatTokenCount(row.context_window) : '—'} / {typeof row.max_output === 'number' ? formatTokenCount(row.max_output) : '—'}</td>,
                <td key="a"><span className="capabilities">{capability(row)}</span></td>,
                <td key="p" className="num" title={price ? `版本 ${String(price.id)}（积分 / 百万 Tokens）` : '没有生效中的价格'}>{price && price.pricing_mode === 'fixed'
                  ? `${creditsText(price.fixed_input_credit_per_m) ?? '?'} / ${creditsText(price.fixed_output_credit_per_m) ?? '?'}` : price ? '非固定' : <span className="is-warning">未定价</span>}</td>,
                <td key="v">{row.visible === false ? <span className="muted">隐藏</span> : '✓'}</td>];
            return <tr key={String(row.id ?? index)} className={active ? 'is-selected' : undefined}>
              {cells}
              <td className="col-actions"><span className="row-actions">
                <button type="button" className="btn-text" disabled={busy} onClick={() => {setSelected(String(row.id)); focusEditor();}}>编辑</button>
                {kind === 'models' && <button type="button" className="btn-text" disabled={!!priceBlocked || !published} title={!published ? '先发布这个模型，再调价' : priceBlocked}
                  onClick={() => {setJsonOpen(false); setListing(null); setPriceModel(String(row.id));}}>调价</button>}
              </span></td>
            </tr>;
          })}
          {!listed.length && <tr className="state-row"><td colSpan={9}>{busy ? <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/></div> : <div className="list-state"><p>{needle ? '没有匹配的条目' : '暂无数据'}</p></div>}</td></tr>}
        </tbody>
      </table></div>
    </section>

    <section ref={editorRef} tabIndex={-1} className="panel mapping-editor">
      <h3>编辑：{String(selectedRow?.name ?? selectedRow?.exposed_model_id ?? '请选择条目')}</h3>
      <fieldset disabled={busy || !selectedRow} className="form-grid form-grid-3">
        {editorFields.filter(field => !checkFields.includes(field)).map(field => <label key={field} className="field">
          <span className="field-label">{labels[field]}{tips[field] && <InfoTip text={tips[field]}/>}</span>
          {renderInput(field)}
          {tokenFields.includes(field) && selectedRow && <span className="field-hint" title={formatTokens(selectedRow[field])}>{typeof selectedRow[field] === 'number' && Number(selectedRow[field]) > 0 ? `${formatTokenCount(Number(selectedRow[field]))} Tokens` : '请输入正整数 Tokens'}</span>}
          {field === 'target_model' && upstreamHint && <span className="field-warning">{upstreamHint}</span>}
          {was(field)}
        </label>)}
        {editorFields.some(field => checkFields.includes(field)) && <div className="field field-span check-row">
          {editorFields.filter(field => checkFields.includes(field)).map(field => <span key={field} className="check-field">
            <label><input type="checkbox" checked={field === 'issuance_enabled' ? selectedRow?.[field] !== false : Boolean(selectedRow?.[field])} onChange={event => updateField(field, event.target.checked)}/>{labels[field]}</label>
            {tips[field] && <InfoTip text={tips[field]}/>}
            {was(field)}
          </span>)}
        </div>}
      </fieldset>
    </section>

    {kind === 'models' && <PriceVersions versions={configVersions} rateCards={rateCards} groups={configGroups} cards={cards} faceValue={config?.settings?.credit_face_value_cny} nowSecs={nowSecs}/>}

    {jsonOpen && <Drawer id="config-json" label="JSON 配置" onClose={() => setJsonOpen(false)} className="json-drawer">
      <header className="drawer-head">
        <div className="drawer-title"><span className="drawer-model">JSON</span><span className="muted">{kind === 'groups' ? '分组' : '模型与价格'}</span></div>
        <div className="drawer-tools"><button type="button" className="btn-icon" aria-label="关闭 JSON" title="关闭（Esc）" onClick={() => setJsonOpen(false)}>×</button></div>
      </header>
      <div className="drawer-body">
        <section aria-label="编辑 JSON（高级）">
          <h4>编辑 JSON（高级）</h4>
          <p className="muted">按 ID 新增或更新；删掉一行不会删除服务器上的条目。改动随页面底部的“发布”一起发布。</p>
          <textarea aria-label="配置 JSON" value={draft} disabled={busy} onChange={e => setDraft(e.target.value)} spellCheck={false} className="code-input"/>
          {config && draftError && <p role="alert" className="form-error">{draftError} 修正后才能发布。</p>}
        </section>
        <details className="json-raw">
          <summary>查看原始配置（只读）</summary>
          <pre className="code-block">{JSON.stringify(config, null, 2)}</pre>
        </details>
      </div>
    </Drawer>}

    {priceModel && config && (() => {
      const model = configModels.find(item => item.id === priceModel);
      if (!model) return null;
      return <PriceDrawer key={priceModel} model={model} group={configGroups.find(group => group.id === model.group_id) ?? null} config={config}
        onClose={() => setPriceModel(null)} onPublish={publishPrice} onReload={() => load(false, true)}/>;
    })()}

    {listing && config && <ListModelDrawer preset={listing} config={config} providers={providers} providerKeys={providerKeys}
      onClose={() => setListing(null)} onPublish={(update, listingReason) => publishOne(update, listingReason, '上架')} onReload={() => load(false, true)}/>}

    {newGroup && <Modal label="新建分组" onClose={() => setNewGroup(null)} className="dialog-form">
      <h3 className="modal-title">新建分组</h3>
      {newGroupError && <p role="alert" className="form-error">{newGroupError}</p>}
      <div className="form-grid form-grid-2">
        <label className="field"><span className="field-label">ID</span><input aria-label="分组 ID" placeholder="例：pro-plus-2" value={String(newGroup.id)} onChange={event => setNewGroup({...newGroup, id: event.target.value})}/></label>
        <label className="field"><span className="field-label">名称</span><input aria-label="分组名称" value={String(newGroup.name)} onChange={event => setNewGroup({...newGroup, name: event.target.value})}/></label>
        <label className="field"><span className="field-label">价格表</span><select aria-label="分组价格表" value={String(newGroup.rateCard)} onChange={event => setNewGroup({...newGroup, rateCard: event.target.value})}>
          {!rateCards.length && <option value="">没有价格表</option>}
          {rateCards.map(card => <option key={String(card.id)} value={String(card.id)}>{String(card.name ?? card.id)}</option>)}
        </select></label>
        <label className="field"><span className="field-label">扣费倍率</span><span className="input-suffix"><input aria-label="新分组扣费倍率" inputMode="decimal" value={String(newGroup.multiplier)} onChange={event => setNewGroup({...newGroup, multiplier: event.target.value})}/><span>×</span></span></label>
        <label className="check-field field-span"><input type="checkbox" checked={newGroup.issuance === true} onChange={event => setNewGroup({...newGroup, issuance: event.target.checked})}/> 可发新卡</label>
      </div>
      <p className="muted">对外套餐名默认同名称，显示用量上限为 0，可在加入后修改。</p>
      <div className="modal-actions">
        <button type="button" className="btn" onClick={() => setNewGroup(null)}>取消</button>
        <button type="button" className="btn btn-primary" onClick={addGroup}>加入草稿</button>
      </div>
    </Modal>}

    {showBar && <div className={`action-bar${dirty ? ' is-dirty' : ''}`} role="region" aria-label="发布">
      <div className="action-bar-main">
        <span className="action-bar-status">
          {editedRows.length > 0 && <span className="dirty-dot">{editedRows.length} 项修改未发布</span>}
          {newVersions > 0 && <span className="dirty-dot">{newVersions} 个新价格版本</span>}
          {needsReview && <span className="is-warning">请重新加载确认后再发布</span>}
          {serverChanged && !needsReview && <span className="is-warning">服务器上的配置可能已更新</span>}
        </span>
        <input className="action-bar-reason" aria-label="变更原因" placeholder="变更原因（必填）" value={reason} maxLength={500} disabled={busy} onChange={e => setReason(e.target.value)}/>
        {/* Discarding is offered only when there is something to discard (or to review). */}
        {(dirty || needsReview || serverChanged || !config) && <button type="button" className="btn" disabled={busy} onClick={() => void reload()}>
          {dirty ? (serverChanged && !needsReview ? '放弃修改并加载' : '放弃修改') : '重新加载'}</button>}
        {(conflict || (serverChanged && !needsReview)) && dirty && !draftError && <button type="button" className="btn" disabled={busy} onClick={() => void reloadKeeping()}>重新加载并保留修改</button>}
        <button type="button" className="btn btn-primary" disabled={busy || !!publishBlocked} title={publishBlocked} onClick={() => void publish()}>{publishing ? '发布中…' : '发布'}</button>
      </div>
      {message && <p role="status" className={`message message-${messageTone}`}>{message}</p>}
    </div>}
  </div>;
}
