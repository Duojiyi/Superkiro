// 模型与定价's list: one row per customer model ID, across groups (a chip per group, with its
// state there). Each row: its state, its route on two short lines with its backups, the official
// price and 计费倍率, what customers pay (credits and ¥ per million, with the group and model
// multipliers when either is not 1), the lowest margin over every route, scheduled prices, the
// last 7 days' use when the server reports it, and the credits one request needs to start.
// Filters find losses, thin margins, and models without an official price or a cost. Picking a
// group lists its models in Kiro's order, to reorder them and choose the default.
import {useEffect, useState} from 'react';
import {Menu, type MenuItem} from './components/menu';
import {StatusBadge, Tag} from './components/ui';
import {IconImage, IconSpark, IconTool} from './components/icons';
import {formatCount, formatCreditsMicro, formatFullDateTime, formatTokenCount} from './format';
import {defaultModel, groupModels} from './listing';
import {combinedState, passes, sheetFacts, type SheetFacts, type SheetFilters} from './sheetRules';
import type {Four, PricingSettings} from './officialPricing';
import {creditsText} from './priceChange';
import Probe from './Probe';
import {marginText, marginTone, providerName, timesText, usdText, yuanText} from './pricingText';
import {isLive, modelRoute, nameList, targetProblem, targetsOf} from './routes';
import {modelStateView, type StatusView} from './status';
import {formatTokens} from './tokens';

type Row = Record<string, unknown>;
export type StateAction = 'hide' | 'retire' | 'restore' | 'remove';

const MIXED: StatusView = {label: '部分在售', tone: 'warning', title: '有的分组在售，有的分组隐藏或已下架：见模型下方的分组'};

