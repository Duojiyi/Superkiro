import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import { native } from './bridge';
import { toClientError, type ClientError } from './errors';

export interface UpdateCheck { state: 'disabled' | 'current' | 'available'; current?: string; version?: string; size?: number; mandatory?: boolean; updated?: boolean }
type Phase = 'idle' | 'installing' | 'restarting' | 'failed';
const INTERVAL = 30 * 60 * 1000;
// Refused because a takeover or restore was running: try again once it is likely done.
const BUSY_RETRY = 30 * 1000;
// A backstop so a stuck install never leaves the screen spinning forever with no way out.
const INSTALL_LIMIT = 15 * 60 * 1000;
const version = (v: unknown): v is string => typeof v === 'string' && /^\d{1,9}(\.\d{1,9}){0,3}$/.test(v);

/** Only the shapes the host sends; anything else is no update at all. */
export function parseCheck(value: unknown): UpdateCheck | null {
  const v = value && typeof value === 'object' ? value as Record<string, unknown> : null;
  if (!v) return null;
  const updated = v.updated === true;
  if (v.state === 'disabled') return { state: 'disabled', updated };
  if (v.state === 'current' && version(v.current)) return { state: 'current', current: v.current, updated };
  if (v.state === 'available' && version(v.current) && version(v.version) && typeof v.size === 'number'
    && Number.isSafeInteger(v.size) && v.size > 0 && typeof v.mandatory === 'boolean') {
    return { state: 'available', current: v.current, version: v.version, size: v.size, mandatory: v.mandatory, updated };
  }
  return null;
}

const megabytes = (bytes: number) => (bytes / 1024 / 1024).toLocaleString('zh-CN', { maximumFractionDigits: 1 });

/**
 * Checks for a new version at start and every half hour. A mandatory one installs by itself
 * as soon as nothing else is running; an optional one waits for the customer. A failed
 * mandatory update can be put off so the customer is never locked out of the app - it keeps
 * offering from the header instead of blocking the page.
 */
export function useUpdater(blocked: boolean, onUpdated: (version: string) => void) {
  const [check, setCheck] = useState<UpdateCheck | null>(null);
  const [phase, setPhase] = useState<Phase>('idle');
  const [progress, setProgress] = useState<{ received: number; total: number } | null>(null);
  const [error, setError] = useState<ClientError | null>(null);
  const [retryAt, setRetryAt] = useState(0);
  const [postponed, setPostponed] = useState(false);
  const notified = useRef(false);
  const confirmed = useRef(false);
  const updatedLatest = useRef(onUpdated);
  updatedLatest.current = onUpdated;
  // Once the window is up and talking to the host, tell it this build proved itself: the
  // version it replaced is no longer kept for rollback.
  useEffect(() => { if (!confirmed.current) { confirmed.current = true; void native('update_confirm').catch(() => {}); } }, []);
  const refresh = useCallback(async () => {
    try {
      const result = parseCheck(await native('update_check'));
      if (!result) return;
      setCheck(result);
      if (result.updated && result.current && !notified.current) {
        notified.current = true;
        updatedLatest.current(result.current);
      }
    } catch { /* A failed check changes nothing; the next one tries again. */ }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), INTERVAL);
    return () => clearInterval(timer);
  }, [refresh]);
  useEffect(() => {
    let disposed = false, stop: (() => void) | undefined;
    void listen<{ received?: unknown; total?: unknown }>('update-progress', ({ payload }) => {
      const received = Number(payload?.received), total = Number(payload?.total);
      if (Number.isSafeInteger(received) && Number.isSafeInteger(total) && total > 0 && received >= 0 && received <= total) {
        setProgress({ received, total });
      }
    }).then(unlisten => { if (disposed) unlisten(); else stop = unlisten; }).catch(() => {});
    return () => { disposed = true; stop?.(); };
  }, []);
  const install = useCallback(async () => {
    setPhase('installing'); setError(null); setProgress(null); setPostponed(false);
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        native('update_install'),
        new Promise((_, reject) => { timer = setTimeout(() => reject(toClientError({ code: 'SK-UPDATE-004' })), INSTALL_LIMIT); }),
      ]);
      // The host starts the new version and ends this one.
      setPhase('restarting');
    } catch (e) {
      const failure = toClientError(e);
      // A takeover or restore is running: fall back to the page and try again shortly.
      if (failure.code === 'SK-LOCAL-002') { setPhase('idle'); setRetryAt(Date.now() + BUSY_RETRY); return; }
      setError(failure); setPhase('failed');
    } finally { clearTimeout(timer); }
  }, []);
  const mandatory = check?.state === 'available' && check.mandatory === true;
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (retryAt <= now) return;
    const timer = setTimeout(() => setNow(Date.now()), retryAt - now);
    return () => clearTimeout(timer);
  }, [retryAt, now]);
  useEffect(() => {
    if (mandatory && phase === 'idle' && !blocked && !postponed && retryAt <= now) void install();
  }, [mandatory, phase, blocked, postponed, retryAt, now, install]);
  // Put a failed mandatory update off so the app is usable; it keeps offering from the header.
  const dismiss = useCallback(() => { setPhase('idle'); setError(null); setPostponed(true); }, []);
  const start = useCallback(() => { setPostponed(false); void install(); }, [install]);
  const available = check?.state === 'available' ? check : null;
  return {
    check, phase, progress, error, install, dismiss, start, postponed,
    /** An update the customer can start from the header: an optional one, or a mandatory one
     * that failed and was put off. */
    offer: available && phase === 'idle' && (!mandatory || postponed) ? available : null,
    /** Whether the update screen takes the place of the page. */
    screen: phase !== 'idle' || (mandatory && !postponed),
  };
}

