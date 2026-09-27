// 预览: every pricing change ends here before anything is sent. One table of each model it touches
// (per price table, naming every group charged from it): credits per million before → after for
// all four kinds with the change, what customers pay in ¥, each route's margin before → after, and
// any scheduled price that would take over. A loss on any route (backups included) or a change
// above 30% is marked and needs the model named; a route whose cost is at or above the price, and
// an official price left at 0, must be ticked as intended. Then one publication.
import {useState, type ReactNode} from 'react';
import {Modal} from './components/modal';
import {formatFullDateTime} from './format';
import {KINDS, largestChange, type Four, type ImpactRow, type RouteState} from './officialPricing';
import {creditsText, percentChange} from './priceChange';
import {marginText, marginTone, providerName, yuanText} from './pricingText';
import {nameList} from './routes';

type Row = Record<string, unknown>;

export interface PreviewPlan {
  title: string;
  facts: ReactNode[];
  /** Every model the change touches; unchanged rows are left out. */
  rows: ImpactRow[];
  /** A choice offered with the change, and the rows when it is ticked. */
  option?: {label: string; rows: ImpactRow[]};
  /** Official prices left at 0, each to be ticked as intended ("claude-x 的缓存写"). */
  zeros?: string[];
  /** A face value change moves every official price's credits on purpose: only losses are marked. */
  creditsMove?: boolean;
  extra?: ReactNode;
  consequence: ReactNode;
  confirmLabel: string;
  /** Said when no model is touched. */
  empty?: string;
}

/** Why a row is marked: a route that loses money, or a change above 30%. */
export function rowFlags(row: ImpactRow, creditsMove = false): string[] {
  const flags: string[] = [];
  if (row.after.routes.some(route => route.margin !== null && route.margin < 0)) flags.push('有线路亏损');
  const change = largestChange(row.before.credits, row.after.credits);
  if (!creditsMove && row.before.credits && change > 30) flags.push(change === Infinity ? '新增收费项' : `变化 ${Math.round(change)}%`);
  return flags;
}

/**
 * Routes whose basis × 成本倍率 is at or above what customers pay for some kind. (A route still
 * costed from price versions is caught by its margin instead.)
 */
export function costlyRoutes(row: ImpactRow): RouteState[] {
  const price = row.after.yuanPerM;
  return price ? row.after.routes.filter(route => route.cost.how === 'official' && route.cost.perM?.some((cost, kind) => cost > 0 && cost >= price[kind])) : [];
}

const routeLabel = (route: RouteState, index: number, providers: Row[]) => `${route.primary ? '主' : `备${index}`} ${providerName(providers, route.target.provider_id)} / ${route.target.target_model}`;

function Change({before, after}: {before: number | null; after: number | null}) {
  const was = before === null ? '—' : creditsText(before) ?? '?', will = after === null ? '—' : creditsText(after) ?? '?';
  if (before === after) return <>{will}</>;
  const change = percentChange(before, after);
  return <>{was} → <b>{will}</b> <span className={change.startsWith('+') ? 'is-up' : change.startsWith('−') ? 'is-down' : 'muted'}>{change}</span></>;
}

const yuanPair = (values: Four | null) => values ? `${yuanText(values[0])} / ${yuanText(values[1])}` : '—';

