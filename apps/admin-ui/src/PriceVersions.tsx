// Price versions, grouped by price table: each table names the groups that use it (and their
// cards), and one row per version says what a model costs, from when, and whether it is the one
// in force. Superseded versions stay one click away (显示历史); a table no customer uses is
// folded away, so its prices are not read as the ones customers pay.
import {useState} from 'react';
import {IdCell, StatusBadge} from './components/ui';
import {formatCount, formatDateTime, formatFullDateTime} from './format';
import {costText, creditsText, PRICE_FIELDS, routeCostOf} from './priceChange';
import {priceVersionView} from './status';

type Row = Record<string, unknown>;
type CardRow = {groupId: string; status: string; archivedAt?: number | null};

function priceCells(version: Row) {
  if (version.pricing_mode === 'fixed') return PRICE_FIELDS.map(([field]) => <td key={field} className="num">{creditsText(version[field]) ?? <span className="is-warning" title="无法安全显示，请核对原始配置">?</span>}</td>);
  const text = version.pricing_mode === 'per_call' ? `每次调用扣 ${creditsText(version.per_call_credit) ?? '?'} 积分（不按 Tokens）`
    : version.pricing_mode === 'cost_plus' ? '成本加成（按采购价计费）' : `未知计费方式：${String(version.pricing_mode ?? '—')}`;
  return [<td key="mode" colSpan={PRICE_FIELDS.length} className="muted">{text}</td>];
}

/**
 * Who pays a price table's prices: the groups that use it and their current cards (null when the
 * cards are not known). A table no customer uses — no group, only groups that cannot issue cards,
 * or groups without a card — is `internal`.
 */
export function rateCardUse(cardId: unknown, groups: Row[], cards?: CardRow[] | null): {groups: Row[]; cards: number | null; internal: false | string} {
  const users = groups.filter(group => group.rate_card_id === cardId);
  const count = cards ? cards.filter(card => users.some(group => group.id === card.groupId) && card.status !== 'voided' && card.archivedAt == null).length : null;
  const internal = !users.length ? '没有分组使用的价格表' : users.every(group => group.issuance_enabled === false) ? '验收专用价格表（客户不使用）'
    : count === 0 ? '客户不使用的价格表（分组还没有卡密）' : false;
  return {groups: users, cards: count, internal};
}

export default function PriceVersions({versions, rateCards = [], groups = [], cards, faceValue, providers = [], nowSecs}: {
  versions: Row[];
  rateCards?: Row[];
  groups?: Row[];
  /** To tell a route's procurement cost (`<provider>/<upstream model>`) from a customer price. */
  providers?: Row[];
  /** Current cards, to say how many use each table; null or absent when they are not loaded. */
  cards?: CardRow[] | null;
  /** Yuan per credit (积分面值). */
  faceValue?: number;
  nowSecs: number;
}) {
  const [history, setHistory] = useState(false);
  const rows = versions.map(version => ({version, view: priceVersionView(version, versions, nowSecs)}));
  const superseded = rows.filter(row => row.view.label === '已被替代').length;
  // Every table with versions, customer-facing first; a version of an unknown table gets one of its own.
  const tables = [...new Set([...rateCards.map(card => card.id), ...versions.map(version => version.rate_card_id)])]
    .map(id => ({id, card: rateCards.find(card => card.id === id), use: rateCardUse(id, groups, cards)}))
    .filter(table => versions.some(version => version.rate_card_id === table.id))
    .sort((a, b) => Number(!!a.use.internal) - Number(!!b.use.internal) || String(a.card?.name ?? a.id).localeCompare(String(b.card?.name ?? b.id)));
  const users = (use: ReturnType<typeof rateCardUse>) => use.groups.length
    ? `用于 ${use.groups.map(group => {const name = String(group.name ?? group.id); return `${name}${group.issuance_enabled === false && !name.includes('禁止发卡') ? '（禁止发卡）' : ''}`;}).join('、')}${use.cards === null ? '' : ` · ${formatCount(use.cards)} 张卡`}`
    : '没有分组使用';
  const table = (id: unknown) => {
    const shown = rows.filter(row => row.version.rate_card_id === id && (history || row.view.label !== '已被替代'))
      .sort((a, b) => String(a.version.model).localeCompare(String(b.version.model)) || Number(b.version.effective_from_secs) - Number(a.version.effective_from_secs));
    return <div className="table-scroll"><table className="table price-history">
      <thead><tr><th>模型</th>{PRICE_FIELDS.map(([field, label]) => <th key={field} className="num">{label}</th>)}<th className="num">倍率</th><th>采购价 / 百万</th><th>生效</th><th className="col-status">状态</th><th>版本</th></tr></thead>
      <tbody>
        {shown.map(({version, view}) => {
          const route = routeCostOf(version, providers);
          return <tr key={String(version.id)} className={view.label === '已被替代' ? 'is-muted' : undefined}>
          <td className="mono">{route ? <span title="这条线路（供应商 + 上游模型）的采购成本：只用来算成本和毛利，不是客户价格"><span className="tag tag-info">线路采购价</span> {String(route.provider.name ?? route.provider.id)} / {route.target}</span>
            : version.model === '*' ? <span title="这个价格表里没有单独定价的模型都按这一行扣费">*（其余所有模型）</span> : String(version.model ?? '—')}</td>
          {route ? <td colSpan={PRICE_FIELDS.length} className="muted">不用于扣费</td> : priceCells(version)}
          <td className="num">{!route && typeof version.margin_multiplier === 'number' && Number.isFinite(version.margin_multiplier) ? String(version.margin_multiplier) : '—'}</td>
          <td className="nowrap">{costText(version)}</td>
          <td title={Number.isFinite(Number(version.effective_from_secs)) && version.effective_from_secs !== null ? formatFullDateTime(Number(version.effective_from_secs)) : undefined}>
            {typeof version.effective_from_secs === 'number' && Number.isSafeInteger(version.effective_from_secs) ? formatDateTime(version.effective_from_secs) : '未提供有效时间'}</td>
          <td className="col-status"><StatusBadge view={view}/></td>
          <td><IdCell value={version.id} kind="generic"/></td>
        </tr>;})}
        {!shown.length && <tr><td colSpan={PRICE_FIELDS.length + 6} className="muted">没有生效中或已排期的版本</td></tr>}
      </tbody>
    </table></div>;
  };
  return <section className="panel" aria-label="价格版本">
    <div className="panel-head">
      <h3>价格版本</h3>
      <span className="muted">积分 / 百万 Tokens，倍率前{typeof faceValue === 'number' ? ` · 1 积分 = ¥${faceValue}（积分面值，见财务对账）` : ''}</span>
      <label className="check-field"><input type="checkbox" checked={history} onChange={event => setHistory(event.target.checked)}/> 显示历史（{superseded}）</label>
    </div>
    {tables.map(({id, card, use}) => {
      const title = <><b>{String(card?.name ?? id)}</b> <span className="mono muted">{String(id)}</span> <span className="muted">· {card ? users(use) : '价格表不存在'}</span></>;
      return use.internal
        ? <details key={String(id)} className="price-table is-internal" aria-label={`价格表 ${String(card?.name ?? id)}`}>
          <summary><span className="tag tag-outline">{use.internal}</span> {title}</summary>{table(id)}</details>
        : <section key={String(id)} className="price-table" aria-label={`价格表 ${String(card?.name ?? id)}`}><h4>{title}</h4>{table(id)}</section>;
    })}
    {!versions.length && <p className="muted">暂无价格版本</p>}
  </section>;
}
