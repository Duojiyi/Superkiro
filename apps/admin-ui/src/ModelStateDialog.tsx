// 隐藏 / 下架 / 重新上架 / 删除 for one customer model: every group it is in by default, each one
// named and each one can be left out. Hiding or retiring says how much the model was used in the
// last 7 days and when last, and can pass its requests to another model (请求转给): the old
// entries are removed and the old ID becomes an alias of that model, in the same publication.
import {useState} from 'react';
import {Modal} from './components/modal';
import {formatCount, formatDateTime} from './format';
import {defaultModel} from './listing';
import type {StateAction} from './ModelSheet';
import {isLive, nameList} from './routes';
import {modelStateView} from './status';

type Row = Record<string, unknown>;

export interface StateChoice {mappings: Row[]; reason: string; successor: string | null}

const PLAN = {
  hide: {label: '隐藏', consequence: '客户的模型列表里不再显示；已经在用这个模型 ID 的客户仍可继续调用，照常扣费。', placeholder: '例：先不对新客户开放'},
  retire: {label: '下架', consequence: '不再显示，所有请求都会被拒绝，包括已经在用的客户。线路和价格都保留，可以随时重新上架。', placeholder: '例：上游停止供应'},
  restore: {label: '重新上架', consequence: '重新出现在客户的模型列表里，可以正常调用。', placeholder: '例：恢复供应'},
  remove: {label: '删除', consequence: '删除这个模型条目，不能撤销：要再卖需要重新上架。它的价格版本仍留在价格表里。', placeholder: '例：不再供应'},
} as const;

