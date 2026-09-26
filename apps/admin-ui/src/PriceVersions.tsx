// Price versions, one row each: what a model costs, from when, and whether it is the one in
// force. Superseded versions stay one click away (显示历史).
import {useState} from 'react';
import {IdCell, StatusBadge} from './components/ui';
import {formatDateTime, formatFullDateTime} from './format';
import {COST_FIELDS, creditsText, PRICE_FIELDS} from './priceChange';
import {priceVersionView} from './status';

type Row = Record<string, unknown>;

function priceCells(version: Row) {
  if (version.pricing_mode === 'fixed') return PRICE_FIELDS.map(([field]) => <td key={field} className="num">{creditsText(version[field]) ?? <span className="is-warning" title="无法安全显示，请核对原始配置">?</span>}</td>);
  const text = version.pricing_mode === 'per_call' ? `按次 ${creditsText(version.per_call_credit) ?? '?'} 积分`
    : version.pricing_mode === 'cost_plus' ? '成本加成（按采购价计费）' : `未知计费方式：${String(version.pricing_mode ?? '—')}`;
  return [<td key="mode" colSpan={PRICE_FIELDS.length} className="muted">{text}</td>];
}

function costText(version: Row): string {
  if (!['USD', 'CNY'].includes(String(version.currency))) return '—';
  const values = COST_FIELDS.map(([field]) => typeof version[field] === 'number' && Number.isFinite(version[field]) && Number(version[field]) >= 0 ? String(version[field]) : '—');
  return `${String(version.currency)} ${values.join(' / ')}`;
}

export default function PriceVersions({versions, rateCardName, nowSecs}: {versions: Row[]; rateCardName: (id: unknown) => string; nowSecs: number}) {
  const [history, setHistory] = useState(false);
  const rows = versions.map(version => ({version, view: priceVersionView(version, versions, nowSecs)}));
  const superseded = rows.filter(row => row.view.label === '已被替代').length;
  const shown = rows.filter(row => history || row.view.label !== '已被替代')
    .sort((a, b) => String(a.version.model).localeCompare(String(b.version.model)) || Number(b.version.effective_from_secs) - Number(a.version.effective_from_secs));
  return <section className="panel" aria-label="价格版本">
    <div className="panel-head">
      <h3>价格版本</h3>
      <label className="check-field"><input type="checkbox" checked={history} onChange={event => setHistory(event.target.checked)}/> 显示历史（{superseded}）</label>
    </div>
    <div className="table-scroll"><table className="table price-history">
      <thead><tr><th>模型</th>{PRICE_FIELDS.map(([field, label]) => <th key={field} className="num">{label}</th>)}<th className="num">倍率</th><th>采购价 / 百万</th><th>生效</th><th className="col-status">状态</th><th>版本</th></tr></thead>
      <tbody>
        {shown.map(({version, view}) => <tr key={String(version.id)} className={view.label === '已被替代' ? 'is-muted' : undefined}>
          <td className="mono" title={`价格表：${rateCardName(version.rate_card_id)}`}>{String(version.model ?? '—')}</td>
          {priceCells(version)}
          <td className="num">{typeof version.margin_multiplier === 'number' && Number.isFinite(version.margin_multiplier) ? String(version.margin_multiplier) : '—'}</td>
          <td className="nowrap">{costText(version)}</td>
          <td title={Number.isFinite(Number(version.effective_from_secs)) && version.effective_from_secs !== null ? formatFullDateTime(Number(version.effective_from_secs)) : undefined}>
            {typeof version.effective_from_secs === 'number' && Number.isSafeInteger(version.effective_from_secs) ? formatDateTime(version.effective_from_secs) : '未提供有效时间'}</td>
          <td className="col-status"><StatusBadge view={view}/></td>
          <td><IdCell value={version.id} kind="generic"/></td>
        </tr>)}
        {!shown.length && <tr><td colSpan={PRICE_FIELDS.length + 6} className="muted">{versions.length ? '没有生效中或已排期的版本' : '暂无价格版本'}</td></tr>}
      </tbody>
    </table></div>
    <p className="muted">积分 / 百万 Tokens，倍率前。</p>
  </section>;
}
