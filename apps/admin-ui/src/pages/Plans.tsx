// 套餐: the plan catalog cards are issued from — what a card sells for, gives and shows the customer,
// and the group it goes into by default — published under the commercial configuration's revision
// check. A plan cards were issued from can only be taken off sale; one of more than one device can
// be kept but not issued from yet. An unconfirmed publication locks editing until the list is checked.
import {useEffect, useRef, useState, type InputHTMLAttributes, type ReactElement} from 'react';
import {adminApi, type Plan} from '../api';
import {ask, confirmAction} from '../components/confirm';
import {Modal} from '../components/modal';
import {toast} from '../components/toast';
import {StatusBadge, TableState, TopbarActions} from '../components/ui';
import {formatCount, formatMoney} from '../format';
import {customerView, draftOf, KIRO_PLAN_TYPES, kiroLabel, MAX_PLANS, MULTI_DEVICE_NOTE, newDraft, ONE_DEVICE, parsePlan, PLAN_CHANGE_KEY, planChanges, priceMicro, type PlanDraft} from '../plans';
import {publishFailure} from '../refusal';
import type {Refresh, ReportError, Row, WriteGuards} from '../types';

const REASON_MAX = 160;
const errorText = (error: unknown) => (error instanceof Error ? error.message : String(error));
const readPending = (): {what: string} | null => {
  try {const value = JSON.parse(sessionStorage.getItem(PLAN_CHANGE_KEY) || 'null'); return value && typeof value.what === 'string' ? value : null;} catch {return null;}
};