export default function ModelStateDialog({id, mappings, action, models, groups, versionsOf, usage, lastUse, blockedOf, onCancel, onConfirm}: {
  id: string;
  /** The model's published entries, one per group. */
  mappings: Row[];
  action: StateAction;
  /** Every published entry, for defaults and successors. */
  models: Row[];
  groups: Row[];
  /** Whether a price table still prices this model ID (its requests, as an alias, are charged at that price). */
  versionsOf: (groupId: unknown) => boolean;
  /** The last 7 days, when the server reports them. */
  usage: {requests: number; cards: number} | null | undefined;
  /** The latest request among the recent ones read, or null. */
  lastUse: number | null;
  /** Why an entry cannot be re-listed (its route or price), for 重新上架. */
  blockedOf: (mapping: Row) => string;
  onCancel: () => void;
  onConfirm: (choice: StateChoice) => void;
}) {
  const plan = PLAN[action];
  const eligible = (mapping: Row) => action === 'hide' ? isLive(mapping) : action === 'retire' ? mapping.retired !== true : action === 'restore' ? !isLive(mapping) && !blockedOf(mapping) : !isLive(mapping);
  const [chosen, setChosen] = useState<string[]>(() => mappings.filter(eligible).map(mapping => String(mapping.id)));
  const [successor, setSuccessor] = useState('');
  const [reason, setReason] = useState('');
  const [typed, setTyped] = useState('');
  const groupName = (groupId: unknown) => String(groups.find(group => group.id === groupId)?.name ?? groupId);
  const picked = mappings.filter(mapping => chosen.includes(String(mapping.id)));
  // A model to pass the requests to: live in every group chosen.
  const successors = action === 'hide' || action === 'retire' ? [...new Set(models.filter(model => model.exposed_model_id !== id && isLive(model)).map(model => String(model.exposed_model_id)))]
    .filter(name => picked.length > 0 && picked.every(mapping => models.some(model => model.group_id === mapping.group_id && model.exposed_model_id === name && isLive(model)))).sort() : [];
  const next = successors.includes(successor) ? successor : '';
  // Hiding or retiring a group's default makes its next shown model the default.
  const defaults = action === 'restore' || action === 'remove' ? [] : picked.filter(mapping => defaultModel(models, mapping.group_id) === mapping).map(mapping => {
    const after = defaultModel(models.filter(model => model !== mapping), mapping.group_id);
    return after ? `它是 ${groupName(mapping.group_id)} 的默认模型：之后默认变为 ${String(after.exposed_model_id)}` : `它是 ${groupName(mapping.group_id)} 唯一对客户可见的模型：之后这个分组没有可选的模型`;
  });
  const charged = next ? picked.filter(mapping => versionsOf(mapping.group_id)).map(mapping => groupName(mapping.group_id)) : [];
  const ready = picked.length > 0 && !!reason.trim() && (action !== 'remove' || typed.trim() === '删除');
  const danger = action === 'retire' || action === 'remove';
  return <Modal role="alertdialog" label={`${plan.label} ${id}？`} onClose={onCancel} className="confirm-dialog state-dialog">
    <h3 className="modal-title">{plan.label} {id}？</h3>
    <ul className="confirm-facts">
      {mappings.map(mapping => <li key={String(mapping.id)}>{groupName(mapping.group_id)} · 现在：{modelStateView(mapping).label}{action === 'restore' && blockedOf(mapping) ? `（${blockedOf(mapping)}）` : ''}</li>)}
      {(action === 'hide' || action === 'retire') && <>
        {usage !== null && <li>近 7 天：{formatCount(usage?.requests ?? 0)} 次请求 · {formatCount(usage?.cards ?? 0)} 张卡</li>}
        <li>{lastUse ? `最近一次使用：${formatDateTime(lastUse)}` : '最近的请求里没有它'}</li>
      </>}
      {defaults.map(text => <li key={text}>{text}</li>)}
    </ul>
    {mappings.length > 1 && <fieldset className="field state-groups"><legend className="field-label">应用到分组</legend>
      {mappings.map(mapping => <label key={String(mapping.id)} className="check-field"><input type="checkbox" disabled={!eligible(mapping)} checked={chosen.includes(String(mapping.id))}
        onChange={event => setChosen(value => event.target.checked ? [...value, String(mapping.id)] : value.filter(item => item !== String(mapping.id)))}/>{groupName(mapping.group_id)}</label>)}
    </fieldset>}
    {(action === 'hide' || action === 'retire') && <div className="field">
      <label className="field-label" htmlFor="state-successor">请求转给</label>
      <select id="state-successor" aria-label="请求转给" value={next} onChange={event => setSuccessor(event.target.value)}>
        <option value="">不转（{action === 'hide' ? '已在用的客户继续用它' : '它的请求会被拒绝'}）</option>
        {successors.map(name => <option key={name} value={name}>{name}</option>)}
      </select>
      {next && <p className="field-hint">会删除 {id} 在 {nameList(picked.map(mapping => groupName(mapping.group_id)))} 的条目，把 {id} 加为 {next} 的别名：客户用 {id} 发的请求改由 {next} 服务。
        {charged.length > 0 && ` 这些请求仍按价格表里 ${id} 的价格扣费（${nameList(charged)}；扣费先找请求里的模型 ID）。`}</p>}
    </div>}
    <p className={`confirm-consequence${danger ? ' is-danger' : ''}`}>{next ? `${id} 不再显示，它的请求由 ${next} 服务。` : plan.consequence}</p>
    <div className="field">
      <label className="field-label" htmlFor="confirm-reason">原因<span className="required-mark">（必填）</span></label>
      <input id="confirm-reason" value={reason} maxLength={160} placeholder={plan.placeholder} onChange={event => setReason(event.target.value)}/>
    </div>
    {action === 'remove' && <div className="field">
      <label className="field-label" htmlFor="confirm-typed">输入 <b className="mono">删除</b> 确认</label>
      <input id="confirm-typed" aria-label="确认输入" value={typed} autoComplete="off" spellCheck={false} onChange={event => setTyped(event.target.value)}/>
    </div>}
    <div className="modal-actions">
      <button type="button" className="btn" data-autofocus onClick={onCancel}>取消</button>
      <button type="button" className={danger ? 'btn btn-danger-solid' : 'btn btn-primary'} data-confirm="accept" disabled={!ready}
        title={ready ? undefined : !picked.length ? '至少选一个分组' : !reason.trim() ? `填写原因后可以${plan.label}` : `输入 删除 后可以${plan.label}`}
        onClick={() => {if (ready) onConfirm({mappings: picked, reason: reason.trim(), successor: next || null});}}>{plan.label}</button>
    </div>
  </Modal>;
}