export default function PricingPreview({plan, providers, groups, onCancel, onConfirm}: {
  plan: PreviewPlan;
  providers: Row[];
  groups: Row[];
  onCancel: () => void;
  onConfirm: (option: boolean) => void;
}) {
  const [option, setOption] = useState(false);
  const [typed, setTyped] = useState('');
  const [ticked, setTicked] = useState<string[]>([]);
  const rows = (option && plan.option ? plan.option.rows : plan.rows).filter(row => row.changed);
  const groupNames = (row: ImpactRow) => row.mappings.map(mapping => String(groups.find(group => group.id === mapping.group_id)?.name ?? mapping.group_id));
  const flagged = rows.filter(row => rowFlags(row, plan.creditsMove).length);
  const costly = rows.flatMap(row => costlyRoutes(row).map(route => `${row.model} 经 ${providerName(providers, route.target.provider_id)} / ${route.target.target_model} 的成本不低于售价`));
  const checks = [...costly, ...(plan.zeros ?? []).map(zero => `${zero}官方价是 0（确实免费）`)];
  const word = flagged.length === 1 ? flagged[0].model : flagged.length > 1 ? String(flagged.length) : '';
  const typedReady = !word || typed.trim() === word, checksReady = checks.every(check => ticked.includes(check));
  const ready = typedReady && checksReady;
  const danger = flagged.length > 0 || checks.length > 0;
  return <Modal role="alertdialog" label={plan.title} onClose={onCancel} className="confirm-dialog pricing-preview-dialog">
    <h3 className="modal-title">{plan.title}</h3>
    {plan.facts.length > 0 && <ul className="confirm-facts">{plan.facts.map((fact, index) => <li key={index}>{fact}</li>)}</ul>}
    {plan.option && <label className="check-field confirm-option"><input type="checkbox" checked={option} onChange={event => setOption(event.target.checked)}/>{plan.option.label}</label>}
    {rows.length ? <div className="table-scroll preview-scroll"><table className="table table-compact preview-table" aria-label="受影响的模型">
      <thead><tr><th>模型 · 同一价格表的分组</th>{KINDS.map(kind => <th key={kind} className="num">{kind}</th>)}<th className="num">¥ / 百万（入 / 出）</th><th>各线路毛利</th></tr></thead>
      <tbody>{rows.map(row => {
        const flags = rowFlags(row, plan.creditsMove);
        return <tr key={`${row.rateCardId}\n${row.model}`} className={flags.length ? 'is-flagged' : undefined}>
          <td><span className="mono">{row.model}</span><span className="preview-groups">{nameList(groupNames(row), 4)}</span>
            {flags.length > 0 && <span className="preview-flag">{flags.join('、')}</span>}
            {row.overriddenBy && <span className="field-warning">已有 {formatFullDateTime(Number(row.overriddenBy.effective_from_secs))} 起的排期价格，会覆盖这次修改</span>}</td>
          {KINDS.map((kind, index) => <td key={kind} className="num nowrap"><Change before={row.before.credits?.[index] ?? null} after={row.after.credits?.[index] ?? null}/></td>)}
          <td className="num nowrap">{yuanPair(row.before.yuanPerM) === yuanPair(row.after.yuanPerM) ? yuanPair(row.after.yuanPerM) : <>{yuanPair(row.before.yuanPerM)} → <b>{yuanPair(row.after.yuanPerM)}</b></>}</td>
          <td><ul className="preview-routes">{row.after.routes.map((route, index) => {
            const was = row.before.routes[index]?.margin ?? null;
            return <li key={index}><span className="muted">{routeLabel(route, index, providers)}</span>{' '}
              {was === route.margin ? <span className={marginTone(route.margin)}>{marginText(route.margin)}</span>
                : <><span className={marginTone(was)}>{marginText(was)}</span> → <b className={marginTone(route.margin)}>{marginText(route.margin)}</b></>}
              {route.cost.how === 'legacy' && <span className="muted">（旧版采购价）</span>}{route.cost.how === null && <span className="is-warning">（成本未知）</span>}</li>;
          })}</ul></td>
        </tr>;
      })}</tbody>
    </table></div> : <p className="muted">{plan.empty ?? '没有模型的价格或毛利因此改变。'}</p>}
    {plan.extra}
    {checks.length > 0 && <div className="preview-checks" role="group" aria-label="需要确认">
      {checks.map(check => <label key={check} className="check-field"><input type="checkbox" checked={ticked.includes(check)}
        onChange={event => setTicked(current => event.target.checked ? [...current, check] : current.filter(item => item !== check))}/>{check}</label>)}
    </div>}
    <p className={`confirm-consequence${danger ? ' is-danger' : ''}`}>{plan.consequence}</p>
    {word && <div className="field">
      <label className="field-label" htmlFor="preview-typed">{flagged.length === 1 ? <>这个模型有线路亏损或变化超过 30%：输入 <b className="mono">{word}</b> 确认</> : <>{flagged.length} 个模型有线路亏损或变化超过 30%：输入 <b className="mono">{word}</b> 确认</>}</label>
      <input id="preview-typed" aria-label="确认输入" value={typed} autoComplete="off" spellCheck={false} onChange={event => setTyped(event.target.value)}/>
    </div>}
    <div className="modal-actions">
      <button type="button" className="btn" data-autofocus onClick={onCancel}>取消</button>
      <button type="button" className={danger ? 'btn btn-danger-solid' : 'btn btn-primary'} data-confirm="accept" disabled={!ready}
        title={ready ? undefined : !checksReady ? '先勾选上面每一项' : `输入 ${word} 后可以${plan.confirmLabel}`} onClick={() => {if (ready) onConfirm(option);}}>{plan.confirmLabel}</button>
    </div>
  </Modal>;
}