export default function PlansPage({plans, editable, cardsByPlan, groups, revision, loading, failed, refresh, guards, reportError, onDirtyChange, onBusyChange}: {
  /** The catalog in force, in its order (the four tiers when the server keeps none). */
  plans: Plan[];
  /** The server keeps a catalog it can publish (newer servers); otherwise the tiers are shown read-only. */
  editable: boolean;
  cardsByPlan: Record<string, number> | null;
  groups: Row[];
  revision?: string;
  loading: boolean;
  failed: boolean;
  refresh: Refresh;
  guards: WriteGuards;
  reportError: ReportError;
  onDirtyChange: (dirty: boolean) => void;
  onBusyChange: (busy: boolean) => void;
}) {
  const alive = useRef(true);
  useEffect(() => {alive.current = true; return () => {alive.current = false;};}, []);
  const [editing, setEditing] = useState<{original?: Plan; draft: PlanDraft; reason: string} | null>(null);
  const [formError, setFormError] = useState('');
  const [busy, setBusy] = useState(false);
  const [pending, setPending] = useState(readPending);
  const [checked, setChecked] = useState(false);
  const groupIds = groups.map(group => String(group.id));
  const groupName = (id: string) => String(groups.find(group => group.id === id)?.name ?? id);
  const planName = (id: string) => plans.find(plan => plan.id === id)?.name ?? id;
  const dirty = !!editing && (!editing.original || JSON.stringify(draftOf(editing.original)) !== JSON.stringify(editing.draft) || !!editing.reason.trim());
  useEffect(() => {onDirtyChange(dirty); return () => onDirtyChange(false);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  const blocked = !editable ? '服务器还不支持编辑套餐' : pending ? '上次套餐发布的结果未确认，请先核对' : failed ? '配置没有加载成功，请刷新后再试' : !revision ? '配置还没有加载' : undefined;

  /** One publication of plans: locked while unconfirmed, and read back from the server either way. */
  const publish = async (change: {plans?: Plan[]; removed_plans?: string[]}, reason: string, what: string): Promise<boolean> => {
    if (guards.writing.current || busy || blocked) return false;
    try {sessionStorage.setItem(PLAN_CHANGE_KEY, JSON.stringify({what, at: new Date().toISOString()}));}
    catch {reportError('浏览器存储不可用，无法在结果未确认时提醒核对，已取消发布'); return false;}
    guards.writing.current = true; setBusy(true); setFormError(''); reportError('');
    try {
      const result = await adminApi.publishCommercialConfig({expected_revision: revision, reason, ...change});
      if (result.success !== true) throw new Error('服务器未确认发布结果');
      try {sessionStorage.removeItem(PLAN_CHANGE_KEY);} catch {/* the panel asks to check it */}
      toast.success(`已发布：${what}`);
      void refresh({keepSelection: true});
      return true;
    } catch (error) {
      const failure = publishFailure(error, '发布', planName);
      if (!failure.uncertain) {
        try {sessionStorage.removeItem(PLAN_CHANGE_KEY);} catch {/* nothing was changed */}
        if (alive.current) setFormError(failure.message);
        if (!editing) reportError(failure.message);
        if (failure.conflict) void refresh({keepSelection: true});
        return false;
      }
      if (alive.current) {setPending({what}); setChecked(false); setEditing(null);}
      reportError(`没收到套餐发布的结果（${errorText(error)}）：可能已经发布。请刷新核对套餐列表，不要重复发布。`);
      return false;
    } finally {guards.writing.current = false; if (alive.current) setBusy(false);}
  };

  const save = async () => {
    if (!editing) return;
    const parsed = parsePlan(editing.draft, {groupIds, takenIds: editing.original ? [] : plans.map(plan => plan.id)});
    if (parsed.errors) {setFormError('请先改正标出的项'); return;}
    const reason = editing.reason.trim();
    if (!reason) {setFormError('请填写变更原因'); return;}
    const changes = planChanges(editing.original, parsed.plan, groupName);
    if (!changes.length) {setFormError('没有修改'); return;}
    if (!editing.original && plans.length >= MAX_PLANS) {setFormError(`最多只能有 ${MAX_PLANS} 个套餐`); return;}
    const confirmed = await confirmAction({title: editing.original ? `发布套餐「${parsed.plan.name}」的修改？` : `发布新套餐「${parsed.plan.name}」？`,
      facts: [...changes, `原因：${reason}`, ...(parsed.plan.max_devices > 1 ? [MULTI_DEVICE_NOTE] : [])],
      consequence: editing.original ? '已发出的卡不变：每张卡保留发卡时的套餐（积分、售价、有效期和并发）。以后发的卡按新的内容。' : '发布后可以在“批量生成”里选它发卡。',
      confirmLabel: '发布'});
    if (!confirmed || !alive.current) return;
    if (await publish({plans: [parsed.plan]}, reason, `${editing.original ? '修改' : '新建'}套餐「${parsed.plan.name}」`) && alive.current) setEditing(null);
  };

  const toggleSale = async (plan: Plan) => {
    const next = {...plan, on_sale: !plan.on_sale};
    const answer = await ask({title: plan.on_sale ? `下架套餐「${plan.name}」？` : `重新上架套餐「${plan.name}」？`,
      facts: [customerView(plan), `已发卡 ${formatCount(cardsByPlan?.[plan.id] ?? 0)} 张`],
      consequence: plan.on_sale ? '下架后不能再从它发卡；已发出的卡照常使用。' : '上架后可以在“批量生成”里选它发卡。',
      confirmLabel: plan.on_sale ? '下架' : '上架', reason: {label: '变更原因', maxLength: REASON_MAX, required: true}});
    if (!answer.confirmed || !alive.current) return;
    await publish({plans: [next]}, answer.reason.trim(), `${plan.on_sale ? '下架' : '上架'}套餐「${plan.name}」`);
  };

  const remove = async (plan: Plan) => {
    const answer = await ask({title: `删除套餐「${plan.name}」？`, facts: [customerView(plan), '还没有从它发过卡'],
      consequence: '删除后不能再选它发卡；要再用，需要重新新建。', confirmLabel: '删除', danger: true,
      reason: {label: '变更原因', maxLength: REASON_MAX, required: true}});
    if (!answer.confirmed || !alive.current) return;
    await publish({removed_plans: [plan.id]}, answer.reason.trim(), `删除套餐「${plan.name}」`);
  };

  const recheck = async () => {
    if (busy) return;
    setBusy(true);
    try {await refresh({keepSelection: true}); if (alive.current) setChecked(true);}
    finally {if (alive.current) setBusy(false);}
  };
  const release = async () => {
    if (!(await confirmAction({title: '已核对套餐列表？', consequence: '如果上次的修改已经生效，请不要再发布一次。', confirmLabel: '继续修改'}))) return;
    try {sessionStorage.removeItem(PLAN_CHANGE_KEY); setPending(null); reportError('');}
    catch {reportError('无法清除待核对记录，仍不能修改套餐。请检查浏览器存储。');}
  };

  const firstGroup = groupIds.find(id => groups.find(group => group.id === id)?.issuance_enabled !== false) ?? groupIds[0] ?? '';
  const parsed = editing ? parsePlan(editing.draft, {groupIds, takenIds: editing.original ? [] : plans.map(plan => plan.id)}) : null;
  const errors = parsed?.errors ?? {};
  const set = (field: keyof PlanDraft, value: string | boolean) => {setFormError(''); setEditing(current => current && {...current, draft: {...current.draft, [field]: value}});};
  const field = (name: keyof PlanDraft, label: string, input: ReactElement, hint?: string) => <label className="field"><span className="field-label">{label}</span>{input}
    {errors[name] ? <span className="field-error">{errors[name]}</span> : hint ? <span className="field-hint">{hint}</span> : null}</label>;
  const numberInput = (name: keyof PlanDraft, label: string, extra: InputHTMLAttributes<HTMLInputElement> = {}) =>
    <input inputMode="numeric" aria-label={label} aria-invalid={!!errors[name]} value={String(editing?.draft[name] ?? '')} disabled={busy} onChange={event => set(name, event.target.value)} {...extra}/>;

  return <div className="page-stack">
    <TopbarActions>
      <button type="button" className="btn btn-primary" disabled={!!blocked || busy} title={blocked}
        onClick={() => {setFormError(''); setEditing({draft: newDraft(plans, firstGroup), reason: ''});}}>＋ 新建套餐</button>
    </TopbarActions>

    {pending && <section className="recovery-panel" aria-label="套餐发布结果核对">
      <div>
        <h3>上次套餐发布的结果未确认</h3>
        <p>可能已经发布：{pending.what}。刷新后核对下面的列表，再继续修改。</p>
      </div>
      <div className="button-row">
        <button type="button" className="btn btn-small" disabled={busy || loading} onClick={() => void recheck()}>{busy ? '正在刷新…' : '刷新列表'}</button>
        <button type="button" className="btn btn-small" disabled={busy || !checked} title={checked ? undefined : '先刷新列表'} onClick={() => void release()}>已核对，继续</button>
      </div>
    </section>}

    {!editable && !loading && !failed && <p className="note-info" role="note">服务器还不支持编辑套餐：下面是它发卡用的四档（每张 30 天、1 台设备、同时 2 个请求）。更新服务器后可以在这里新建、修改和下架套餐。</p>}

    <section className="panel" aria-label="套餐列表">
      <div className="panel-head"><h3>套餐</h3><span className="muted">客户端显示套餐名称和 Kiro 档位；每张卡保留发卡时的套餐内容，改套餐不影响已发出的卡</span></div>
      <div className="table-scroll"><table className="table plans-table">
        <thead><tr><th>套餐</th><th className="num">积分</th><th className="num">售价</th><th className="num">有效期</th><th className="num">设备</th><th className="num">并发</th>
          <th>默认分组</th><th>Kiro 显示</th><th>状态</th><th className="num">排序</th><th className="num">已发卡</th><th className="col-actions"><span className="sr-only">操作</span></th></tr></thead>
        <tbody>
          {plans.map(plan => {
            const issued = cardsByPlan?.[plan.id] ?? 0;
            return <tr key={plan.id}>
              <td><span className="cell-strong">{plan.name}</span> <span className="mono muted">{plan.id}</span></td>
              <td className="num">{formatCount(plan.points)}</td>
              <td className="num">{formatMoney(priceMicro(plan))}</td>
              <td className="num">{plan.validity_days} 天</td>
              <td className="num">{plan.max_devices} 台{plan.max_devices > 1 && <span className="tag tag-warning plan-tag" title={MULTI_DEVICE_NOTE}>暂不能发卡</span>}</td>
              <td className="num">{plan.concurrency}</td>
              <td>{groups.some(group => group.id === plan.default_group_id) ? groupName(plan.default_group_id) : <span className="is-warning" title={plan.default_group_id}>分组不存在</span>}</td>
              <td title={plan.kiro_plan_type}>{kiroLabel(plan.kiro_plan_type)}</td>
              <td><StatusBadge view={plan.on_sale ? {label: '在售', tone: 'success'} : {label: '已下架', tone: 'neutral'}}/></td>
              <td className="num">{plan.sort_order}</td>
              <td className="num">{cardsByPlan ? formatCount(issued) : '—'}</td>
              <td className="col-actions"><span className="row-actions">
                <button type="button" className="btn-text" disabled={!!blocked || busy} title={blocked} onClick={() => {setFormError(''); setEditing({original: plan, draft: draftOf(plan), reason: ''});}}>编辑</button>
                <button type="button" className="btn-text" disabled={!!blocked || busy} title={blocked} onClick={() => void toggleSale(plan)}>{plan.on_sale ? '下架' : '上架'}</button>
                <button type="button" className="btn-text is-danger" disabled={!!blocked || busy || issued > 0 || !cardsByPlan}
                  title={blocked ?? (!cardsByPlan ? '不知道从它发过多少卡' : issued > 0 ? `已经从它发过 ${formatCount(issued)} 张卡，只能下架` : undefined)} onClick={() => void remove(plan)}>删除</button>
              </span></td>
            </tr>;
          })}
          {!plans.length && <TableState colSpan={12} loading={loading} failed={failed} empty="还没有套餐" onRetry={() => void refresh()}/>}
        </tbody>
      </table></div>
      <p className="muted plans-note">{ONE_DEVICE}。下架的套餐不能再发卡，已发出的卡照常使用；发过卡的套餐不能删除。</p>
    </section>

    {editing && <Modal label={editing.original ? '编辑套餐' : '新建套餐'} onClose={() => setEditing(null)} busy={busy} className="dialog-form plan-editor">
      <h3 className="modal-title">{editing.original ? `编辑套餐 · ${editing.original.name}` : '新建套餐'}</h3>
      {formError && <p role="alert" className="form-error">{formError}</p>}
      <div className="form-grid">
        {field('id', '套餐 ID', <input aria-label="套餐 ID" aria-invalid={!!errors.id} className="mono" value={editing.draft.id} disabled={busy || !!editing.original} placeholder="例：trial-7d"
          onChange={event => set('id', event.target.value)}/>, editing.original ? '发布后不能改' : '小写字母、数字和 -，发布后不能改')}
        {field('name', '名称', <input aria-label="套餐名称" aria-invalid={!!errors.name} value={editing.draft.name} disabled={busy} placeholder="例：体验卡" onChange={event => set('name', event.target.value)}/>, '客户端显示的套餐名')}
        {field('points', '积分', numberInput('points', '套餐积分'))}
        {field('price', '售价（元）', numberInput('price', '套餐售价', {inputMode: 'decimal'}), '财务对账按这个价格计算销售额')}
        {field('validityDays', '有效期（天）', numberInput('validityDays', '有效期天数'), '从激活起算')}
        {field('maxDevices', '设备数', numberInput('maxDevices', '设备数'))}
        {field('concurrency', '并发', numberInput('concurrency', '并发请求数'), '同时进行的请求数')}
        {field('defaultGroupId', '默认分组', <select aria-label="默认分组" aria-invalid={!!errors.defaultGroupId} value={editing.draft.defaultGroupId} disabled={busy} onChange={event => set('defaultGroupId', event.target.value)}>
          <option value="" disabled>请选择分组</option>
          {groups.map(group => <option key={String(group.id)} value={String(group.id)}>{String(group.name ?? group.id)}{group.issuance_enabled === false ? '（不可发新卡）' : ''}</option>)}
        </select>, '批量生成选这个套餐时先选中这个分组；也可以发到别的分组')}
        {field('kiroPlanType', 'Kiro 显示', <select aria-label="Kiro 显示" value={editing.draft.kiroPlanType} disabled={busy} onChange={event => set('kiroPlanType', event.target.value)}>
          {KIRO_PLAN_TYPES.map(type => <option key={type} value={type}>{kiroLabel(type)}（{type}）</option>)}
        </select>, 'Kiro 里显示的订阅档位')}
        {field('sortOrder', '排序', numberInput('sortOrder', '排序', {inputMode: 'text'}), '小的排在前面')}
        <label className="check-field plan-sale"><input type="checkbox" checked={editing.draft.onSale} disabled={busy} onChange={event => set('onSale', event.target.checked)}/>在售（可以发卡）</label>
      </div>
      {parsed?.plan && <p className="plan-preview" aria-label="客户看到">客户看到：{customerView(parsed.plan)}</p>}
      <label className="field"><span className="field-label">变更原因</span>
        <input aria-label="变更原因" maxLength={REASON_MAX} placeholder="例：新增 7 天体验卡" value={editing.reason} disabled={busy}
          onChange={event => {setFormError(''); setEditing(current => current && {...current, reason: event.target.value});}}/></label>
      <div className="modal-actions">
        <button type="button" className="btn" disabled={busy} onClick={() => setEditing(null)}>取消</button>
        <button type="button" className="btn btn-primary" disabled={busy || !!blocked} title={blocked} onClick={() => void save()}>{busy ? '发布中…' : '发布'}</button>
      </div>
    </Modal>}
  </div>;
}
