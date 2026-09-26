import {useEffect, useRef, useState} from 'react';
import {adminApi, AdminApiError, type AdminFinancials, type CommercialConfig} from './api';
import {confirmAction} from './components/confirm';
import {toast} from './components/toast';
import {InfoTip} from './components/ui';
import {parseFinancialSettings} from './financial';
import {formatDateTime} from './format';

type Message = {tone: 'error' | 'warning' | 'info'; text: string} | null;

/** 结算参数: the credit face value and the USD rate, published with a reason against the version read. */
export default function FinancialPanel({onPublished, onDirtyChange, onBusyChange}: {data?: AdminFinancials | null; onPublished: () => Promise<void>; onDirtyChange: (dirty: boolean) => void; onBusyChange: (busy: boolean) => void}) {
  const [config, setConfig] = useState<CommercialConfig | null>(null);
  const [face, setFace] = useState(''), [rate, setRate] = useState(''), [reason, setReason] = useState('');
  const [message, setMessage] = useState<Message>(null);
  const [busy, setBusy] = useState(false);
  const [publishing, setPublishing] = useState(false);
  const pending = useRef(false), alive = useRef(true);
  const [needsReview, setNeedsReview] = useState(false);
  const dirty = !!reason.trim() || (!!config?.settings && (face !== String(config.settings.credit_face_value_cny) || rate !== String(config.settings.usd_cny_rate)));
  let inputError = '';
  if (config?.settings) {try {parseFinancialSettings(face, rate);} catch (error) {inputError = error instanceof Error ? error.message : '请填写有效数值';}}
  const reasonBytes = new TextEncoder().encode(reason.trim()).length;
  useEffect(() => {onBusyChange(busy); return () => onBusyChange(false);}, [busy, onBusyChange]);
  useEffect(() => {onDirtyChange(dirty);}, [dirty, onDirtyChange]);
  useEffect(() => () => onDirtyChange(false), [onDirtyChange]);
  const apply = (next: CommercialConfig) => {
    setConfig(next);
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
      consequence: '会影响成本加成计费和估算，不会改变卡密余额；有未结算请求时服务器会拒绝。',
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
        const mustReview = submitted && !(error instanceof AdminApiError && [400, 401, 403, 413, 422].includes(error.status));
        if (mustReview) setNeedsReview(true);
        const text = error instanceof Error ? error.message : '发布失败';
        setMessage({tone: 'error', text: mustReview ? `没收到发布结果（${text}），请重新加载确认后再发布；修改已保留` : text});
      }
    } finally {if (submitted) {pending.current = false; if (alive.current) {setBusy(false); setPublishing(false);}}}
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
      <div className="editor-actions">
        {message && <p role="status" className={`message message-${message.tone}`}>{message.text}</p>}
        <div className="button-row">
          <button type="button" className="btn" disabled={busy} onClick={async () => {
            if (dirty && !(await confirmAction({title: '放弃未发布的修改？', consequence: '会重新加载服务器上的结算参数。', confirmLabel: '放弃修改'}))) return;
            void load(true);
          }}>重新加载</button>
          <button type="submit" className="btn btn-primary" disabled={busy || !!blockedReason} title={blockedReason}>{publishing ? '发布中…' : '发布'}</button>
        </div>
      </div>
    </form>
  </section>;
}
