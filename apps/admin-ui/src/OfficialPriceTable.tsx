// 官方价表: the vendors' list prices in USD per million tokens, one row per model name (a customer
// model, or the upstream model a route targets): input, output, 5-minute cache write, cache read,
// a note on where it comes from, and when it last changed. Rows are added, edited and deleted here,
// or pasted from CSV, and exported. Routes to a listed model are costed from its row; the preview
// shows the margins that moves and offers to re-derive the customer prices set from a changed row.
import {useEffect, useState} from 'react';
import {pricingNow} from './clock';
import {type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {formatClock, formatDateTime} from './format';
import {currentVersion, versionIdFor} from './priceChange';
import {freeTime, KINDS, MAX_OFFICIAL_PRICES, modelEntries, officialOf, officialVersion, OFFICIAL_FIELDS, pricingImpact, primaryCost, readSettings, usdOk, type Four} from './officialPricing';
import PricingPreview, {type PreviewPlan} from './PricingPreview';
import type {Publish} from './PricingSettings';
import {typedNumber, usdText} from './pricingText';
import {nameList} from './routes';

type Row = Record<string, unknown>;
interface Entry {usd: string[]; note: string}
const bytes = (value: string) => new TextEncoder().encode(value).length;
const plainText = (value: string, max: number) => !!value.trim() && bytes(value) <= max && !/[\x00-\x1f\x7f-\x9f]/.test(value);
const validReason = (value: string) => plainText(value.trim(), 500);
const serverNow = pricingNow;
const soon = () => Math.ceil((serverNow() + 60) / 60) * 60;
/** An entry as compared: its prices as numbers, and its note. */
const keyOf = (entry: Entry | undefined | null) => entry ? JSON.stringify({usd: entry.usd.map(value => typedNumber(value)), note: entry.note.trim()}) : 'null';
const draftOf = (settings: unknown): Record<string, Entry> => Object.fromEntries(Object.entries(readSettings(settings).official)
  .map(([name, price]) => [name, {usd: price.usd.map(String), note: price.note ?? ''}]));

/** An entry's four prices, checked; throws with the reason, naming the row. */
function pricesOf(name: string, entry: Entry): Four {
  return entry.usd.map((value, index) => {
    const number = typedNumber(value);
    if (number === null || !Number.isFinite(number) || !usdOk(number)) throw new Error(`${name} 的${KINDS[index]}官方价须在 0–10,000 美元之间（免费填 0）`);
    return number;
  }) as Four;
}

/**
 * Official prices pasted as CSV (model,input,output,cache_write,cache_read[,note]), with commas or
 * tabs, a header line allowed: the rows read, and the lines that could not be.
 */
export function parseOfficialCsv(text: string): {rows: Array<{name: string; usd: Four; note?: string}>; errors: string[]} {
  const rows: Array<{name: string; usd: Four; note?: string}> = [], errors: string[] = [];
  text.split(/\r?\n/).forEach((line, index) => {
    const cells = line.split(/\t|,/).map(cell => cell.trim());
    if (!line.trim() || (index === 0 && /^model$/i.test(cells[0]))) return;
    const [name, ...rest] = cells, numbers = rest.slice(0, 4).map(typedNumber);
    if (!name || !plainText(name, 256)) {errors.push(`第 ${index + 1} 行：模型名无效`); return;}
    if (numbers.length < 4 || numbers.some(value => value === null || !Number.isFinite(value) || !usdOk(value))) {errors.push(`第 ${index + 1} 行（${name}）：需要四个 0–10,000 的价格`); return;}
    rows.push({name, usd: numbers as Four, ...(rest[4] ? {note: rest.slice(4).join(', ')} : {})});
  });
  return {rows, errors};
}

export default function OfficialPriceTable({config, readAt, providers, sample, blocked, focus, onPublish, onReload, onDirtyChange}: {
  config: CommercialConfig;
  /** When the configuration was read (server seconds). */
  readAt?: number;
  providers: Row[];
  sample: (model: string) => Four;
  blocked?: string;
  /** A model name whose row to open, or start (先设官方价). */
  focus?: {name: string} | null;
  onPublish: Publish;
  /** Reads the configuration again after a conflict; the draft stays. */
  onReload: () => Promise<void>;
  onDirtyChange: (dirty: boolean) => void;
}) {
  const settings = (config.settings ?? {}) as unknown as Row;
  const published = readSettings(settings).official;
  const [draft, setDraft] = useState<Record<string, Entry>>(() => draftOf(settings));
  const [editing, setEditing] = useState<string | null>(null);
  const [adding, setAdding] = useState<{name: string; entry: Entry} | null>(null);
  const [csv, setCsv] = useState('');
  const [csvResult, setCsvResult] = useState('');
  const [reason, setReason] = useState('');
  const [error, setError] = useState('');
  const [conflict, setConflict] = useState(false);
  const [preview, setPreview] = useState<{plan: PreviewPlan; publish: (option: boolean) => Promise<void>} | null>(null);
  const [working, setWorking] = useState(false);
  const names = [...new Set([...Object.keys(published), ...Object.keys(draft)])].sort();
  const changed = names.filter(name => keyOf(draft[name]) !== keyOf(published[name] ? {usd: published[name].usd.map(String), note: published[name].note ?? ''} : null));
  const dirty = changed.length > 0 || !!adding || !!reason.trim();
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  // Another publication moved the table: a draft nobody has touched follows it.
  useEffect(() => {if (!dirty) setDraft(draftOf(settings));
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [config.revision]);
  useEffect(() => {if (!focus) return; if (draft[focus.name]) {setEditing(focus.name); setAdding(null);} else {setAdding({name: focus.name, entry: {usd: ['', '', '', ''], note: ''}}); setEditing(null);}
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focus]);
  // Who uses a row: customer models of that name, and routes to that upstream model.
  const usersOf = (name: string) => [...new Set(config.models.filter(model => model.exposed_model_id === name || model.target_model === name
    || (Array.isArray(model.fallback_chain) && model.fallback_chain.some(entry => (entry as Row)?.target_model === name))).map(model => String(model.exposed_model_id)))];
  const blockedReason = working ? '正在发布' : blocked ? blocked : !changed.length ? '官方价表没有修改' : !validReason(reason) ? '填写原因后可预览（最多约 160 字）' : undefined;

  const saveRow = (name: string, entry: Entry, replacing?: string) => {
    setError('');
    const trimmed = name.trim();
    try {
      if (!plainText(trimmed, 256)) throw new Error('模型名 1–256 字节，不含控制字符');
      if (replacing === undefined && draft[trimmed]) throw new Error(`官方价表里已有 ${trimmed}，直接编辑那一行`);
      pricesOf(trimmed, entry);
      if (entry.note && (bytes(entry.note) > 256 || /[\x00-\x1f\x7f-\x9f]/.test(entry.note))) throw new Error('备注最多 256 字节，不含控制字符');
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return false;}
    setDraft(current => ({...current, [trimmed]: {usd: entry.usd.map(value => value.trim()), note: entry.note.trim()}}));
    return true;
  };
  const applyCsv = () => {
    const parsed = parseOfficialCsv(csv);
    let added = 0, updated = 0;
    const next = {...draft};
    for (const row of parsed.rows) {
      const entry = {usd: row.usd.map(String), note: row.note ?? next[row.name]?.note ?? ''};
      if (!next[row.name]) added++; else if (JSON.stringify(next[row.name]) !== JSON.stringify(entry)) updated++;
      next[row.name] = entry;
    }
    setDraft(next);
    setCsvResult(`读到 ${parsed.rows.length} 行：新增 ${added}、更新 ${updated}${parsed.errors.length ? `；${parsed.errors.length} 行没读懂：${nameList(parsed.errors, 3)}` : ''}`);
    if (!parsed.errors.length) setCsv('');
  };
  const exportCsv = () => {
    const lines = ['model,input,output,cache_write,cache_read,note,updated_at', ...Object.entries(published).sort(([a], [b]) => a.localeCompare(b))
      .map(([name, price]) => [name, ...price.usd, JSON.stringify(price.note ?? ''), price.updatedAt ? new Date(price.updatedAt * 1000).toISOString() : ''].join(','))];
    const url = URL.createObjectURL(new Blob([`${lines.join('\n')}\n`], {type: 'text/csv'}));
    const link = document.createElement('a');
    link.href = url; link.download = `official-prices-${new Date().toISOString().slice(0, 10)}.csv`;
    document.body.appendChild(link); link.click(); link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 60000);
    toast.success('已导出官方价表');
  };

  const open = () => {
    setError(''); setConflict(false);
    let map: Record<string, Row>;
    try {
      map = Object.fromEntries(Object.entries(draft).map(([name, entry]) => [name, {...Object.fromEntries(OFFICIAL_FIELDS.map((field, index) => [field, pricesOf(name, entry)[index]])), ...(entry.note ? {note: entry.note} : {})}]));
    } catch (cause) {setError(cause instanceof Error ? cause.message : String(cause)); return;}
    if (Object.keys(map).length > MAX_OFFICIAL_PRICES) {setError(`官方价表最多 ${MAX_OFFICIAL_PRICES} 项，这次发布后会有 ${Object.keys(map).length} 项：先删掉不再用的行，再发布`); return;}
    const now = serverNow(), later = soon(), before = readSettings(settings);
    const payload = {credit_face_value_cny: settings.credit_face_value_cny, usd_cny_rate: settings.usd_cny_rate, official_prices: map};
    const after = readSettings({...settings, official_prices: map});
    // The customer prices set from a changed row: in force, computed from exactly its old prices.
    const rederive: Array<{version: Row; model: string}> = [], taken = config.versions.map(version => version.id), seen = new Set<string>();
    for (const name of changed.filter(item => published[item] && draft[item])) {
      const old = published[name].usd, next = after.official[name].usd;
      if (old.every((value, index) => value === next[index])) continue;
      for (const entry of modelEntries(config.models, config.groups)) for (const mapping of entry.mappings) {
        const rateCardId = config.groups.find(group => group.id === mapping.group_id)?.rate_card_id;
        if (typeof rateCardId !== 'string' || seen.has(`${rateCardId}\n${entry.id}`)) continue;
        const version = currentVersion(config.versions, rateCardId, [mapping.exposed_model_id, mapping.target_model], now), input = officialOf(version);
        if (!version || !input || !input.official.every((value, index) => value === old[index])) continue;
        seen.add(`${rateCardId}\n${entry.id}`);
        const at = freeTime(config.versions, [[rateCardId, version.model]], later), id = versionIdFor(String(version.model), at, taken);
        taken.push(id);
        rederive.push({model: entry.id, version: officialVersion({...input, official: next, basis: primaryCost(after, mapping, next).basis, usdCny: after.usdCny, face: after.face ?? input.face}, {id, rateCardId, model: String(version.model), effectiveSecs: at})});
      }
    }
    const context = {models: config.models, groups: config.groups, nowSecs: now, effectiveSecs: now, sample};
    const rows = pricingImpact({settings: before, versions: config.versions}, {settings: after, versions: config.versions}, context);
    const optionRows = rederive.length ? pricingImpact({settings: before, versions: config.versions}, {settings: after, versions: [...config.versions, ...rederive.map(entry => entry.version)]}, {...context, effectiveSecs: later}) : [];
    const describe = (name: string) => !draft[name] ? `删除 ${name}` : !published[name] ? `新增 ${name}：${draft[name].usd.map(value => usdText(Number(value))).join(' / ')}`
      : `${name}：${published[name].usd.map(usd => usdText(usd)).join(' / ')} → ${draft[name].usd.map(value => usdText(Number(value))).join(' / ')}${draft[name].note !== (published[name].note ?? '') ? '（备注已改）' : ''}`;
    const zeros = changed.filter(name => draft[name]).flatMap(name => draft[name].usd.map((value, index) => Number(value) === 0 ? `${name} 的${KINDS[index]}` : '').filter(Boolean));
    setPreview({
      plan: {title: `发布 ${changed.length} 项官方价修改？`, facts: [...changed.slice(0, 8).map(describe), ...(changed.length > 8 ? [`等 ${changed.length} 项`] : []), `原因：${reason.trim()}`, `基于 ${formatClock(readAt ?? now)} 读取的配置`], rows, zeros,
        option: rederive.length ? {label: `同时按新官方价重算这些模型的售价（约 1 分钟后生效）：${nameList([...new Set(rederive.map(entry => entry.model))])}`, rows: optionRows} : undefined,
        consequence: '发布后，线路到这些上游模型的请求按新官方价计算成本；客户价格只在勾选重算时改变。', confirmLabel: '发布', empty: '没有线路或模型的成本因此改变。'},
      publish: async option => {
        const update = {settings: payload, ...(option && rederive.length ? {versions: rederive.map(entry => entry.version)} : {})};
        setWorking(true);
        const outcome = await onPublish(update, reason.trim(), '发布', '已发布官方价表', '“官方价表”里的价格是否已是新的');
        setWorking(false);
        if (outcome.ok) {setReason(''); setEditing(null); return;}
        if (!outcome.uncertain) {setError(outcome.message); setConflict(!!outcome.conflict);}
      },
    });
  };

  const priceInputs = (name: string, entry: Entry, change: (entry: Entry) => void) => <>
    {KINDS.map((kind, index) => <td key={kind} className="num"><input aria-label={`${name || '新模型'} 官方${kind}价`} inputMode="decimal" value={entry.usd[index]}
      onChange={event => change({...entry, usd: entry.usd.map((value, i) => i === index ? event.target.value : value)})}/></td>)}
    <td><input aria-label={`${name || '新模型'} 备注`} placeholder="来源，如官网 9/20" value={entry.note} onChange={event => change({...entry, note: event.target.value})}/></td>
  </>;

  return <section className="panel official-table" aria-label="官方价表">
    <div className="panel-head">
      <h3>官方价表</h3>
      <span className="muted">美元 / 百万 Tokens · 缓存写按 5 分钟缓存 · 线路到这些上游模型的请求按它计算成本 · {Object.keys(draft).length} / {MAX_OFFICIAL_PRICES} 项</span>
      <span className="drawer-foot-spacer"/>
      <button type="button" className="btn btn-small" disabled={working || !!adding} onClick={() => {setAdding({name: '', entry: {usd: ['', '', '', ''], note: ''}}); setEditing(null);}}>＋ 新增</button>
      <button type="button" className="btn btn-small" disabled={!Object.keys(published).length} onClick={exportCsv}>导出 CSV</button>
    </div>
    <div className="table-scroll"><table className="table table-compact official-prices">
      <thead><tr><th>模型</th>{KINDS.map(kind => <th key={kind} className="num">{kind === '缓存写' ? <>缓存写<InfoTip text="5 分钟缓存的写入价；1 小时缓存另计，这里不含"/></> : kind}</th>)}<th>备注</th><th>更新</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
      <tbody>
        {adding && <tr className="is-selected">
          <td><input aria-label="新官方价模型名" placeholder="模型名，如 claude-opus-5" value={adding.name} onChange={event => setAdding({...adding, name: event.target.value})}/></td>
          {priceInputs(adding.name, adding.entry, entry => setAdding({...adding, entry}))}
          <td/>
          <td className="col-actions"><span className="row-actions">
            <button type="button" className="btn-text" onClick={() => {if (saveRow(adding.name, adding.entry)) setAdding(null);}}>加入</button>
            <button type="button" className="btn-text" onClick={() => setAdding(null)}>取消</button></span></td>
        </tr>}
        {names.map(name => {
          const entry = draft[name], price = published[name], users = usersOf(name);
          if (editing === name && entry) return <tr key={name} className="is-selected">
            <td className="mono">{name}</td>{priceInputs(name, entry, next => setDraft({...draft, [name]: next}))}<td/>
            <td className="col-actions"><span className="row-actions"><button type="button" className="btn-text" onClick={() => {if (saveRow(name, entry, name)) setEditing(null);}}>完成</button></span></td>
          </tr>;
          return <tr key={name} className={!entry ? 'is-muted' : changed.includes(name) ? 'is-edited' : undefined}>
            <td title={users.length ? `用到它的模型：${users.join('、')}` : '还没有模型或线路用到它'}><span className="mono">{name}</span>{changed.includes(name) && <span className="edited-dot">{!entry ? '将删除' : price ? '已修改' : '新增'}</span>}</td>
            {KINDS.map((kind, index) => <td key={kind} className="num">{entry ? usdText(Number(entry.usd[index])) : usdText(price?.usd[index])}</td>)}
            <td className="clip clip-reason" title={entry?.note ?? price?.note}>{(entry?.note ?? price?.note) || <span className="muted">—</span>}</td>
            <td className="nowrap">{price?.updatedAt ? formatDateTime(price.updatedAt) : <span className="muted">—</span>}</td>
            <td className="col-actions"><span className="row-actions">
              {entry ? <>
                <button type="button" className="btn-text" disabled={working} onClick={() => {setEditing(name); setAdding(null);}}>编辑</button>
                <button type="button" className="btn-text is-danger" disabled={working} title={users.length ? `用到它的模型：${users.join('、')}` : undefined}
                  onClick={() => {const next = {...draft}; delete next[name]; setDraft(next);}}>删除</button></>
                : <button type="button" className="btn-text" onClick={() => price && setDraft({...draft, [name]: {usd: price.usd.map(String), note: price.note ?? ''}})}>撤销</button>}
            </span></td>
          </tr>;
        })}
        {!names.length && !adding && <tr className="state-row"><td colSpan={8}><div className="list-state"><p>还没有官方价。点“＋ 新增”，或粘贴 CSV。</p></div></td></tr>}
      </tbody>
    </table></div>
    <details className="price-advanced official-csv">
      <summary>粘贴 CSV（model,input,output,cache_write,cache_read）</summary>
      <textarea aria-label="粘贴官方价 CSV" className="code-input csv-input" placeholder={'model,input,output,cache_write,cache_read\nclaude-opus-5,5,25,6.25,0.5'} value={csv} onChange={event => setCsv(event.target.value)}/>
      <div className="button-row"><button type="button" className="btn btn-small" disabled={!csv.trim()} onClick={applyCsv}>套用到表格</button>{csvResult && <span role="status" className="muted">{csvResult}</span>}</div>
    </details>
    <div className="editor-actions">
      {error && <div role="alert" className="message message-error"><p>{error}</p>{conflict && <button type="button" className="btn btn-small" disabled={working} onClick={() => void onReload().then(() => {setError(''); setConflict(false);})}>重新加载</button>}</div>}
      <div className="button-row settings-actions">
        {changed.length > 0 && <span className="dirty-dot">{changed.length} 项修改未发布</span>}
        <input className="action-bar-reason" aria-label="官方价变更原因" placeholder="变更原因（必填）" maxLength={500} value={reason} disabled={working} onChange={event => setReason(event.target.value)}/>
        {dirty && <button type="button" className="btn" disabled={working} onClick={async () => {
          if (!(await confirmAction({title: '放弃未发布的修改？', consequence: '官方价表回到服务器上的内容。', confirmLabel: '放弃修改'}))) return;
          setDraft(draftOf(settings)); setAdding(null); setEditing(null); setReason(''); setError(''); setConflict(false);
        }}>放弃修改</button>}
        <button type="button" className="btn btn-primary" disabled={!!blockedReason} title={blockedReason} onClick={open}>{working ? '发布中…' : '预览并发布'}</button>
      </div>
    </div>
    {preview && <PricingPreview plan={preview.plan} providers={providers} groups={config.groups} onCancel={() => setPreview(null)}
      onConfirm={option => {const run = preview.publish; setPreview(null); void run(option);}}/>}
  </section>;
}