export type Updater = ReturnType<typeof useUpdater>;

/** In place of the page while an update is due or running. The header stays usable, restoring
 * Kiro is always reachable, and a mandatory update that keeps failing can be put off - a
 * customer is never locked out of restoring their configuration or using the client. */
export function UpdateScreen({ updater, blocked, openDownloads, restore }: { updater: Updater; blocked: boolean; openDownloads: () => void; restore: (() => void) | null }) {
  const { check, phase, progress, error } = updater;
  const mandatory = check?.state === 'available' && check.mandatory === true;
  const working = phase === 'installing' || phase === 'restarting';
  const title = phase === 'restarting' ? '正在重启 Superkiro' : working ? '正在更新 Superkiro' : phase === 'failed' ? '更新未完成' : '需要更新 Superkiro';
  const total = progress?.total ?? check?.size ?? 0;
  return <section className="update-screen" aria-labelledby="update-title">
    <h1 id="update-title">{title}</h1>
    <p className="subtitle">{check?.version ? `新版本 ${check.version}` : '新版本'}{check?.current ? `（当前 ${check.current}）` : ''}{mandatory ? '为必需更新，' : '，'}完成后客户端会自动重启。Kiro 的连接配置不受影响。</p>
    <div className="panel spaced" role="status" aria-live="polite">
      {phase === 'restarting' ? <p>新版本已就绪，正在重新打开客户端…</p>
        : working ? <><p>{progress ? `已下载 ${megabytes(progress.received)} / ${megabytes(progress.total)} MB` : '正在连接更新服务器…'}</p><progress aria-label="更新下载进度" max={total || undefined} value={progress && total ? progress.received : undefined}/></>
        : phase === 'failed' && error ? <><p>{error.message}</p><p className="muted">错误码：{error.code} · 反馈编号：{error.feedback_id}</p></>
        : <p>{blocked ? '正在等待当前操作完成，完成后自动开始更新。' : '即将开始更新…'}</p>}
    </div>
    {phase === 'failed' && <>
      <button className="primary full" onClick={() => void updater.install()}>重试更新</button>
      <button className="full" onClick={openDownloads}>从官网下载新版 ↗</button>
      {restore && <button className="full" onClick={restore}>还原 Kiro 配置</button>}
      {/* Even a required update can be put off after it fails, so the client stays usable. */}
      <button className="text full" onClick={updater.dismiss}>暂时进入客户端</button>
    </>}
    {/* While waiting to start (not mid-download), restoring Kiro must stay available. */}
    {phase === 'idle' && restore && <button className="full" onClick={restore}>还原 Kiro 配置</button>}
  </section>;
}
