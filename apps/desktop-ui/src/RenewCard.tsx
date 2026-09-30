import { useEffect, useRef, useState, type FormEvent } from 'react';
import { api, number } from './bridge';

export function RenewCard({ current, gateway, close, renewed }: { current?: string; gateway: string; close: () => void; renewed: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [busy, setBusy] = useState(false), [error, setError] = useState(''), [done, setDone] = useState(false);
  useEffect(() => { const opener = document.activeElement as HTMLElement; dialog.current?.showModal(); return () => opener?.focus(); }, []);
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault(); if (busy || done) return;
    const data = new FormData(event.currentTarget);
    const target = (current || String(data.get('current') || '')).trim();
    const source = String(data.get('source') || '').trim();
    if (!target || !source || source === target) { setError('请输入与当前卡密不同的未激活卡密。'); return; }
    setBusy(true); setError('');
    try {
      const result = await api<{ success: boolean; addedPoints: number }>('/api/renew-card', 'POST', { gateway_url: gateway, card_key: target, renewal_card: source });
      if (!result.success) throw new Error();
      setDone(true); setError('续费成功，增加 ' + number(result.addedPoints) + ' 积分。'); renewed();
    } catch { setError('续费未确认。请先刷新余额；确认未到账后，再检查卡密是否未激活并重试。'); }
    finally { setBusy(false); }
  }
  return <dialog ref={dialog} onCancel={e => { e.preventDefault(); if (!busy) close(); }} aria-labelledby="renew-title"><form onSubmit={e => void submit(e)}>
    <h2 id="renew-title">卡密续费</h2><p>未激活卡密的积分将转入当前卡密，充值卡密随即作废。有效期延至续费日起 30 天；原有效期更晚或永久有效的保持不变。</p>
    {!done && <>{!current && <label>当前卡密<input name="current" type="password" required maxLength={256} autoComplete="off" disabled={busy}/></label>}<label>未激活的充值卡密<input name="source" type="password" required maxLength={256} autoComplete="off" disabled={busy}/></label></>}
    {error && <p role="status">{error}</p>}
    {!done && <button className="primary full" disabled={busy} type="submit">{busy ? '正在续费' : '确认续费'}</button>}
    <button className="full" disabled={busy} type="button" onClick={close}>{done ? '完成' : '取消'}</button>
  </form></dialog>;
}