export default function ModelSheet({rows, groups, versions, settings, nowSecs, providers, providerKeys, routesKnown, sample, usage, selectedId, picked, published, busy,
  priceBlocked, stateBlocked, isEdited, isNew, focus, onPick, onEdit, onPrice, onMove, onState}: {
  /** The model entries as they will be published (the draft). */
  rows: Row[];
  groups: Row[];
  versions: Row[];
  settings: PricingSettings;
  nowSecs: number;
  providers: Row[];
  providerKeys: Row[];
  routesKnown: boolean;
  sample: (model: string) => Four;
  /** The last 7 days by customer model, when the server reports it. */
  usage: Map<string, {requests: number; cards: number}> | null;
  selectedId: string | null;
  picked: string[];
  /** Entries the server has (unpublished ones cannot be priced, picked or changed on their own). */
  published: (row: Row) => boolean;
  busy: boolean;
  priceBlocked?: string;
  stateBlocked?: string;
  isEdited: (row: Row) => boolean;
  isNew: (row: Row) => boolean;
  onPick: (ids: string[], on: boolean) => void;
  onEdit: (row: Row) => void;
  onPrice: (row: Row) => void;
  onMove: (row: Row, to: 'up' | 'down' | 'first') => void;
  onState: (id: string, mappings: Row[], action: StateAction) => void;
  /** A model a link names: the list shows it (all groups, no filter), scrolled to and marked. */
  focus?: {model: string} | null;
}) {
  const [groupView, setGroupView] = useState('');
  const [marked, setMarked] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  const [filters, setFilters] = useState<SheetFilters>({loss: false, below: null, unofficial: false, noCost: false, scheduled: false});
  const [belowText, setBelowText] = useState('20');
  const groupName = (id: unknown) => String(groups.find(group => group.id === id)?.name ?? `${String(id ?? '（无分组）')}（分组不存在）`);
  const facts = sheetFacts(rows, {groups, versions, settings, nowSecs, sample});
  const needle = query.trim().toLowerCase();
  const matches = (entry: SheetFacts) => !needle || entry.mappings.some(row => [row.exposed_model_id, row.display_name, row.target_model, row.target_provider_id, providerName(providers, row.target_provider_id)]
    .some(value => String(value ?? '').toLowerCase().includes(needle)));
  const inView = (entry: SheetFacts) => !groupView || entry.mappings.some(row => row.group_id === groupView);
  const shownFacts = facts.filter(entry => inView(entry) && matches(entry) && passes(entry, filters));
  // A group's models in Kiro's order (sort_order, ties as the server lists them).
  const ordered = groupView ? groupModels(rows, groupView).map(row => shownFacts.find(entry => entry.mappings.includes(row))).filter((entry, index, list): entry is SheetFacts => !!entry && list.indexOf(entry) === index) : shownFacts;
  // A named model: everything shown again, then it is scrolled into view and marked.
  useEffect(() => {
    if (!focus) return;
    setGroupView(''); setQuery(''); setFilters({loss: false, below: null, unofficial: false, noCost: false, scheduled: false}); setMarked(focus.model);
    const frame = requestAnimationFrame(() => document.querySelector(`tr[data-model="${CSS.escape(focus.model)}"]`)?.scrollIntoView({block: 'center'}));
    return () => cancelAnimationFrame(frame);
  }, [focus]);
  const counts = {loss: facts.filter(entry => entry.worst !== null && entry.worst < 0).length, unofficial: facts.filter(entry => entry.unofficial).length,
    noCost: facts.filter(entry => entry.noCost).length, scheduled: facts.filter(entry => entry.scheduled).length};
  const withDisplay = rows.some(row => typeof row.display_name === 'string' && row.display_name.trim());
  const pickable = rows.filter(published).map(row => String(row.id)), allPicked = pickable.length > 0 && pickable.every(id => picked.includes(id));
  const toggle = (key: 'loss' | 'unofficial' | 'noCost' | 'scheduled') => setFilters(value => ({...value, [key]: !value[key]}));
  const below = () => {const value = Number(belowText); return Number.isFinite(value) && value > -1000 && value <= 100 ? value / 100 : null;};
  const filtering = !!needle || Object.entries(filters).some(([key, value]) => key === 'below' ? value !== null : value === true);

  // The group being ordered: its default, and models tied at a place.
  const view = groupView ? (() => {
    const all = groupModels(rows, groupView), first = defaultModel(rows, groupView);
    const live = all.filter(isLive), candidates = first ? live.filter(row => Number(row.sort_order ?? 0) === Number(first.sort_order ?? 0)) : [];
    const ties = live.filter(row => live.some(other => other !== row && Number(other.sort_order ?? 0) === Number(row.sort_order ?? 0))).map(row => String(row.exposed_model_id ?? row.id));
    return {all, first, candidates, ties};
  })() : null;
  const capability = (row: Row) => ([['supports_tools', '工具', <IconTool key="i"/>], ['supports_vision', '图片', <IconImage key="i"/>], ['supports_reasoning', '推理', <IconSpark key="i"/>]] as const)
    .filter(([field]) => row[field] === true).map(([, text, icon]) => <span key={text} className="capability" title={text}>{icon}<span className="capability-label">{text}</span></span>);

  const renderRow = (entry: SheetFacts) => {
    const row = groupView ? entry.mappings.find(item => item.group_id === groupView)! : entry.mappings[0];
    const id = entry.id, entryPublished = entry.mappings.filter(published), ids = entryPublished.map(item => String(item.id));
    const route = routesKnown && row.retired !== true ? modelRoute(row, {providers, keys: providerKeys}) : null;
    const [primary, ...backups] = targetsOf(row);
    const table = entry.tables.find(item => item.mappings.includes(row)) ?? entry.tables[0], state = table?.state, credits = state?.credits ?? null;
    const factor = state?.multiplier ?? 1, official = entry.official, version = state?.version ?? null;
    const peers = groupView ? view!.all : [], place = peers.indexOf(row);
    const defaults = entry.mappings.filter(item => defaultModel(rows, item.group_id) === item);
    const tied = groupView && view!.candidates.length > 1 && view!.candidates.includes(row);
    const badge = combinedState(entry.mappings) === 'mixed' ? MIXED : modelStateView(entry.mappings[0]);
    const routeTitle = (entry.tables[0]?.state.routes ?? []).map(item => `${item.primary ? '主' : '备'} ${providerName(providers, item.target.provider_id)} / ${item.target.target_model}：成本 ${item.cost.perM ? `${yuanText(item.cost.perM[0])} / ${yuanText(item.cost.perM[1])}` : '未知'}，毛利 ${marginText(item.margin)}`).join('\n');
    const scheduled = entry.tables.flatMap(item => item.scheduled);
    const used = usage?.get(id);
    const menu: MenuItem[] = [
      {label: '调价', disabled: !!priceBlocked || !entryPublished.length, title: !entryPublished.length ? '先发布这个模型，再调价' : priceBlocked, onSelect: () => onPrice(entryPublished[0] ?? row)},
      ...(groupView ? [{label: '设为默认（排到最前）', disabled: (defaults.includes(row) && !tied) || !isLive(row), title: defaults.includes(row) && !tied ? '已经是默认模型' : !isLive(row) ? '客户看不到的模型不能做默认' : undefined, onSelect: () => onMove(row, 'first')}]
        : entry.mappings.map(item => ({label: entry.mappings.length > 1 ? `在 ${groupName(item.group_id)} 设为默认` : '设为默认（排到最前）', disabled: defaultModel(rows, item.group_id) === item || !isLive(item),
          title: defaultModel(rows, item.group_id) === item ? '已经是默认模型' : !isLive(item) ? '客户看不到的模型不能做默认' : undefined, onSelect: () => onMove(item, 'first')}))),
      // The state actions publish on their own, so only for a published model and a page with nothing unpublished.
      ...(entryPublished.length ? [
        ...(entryPublished.some(isLive) ? [{label: '隐藏（已在用的客户仍可用）', disabled: !!stateBlocked, title: stateBlocked, onSelect: () => onState(id, entryPublished, 'hide')}] : []),
        ...(entryPublished.some(item => !isLive(item)) ? [{label: '重新上架', disabled: !!stateBlocked, title: stateBlocked, onSelect: () => onState(id, entryPublished, 'restore')}] : []),
        ...(entryPublished.some(item => item.retired !== true) ? [{label: '下架（停止服务）', danger: true, disabled: !!stateBlocked, title: stateBlocked, onSelect: () => onState(id, entryPublished, 'retire')}] : []),
        {label: '删除（仅隐藏或已下架的条目）', danger: true, disabled: !!stateBlocked || entryPublished.every(isLive), title: entryPublished.every(isLive) ? '先隐藏或下架，再删除' : stateBlocked, onSelect: () => onState(id, entryPublished, 'remove')},
      ] : []),
    ];
    return <tr key={`${id}\n${String(row.id)}`} data-model={id} className={[entry.mappings.some(item => String(item.id) === selectedId) && 'is-selected', marked === id && 'is-marked'].filter(Boolean).join(' ') || undefined}>
      <td className="col-check"><input type="checkbox" aria-label={`选择 ${id}`} disabled={!ids.length} title={ids.length ? undefined : '先发布这个模型'}
        checked={ids.length > 0 && ids.every(item => picked.includes(item))} onChange={event => onPick(ids, event.target.checked)}/></td>
      {groupView && <td className="col-order"><span className="order-buttons">
        <button type="button" className="btn-icon" aria-label={`上移 ${id}`} title="上移一位" disabled={busy || place === 0} onClick={() => onMove(row, 'up')}>▲</button>
        <button type="button" className="btn-icon" aria-label={`下移 ${id}`} title="下移一位" disabled={busy || place === peers.length - 1} onClick={() => onMove(row, 'down')}>▼</button>
      </span></td>}
      <td className="cell-strong col-model" title={row.display_name ? `显示名：${String(row.display_name)}` : undefined}><span className="mono">{id}</span>
        {tied ? <Tag tone="warning" title="和别的模型排在同一位置：谁是默认以服务器为准，上移或下移一次即可固定">默认待定</Tag>
          : defaults.length > 0 && (!groupView || defaults.includes(row)) && <Tag tone="info" title={`Kiro 列表里的第一个模型：请求没指定模型时用它（${nameList(defaults.map(item => groupName(item.group_id)))}）`}>默认</Tag>}
        {entry.mappings.some(isEdited) && <span className="edited-dot">{entry.mappings.every(isNew) ? '新建' : '已修改'}</span>}
        <span className="group-chips">{entry.mappings.map(item => {
          const state = item.retired === true ? '已下架' : item.visible === false ? '隐藏' : '';
          return <span key={String(item.id)} className={`group-chip${state ? ' is-off' : ''}`} title={`${groupName(item.group_id)}：${state || '在售'}`}>{groupName(item.group_id)}{state ? ` · ${state}` : ' ✓'}</span>;
        })}</span>
        <span className="cell-sub" title={`${formatTokens(row.context_window)} / ${formatTokens(row.max_output)}`}>{typeof row.context_window === 'number' ? formatTokenCount(row.context_window) : '—'} / {typeof row.max_output === 'number' ? formatTokenCount(row.max_output) : '—'}
          <span className="capabilities">{capability(row)}</span></span></td>
      {withDisplay && <td className="col-display">{String(row.display_name ?? '') || <span className="muted">—</span>}</td>}
      <td className="col-status"><StatusBadge view={badge}/></td>
      <td className="col-route" title={routeTitle || undefined}>
        <span className="route-line">{providerName(providers, primary.provider_id) || '—'}</span>
        <span className="route-line mono">{primary.target_model || '—'}</span>
        {(backups.length > 0 || (route && !route.primary.ok)) && <span className="route-tags">
          {backups.length > 0 && <Tag tone="info" title={backups.map((backup, index) => `备 ${index + 1}：${providerName(providers, backup.provider_id)} / ${backup.target_model}`).join('\n')}>主 + {backups.length} 备</Tag>}
          {route && !route.primary.ok && (route.down
            ? <Tag tone={isLive(row) ? 'danger' : 'neutral'} title={targetProblem(route.primary, providers)}>无可用线路</Tag>
            : <Tag tone="warning" title={`${targetProblem(route.primary, providers)}；正由备用线路服务`}>主线路不可用</Tag>)}</span>}
        {published(row) && <Probe providerId={primary.provider_id} model={primary.target_model} title="通过主线路发一次很小的真实请求（花费不到 1 分钱），不保存任何东西"/>}
      </td>
      <td className="num col-official" title={official ? `缓存写（5 分钟）${usdText(official.official[2])} · 缓存读 ${usdText(official.official[3])} / 百万 Tokens` : '现行价格不是按官方价算的（旧版：直接填的积分）'}>
        {official ? `${usdText(official.official[0])} / ${usdText(official.official[1])}` : <span className="muted">未设官方价</span>}
        <span className="cell-sub">{official ? timesText(official.priceMultiplier) : version && Number(version.margin_multiplier ?? 1) !== 1 ? <span className="is-warning" title="旧版价格自带的版本倍率">版本倍率 {timesText(Number(version.margin_multiplier))}</span> : ''}</span></td>
      <td className="num col-price" title={version ? `版本 ${String(version.id)}（积分 / 百万 Tokens）${version.model === '*' ? '；按价格表的通配价 * 扣费' : ''}` : '没有生效中的价格'}>
        {credits ? `${creditsText(credits[0]) ?? '?'} / ${creditsText(credits[1]) ?? '?'}` : version ? '非固定' : <span className="is-warning">未定价</span>}
        {state?.yuanPerM && <span className="cell-sub">{yuanText(state.yuanPerM[0])} / {yuanText(state.yuanPerM[1])}{factor !== 1 && <span title="版本倍率 × 分组倍率 × 模型倍率"> · {timesText(factor)}</span>}</span>}
        {scheduled.length > 0 && <span className="cell-sub is-warning" title={scheduled.map(item => `${formatFullDateTime(Number(item.effective_from_secs))} 起：${creditsText(item.fixed_input_credit_per_m) ?? '—'} / ${creditsText(item.fixed_output_credit_per_m) ?? '—'}（${String(item.id)}）`).join('\n')}>
          已排期 {formatFullDateTime(Number(scheduled[0].effective_from_secs)).slice(5, 16)}{scheduled.length > 1 ? ` 等 ${scheduled.length} 个` : ''}</span>}</td>
      <td className="num col-margin" title={routeTitle || undefined}><span className={marginTone(entry.worst)}>{marginText(entry.worst)}</span></td>
      {usage && <td className="num col-usage">{used ? <>{formatCount(used.requests)} 次<span className="cell-sub">{formatCount(used.cards)} 张卡</span></> : <span className="muted">0</span>}</td>}
      <td className="num col-start" title="开始一次请求要先预留的积分：预计输入（近期请求的中位数，不超过上下文）× 最高的输入侧单价 + 最大输出 × 输出单价，再乘倍率">{entry.start !== null ? formatCreditsMicro(entry.start) : '—'}</td>
      <td className="col-actions"><span className="row-actions">
        <button type="button" className="btn-text" disabled={busy} onClick={() => onEdit(row)}>编辑</button>
        <button type="button" className="btn-text row-action-wide" disabled={!!priceBlocked || !entryPublished.length} title={!entryPublished.length ? '先发布这个模型，再调价' : priceBlocked}
          onClick={() => onPrice(entryPublished[0] ?? row)}>调价</button>
        <Menu label={`${id} 的更多操作`} disabled={busy} items={menu}/>
      </span></td>
    </tr>;
  };

  const columns = 7 + (groupView ? 1 : 0) + (withDisplay ? 1 : 0) + (usage ? 1 : 0) + 1;
  return <section className="panel model-sheet">
    <div className="toolbar-row sheet-toolbar">
      <label className="search-field"><input aria-label="搜索模型" placeholder="模型、上游或供应商" title="只剩一个结果时，回车打开它" value={query} onChange={event => setQuery(event.target.value)}
        onKeyDown={event => {if (event.key === 'Enter' && ordered.length === 1) {const entry = ordered[0]; onEdit(groupView ? entry.mappings.find(item => item.group_id === groupView) ?? entry.mappings[0] : entry.mappings[0]);}}}/></label>
      <label className="sheet-group"><span className="muted">分组</span>
        <select aria-label="按分组看" value={groupView} onChange={event => setGroupView(event.target.value)}>
          <option value="">全部分组</option>
          {groups.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}（排序）</option>)}
        </select></label>
      <div className="sheet-filters" role="group" aria-label="筛选">
        <button type="button" className="chip" aria-pressed={filters.loss} onClick={() => toggle('loss')}>亏损 {counts.loss}</button>
        <span className="chip-with-input">
          <button type="button" className="chip" aria-pressed={filters.below !== null} onClick={() => setFilters(value => ({...value, below: value.below === null ? below() : null}))}>毛利低于</button>
          <input aria-label="毛利低于多少" inputMode="decimal" value={belowText} onChange={event => {setBelowText(event.target.value); setFilters(value => value.below === null ? value : {...value, below: Number.isFinite(Number(event.target.value)) ? Number(event.target.value) / 100 : null});}}/>%</span>
        <button type="button" className="chip" aria-pressed={filters.unofficial} onClick={() => toggle('unofficial')}>未设官方价 {counts.unofficial}</button>
        <button type="button" className="chip" aria-pressed={filters.noCost} onClick={() => toggle('noCost')}>未设成本倍率 {counts.noCost}</button>
        <button type="button" className="chip" aria-pressed={filters.scheduled} onClick={() => toggle('scheduled')}>已排期 {counts.scheduled}</button>
      </div>
      {filtering && <span className="muted">匹配 {formatCount(ordered.length)} 个</span>}
    </div>
    <div className="table-scroll"><table className="table config-table sheet-table">
      <thead><tr>
        <th className="col-check"><input type="checkbox" aria-label="选择全部模型" checked={allPicked} ref={element => {if (element) element.indeterminate = !allPicked && pickable.some(id => picked.includes(id));}}
          disabled={!pickable.length} onChange={event => onPick(pickable, event.target.checked)}/></th>
        {groupView && <th className="col-order">顺序</th>}
        <th>模型 · 分组</th>
        {withDisplay && <th className="col-display">显示名</th>}
        <th>状态</th><th>线路</th>
        <th className="num" title="官方价（美元 / 百万 Tokens，入 / 出）和计费倍率">官方价 · 计费倍率</th>
        <th className="num" title="积分 / 百万 Tokens（入 / 出），下面是客户付的 ¥ / 百万">售价（入/出）</th>
        <th className="num" title="所有线路里最低的毛利，按这个模型近期请求的中位数估算">最低毛利</th>
        {usage && <th className="num">近 7 天</th>}
        <th className="num">起步积分</th>
        <th className="col-actions"><span className="sr-only">操作</span></th>
      </tr></thead>
      {view ? <tbody aria-label={groupName(groupView)}>
        <tr className="group-row"><th colSpan={columns} scope="colgroup">
          <span className="group-row-name">{groupName(groupView)}</span>
          <span className="muted"> · {view.all.length} 个模型 · {view.candidates.length > 1 ? `Kiro 默认：${view.candidates.map(row => String(row.exposed_model_id)).join(' 或 ')}（谁在前以服务器为准）`
            : view.first ? `Kiro 默认：${String(view.first.exposed_model_id)}` : '没有对客户可见的模型'}</span>
          {view.ties.length > 0 && <span className="is-warning"> · {nameList(view.ties, 4)} 排在同一位置，Kiro 里它们的先后以服务器为准；上移或下移一次即可固定</span>}
        </th></tr>
        {ordered.map(renderRow)}
      </tbody> : <tbody aria-label="全部分组">{ordered.map(renderRow)}</tbody>}
      {!ordered.length && <tbody><tr className="state-row"><td colSpan={columns}>{busy ? <div className="skeleton" role="status" aria-label="正在加载"><span className="skeleton-bar"/><span className="skeleton-bar"/></div>
        : <div className="list-state"><p>{filtering ? '没有匹配的模型' : groupView ? '这个分组还没有模型' : '暂无数据'}</p></div>}</td></tr></tbody>}
    </table></div>
  </section>;
}
