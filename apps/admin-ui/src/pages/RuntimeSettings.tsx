import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError} from '../api';
import {confirmAction} from '../components/confirm';
import {toast} from '../components/toast';
import {runtimeError, runtimeReasonError, timeoutFields, type RuntimeConfig, type RuntimeSettings} from '../runtimeSettings';
import type {WriteGuards} from '../types';

export default function RuntimeSettingsPage({guards, onDirtyChange, onBusyChange}: {
  guards: WriteGuards; onDirtyChange: (value: boolean) => void; onBusyChange: (value: boolean) => void;
}) {
  const [config, setConfig] = useState<RuntimeConfig | null>(null);
  const [draft, setDraft] = useState<RuntimeSettings | null>(null);
  const [reason, setReason] = useState(''), [error, setError] = useState('');
  const [busy, setBusy] = useState(false), [blocked, setBlocked] = useState(false);
  const alive = useRef(true), pending = useRef(false);
  const dirty = !!config && (JSON.stringify(config.settings) !== JSON.stringify(draft) || !!reason);
  const validation = draft ? runtimeError(draft) || (reason ? runtimeReasonError(reason) : '') : '';
  useEffect(() => {alive.current = true; void read(); return () => {alive.current = false;};}, []);
  useEffect(() => {onDirtyChange(dirty); return () => onDirtyChange(false);}, [dirty, onDirtyChange]);
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  const adopt = (next: RuntimeConfig) => {setConfig(next); setDraft(next.settings); setReason(''); setError(''); setBlocked(false);};
  async function read() {
    if (pending.current) return;
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.getRuntimeSettings();
      if (!result.success || !result.config?.revision || !Array.isArray(result.config.audit) || runtimeError(result.config.settings)) throw new Error('服务器未返回有效运行配置');
      if (alive.current) adopt(result.config);
    } catch (e) {if (alive.current) setError(e instanceof Error ? e.message : String(e));}
    finally {pending.current = false; if (alive.current) setBusy(false);}
  }
  async function reload() {
    if (dirty && !await confirmAction({title: '重新读取运行参数？', consequence: '将丢弃未发布草稿，并读取服务器已保存的版本。', confirmLabel: '重新读取'})) return;
    if (alive.current) await read();
  }
  async function publish() {
    if (!config || !draft || validation || !reason.trim() || blocked || pending.current || guards.writing.current) return;
    // Freeze the draft before awaiting confirmation, not just during the request.
    pending.current = true; guards.writing.current = true; setBusy(true); onBusyChange(true);
    try {
      if (!await confirmAction({title: '发布运行参数？', consequence: '保存成功后，新请求立即使用新参数；正在进行的请求保持原参数。', confirmLabel: '发布'})) return;
      if (!alive.current) return;
      const result = await adminApi.publishRuntimeSettings({expected_revision: config.revision, reason: reason.trim(), settings: draft});
      if (!result.success || !result.config?.revision || !Array.isArray(result.config.audit) || runtimeError(result.config.settings)) throw new Error('服务器未确认保存');
      if (alive.current) {adopt(result.config); toast.success('运行参数已保存，新请求立即生效');}
    } catch (e) {
      if (alive.current) {
        const rejected = e instanceof AdminApiError && e.status === 400;
        setBlocked(!rejected);
        setError(`${e instanceof Error ? e.message : String(e)}。${rejected ? '发布被拒绝，草稿保留；请修正后重试。' : '草稿保留；请重新读取核对服务器版本后再发布。'}`);
      }
    } finally {pending.current = false; guards.writing.current = false; if (alive.current) setBusy(false);}
  }
  return <section className="section-card">
    <h3>上游与流式响应超时</h3>
    <p>单位：秒。无需重新编译或重启；新请求使用同一配置快照，包括重试和后备路由。版本：{config?.revision ?? '读取中'}</p>
    <p>供应商地址、模型映射、价格请使用对应后台页面；密钥和监听端口等启动安全参数不在此开放。</p>
    {error && <p role="alert">{error}</p>}
    {draft && <fieldset disabled={busy} style={{border: 0, padding: 0}}>
      <div className="table-scroll"><table className="data-table"><thead><tr><th>参数</th><th>普通模型</th><th>思考模型</th><th>Claude</th></tr></thead>
        <tbody>{timeoutFields.map(f => <tr key={f.key}><th>{f.label}（{f.min}–{f.max}）</th>{(['standard', 'reasoning', 'claude'] as const).map(group => <td key={group}>
          <input aria-label={`${group} ${f.label}`} type="number" min={f.min} max={f.max} step="1" value={draft[group][f.key]} onChange={e => setDraft({...draft, [group]: {...draft[group], [f.key]: Number(e.target.value)}})}/>
        </td>)}</tr>)}</tbody></table></div>
      <label className="field">OpenAI 格式思考空闲（10–3600 秒；按目标模型请求上限截断）<input type="number" min="10" max="3600" value={draft.openai_reasoning_idle_secs} onChange={e => setDraft({...draft, openai_reasoning_idle_secs: Number(e.target.value)})}/></label>
      <label className="field">客户端保活间隔（1–25 秒）<input type="number" min="1" max="25" value={draft.keepalive_secs} onChange={e => setDraft({...draft, keepalive_secs: Number(e.target.value)})}/></label>
      <label className="field">修改原因<input maxLength={1024} value={reason} onChange={e => setReason(e.target.value)}/></label>
    </fieldset>}
    {validation && <p role="alert">{validation}</p>}
    <div className="button-row"><button className="btn" disabled={busy} onClick={() => void reload()}>重新读取</button><button className="btn btn-primary" disabled={busy || blocked || !dirty || !draft || !!validation || !reason.trim()} onClick={() => void publish()}>发布运行参数</button></div>
    {!!config?.audit.length && <details><summary>最近修改记录</summary><ul>{[...config.audit].reverse().map(a => <li key={a.revision}>{new Date(a.created_at_secs * 1000).toLocaleString()} · {a.reason}</li>)}</ul></details>}
  </section>;
}
