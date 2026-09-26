import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminFinancials, type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {parseFinancialSettings} from './financial';
import {formatDateTime} from './format';
import {publishFailure} from './refusal';

type Message = {tone: 'error' | 'warning' | 'info'; text: string} | null;

/**
 * 结算参数: the credit face value and the USD rate, published with a reason against the version
 * read. A refusal leaves the values to correct; a publication that crossed another one can be
 * sent again on the latest version with the same values (重新加载并保留修改).
 */
export default function FinancialPanel({onPublished, onDirtyChange, onBusyChange, refreshEpoch = 0}: {data?: AdminFinancials | null; onPublished: () => Promise<void>; onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void; refreshEpoch?: number}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [face, setFace] = useState(''), [rate, setRate] = useState(''), [reason, setReason] = useState('');
  const [message, setMessage] = useState<Message>(null);
  const [busy, setBusy] = useState(false);
  const [publishing, setPublishing] = useState(false);
  const pending = useRef(false), alive = useRef(true);
  const [needsReview, setNeedsReview] = useState(false);
  // The console was refreshed while these settings had unpublished edits.
  const [serverChanged, setServerChanged] = useState(false);
  // The last publication was refused because the configuration changed meanwhile.
  const [conflict, setConflict] = useState(false);
  const dirty = !!reason.trim() || (!!config?.settings && (face !== String(config.settings.credit_face_value_cny) || rate !== String(config.settings.usd_cny_rate)));
  let inputError = '';
  if (config?.settings) {try {parseFinancialSettings(face, rate);} catch (error) {inputError = error instanceof Error ? error.message : '请填写有效数值';}}
  const reasonBytes = new TextEncoder().encode(reason.trim()).length;
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange(false), [onDirtyChange]);
  const apply = (next: CommercialConfig) => {
    setConfig(next); setServerChanged(false); setConflict(false);
    setFace(next.settings ? String(next.settings.credit_face_value_cny) : '');
    setRate(next.settings ? String(next.settings.usd_cny_rate) : '');
  };
  async function load(explicit = false) {
    if (pending.current) return;
    pending.current = true; setBusy(true); setMessage(null);
    try {
      const result = await adminApi.getCommercialConfig();
      if (result.success !== true || !result.config?.revision) throw new Error('配置读取未确认');
      if (alive.current) {
        apply(result.config); setNeedsReview(false); setReason('');
        if (!result.config.settings) setMessage({tone: 'warning', text: '服务器没有返回结算参数，暂不能发布'});
        else if (explicit) toast.success('已重新加载结算参数');
      }
    } catch (error) {
      if (alive.current) {
        setNeedsReview(true);
        setMessage({tone: 'error', text: `加载失败（${error instanceof Error ? error.message : '配置读取失败'}），修改已保留；重新加载成功前不能发布`});
      }
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  }
  useEffect(() => {alive.current = true; void load(); return () => {alive.current = false;};}, []);
  // A console refresh reloads the settings too, but never over unpublished edits.
  const seenEpoch = useRef(refreshEpoch);
  useEffect(() => {
    if (refreshEpoch === seenEpoch.current) return;
    seenEpoch.current = refreshEpoch;
    if (pending.current) return;
    if (dirty) setServerChanged(true);
    else void load();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshEpoch]);

  async function publish() {
    if (pending.current || needsReview || !config?.settings) return;
    let settings: {credit_face_value_cny: number; usd_cny_rate: number};
    try {
      settings = parseFinancialSettings(face, rate);
      if (!reason.trim() || reasonBytes > 500 || /[\x00-\x1f\x7f-\x9f]/.test(reason)) throw new Error('请填写变更原因（最多约 160 字，不含控制字符）');
      if (settings.credit_face_value_cny === config.settings.credit_face_value_cny && settings.usd_cny_rate === config.settings.usd_cny_rate) throw new Error('数值与当前版本一致，无需发布');
    } catch (error) {setMessage({tone: 'error', text: error instanceof Error ? error.message : '发布失败'}); return;}
    const before = config.settings;
    const confirmed = await confirmAction({
      title: '发布结算参数？',
      facts: [
        ...(settings.credit_face_value_cny !== before.credit_face_value_cny ? [`积分面值 ${before.credit_face_value_cny} → ${settings.credit_face_value_cny} 元/积分`] : []),
        ...(settings.usd_cny_rate !== before.usd_cny_rate ? [`美元汇率 ${before.usd_cny_rate} → ${settings.usd_cny_rate} CNY/USD`] : []),
        `原因：${reason.trim()}`,
      ],
      consequence: '影响成本加成计费和财务估算，不改卡内余额。',
      confirmLabel: '发布',
    });
    if (!confirmed || pending.current || !alive.current) return;
    let submitted = false;
    try {
      pending.current = true; submitted = true; setBusy(true); setPublishing(true); setMessage(null);
      const result = await adminApi.publishCommercialConfig({settings, expected_revision: config.revision, reason: reason.trim()});
      if (result.success !== true) throw new AdminApiError('服务端未确认发布', 400);
      if (!result.config?.settings || !result.config.revision) throw new Error('服务端未返回可核对的配置版本');
      if (alive.current) {
        apply(result.config); setReason(''); setNeedsReview(false);
        toast.success('已发布结算参数');
        await onPublished();
      }
    } catch (error) {
      if (alive.current) {
        const text = error instanceof Error ? error.message : '发布失败';
        const failure = submitted ? publishFailure(error, '发布') : {message: text, uncertain: false, conflict: false};
        if (failure.uncertain) {setNeedsReview(true); setMessage({tone: 'error', text: `没收到发布结果（${text}），请重新加载确认后再发布；修改已保留`});}
        else if (failure.conflict) {setConflict(true); setMessage({tone: 'warning', text: '配置刚被别人更新（或在另一个窗口发布过），这次什么都没有发布。点“重新加载并保留修改”：读取最新配置，你填的数值和原因都保留。'});}
        else setMessage({tone: 'error', text: failure.message});
      }
    } finally {if (submitted) {pending.current = false; if (alive.current) {setBusy(false); setPublishing(false);}}}
  }

  // 重新加载并保留修改: the latest version, with the values and reason typed here kept.
  async function reloadKeeping() {
    if (pending.current) return;
    const typed = {face, rate, reason}, started = config?.settings;
    pending.current = true; setBusy(true);
    try {
      const result = await adminApi.getCommercialConfig();
      if (result.success !== true || !result.config?.revision) throw new Error('配置读取未确认');
      if (!alive.current) return;
      apply(result.config); setNeedsReview(false);
      setFace(typed.face); setRate(typed.rate); setReason(typed.reason);
      const now = result.config.settings;
      setMessage(now && started && (now.credit_face_value_cny !== started.credit_face_value_cny || now.usd_cny_rate !== started.usd_cny_rate)
        ? {tone: 'warning', text: `服务器上的结算参数已改为：积分面值 ${now.credit_face_value_cny} 元/积分、美元汇率 ${now.usd_cny_rate}。你填的数值仍在输入框里，请核对后再发布。`}
        : {tone: 'info', text: '已读取最新配置，你填的数值和原因都保留了，可以再发布'});
    } catch (error) {
      if (alive.current) setMessage({tone: 'error', text: `重新加载失败（${error instanceof Error ? error.message : '配置读取失败'}），修改已保留，可以再试`});
    } finally {pending.current = false; if (alive.current) setBusy(false);}
  }

  const blockedReason = needsReview ? '请重新加载确认后再发布' : !config?.settings ? '结算参数没有加载' : inputError ? inputError : !reason.trim() ? '填写变更原因后可发布' : reasonBytes > 500 ? '原因太长（最多约 160 字）' : undefined;
  const updated = config?.settings?.rate_updated_at_secs;
  return <section className="panel settings-panel">
    <div className="panel-head">
      <h3>结算参数</h3>
      {updated ? <span className="muted">上次更新 {formatDateTime(updated)}</span> : null}
    </div>
    <form onSubmit={event => {event.preventDefault(); void publish();}}>
      <fieldset disabled={busy || !config?.settings} className="form-grid form-grid-3">
        <label className="field"><span className="field-label">积分面值<InfoTip text="把积分折算成人民币，用于估算"/></span>
          <span className="input-suffix"><input aria-label="积分面值" type="number" min="0" step="any" max="1000" required aria-invalid={!!inputError} value={face} onChange={event => setFace(event.target.value)}/><span>元/积分</span></span></label>
        <label className="field"><span className="field-label">美元汇率<InfoTip text="把 USD 采购价折成人民币"/></span>
          <span className="input-suffix"><input aria-label="美元汇率" type="number" min="0" step="any" max="1000" required aria-invalid={!!inputError} value={rate} onChange={event => setRate(event.target.value)}/><span>CNY/USD</span></span></label>
        <label className="field"><span className="field-label">变更原因</span>
          <input aria-label="变更原因" required maxLength={500} placeholder="例：按 9 月汇率更新" value={reason} onChange={event => setReason(event.target.value)}/>
          {reasonBytes > 400 && <span className={reasonBytes > 500 ? 'field-error' : 'field-hint'}>{reasonBytes} / 500 字节</span>}
        </label>
        {inputError && <p className="field-error field-span" role="alert">{inputError}</p>}
      </fieldset>
      {needsReview && <p role="alert" className="message message-warning">没收到发布结果或加载失败，请重新加载确认后再发布。</p>}
      {serverChanged && !needsReview && dirty && <p className="message message-warning">服务器上的结算参数可能已更新</p>}
      <div className="editor-actions">
        {message && <p role="status" className={`message message-${message.tone}`}>{message.text}</p>}
        <div className="button-row">
          {(dirty || needsReview || serverChanged || !config?.settings) && <button type="button" className="btn" disabled={busy} onClick={async () => {
            if (dirty && !(await confirmAction({title: '放弃未发布的修改？', consequence: '会重新加载服务器上的结算参数。', confirmLabel: '放弃修改'}))) return;
            void load(true);
          }}>{dirty ? (serverChanged && !needsReview ? '放弃修改并加载' : '放弃修改') : '重新加载'}</button>}
          {(conflict || (serverChanged && !needsReview)) && dirty && <button type="button" className="btn" disabled={busy} onClick={() => void reloadKeeping()}>重新加载并保留修改</button>}
          <button type="submit" className="btn btn-primary" disabled={busy || !!blockedReason} title={blockedReason}>{publishing ? '发布中…' : '发布'}</button>
        </div>
      </div>
    </form>
  </section>;
}
