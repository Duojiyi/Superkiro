import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState } from 'react';
import { native } from './bridge';
import { toClientError, type ClientError } from './errors';

export interface UpdateCheck { state: 'disabled' | 'current' | 'available'; current?: string; version?: string; size?: number; mandatory?: boolean; updated?: boolean }
type Phase = 'idle' | 'installing' | 'restarting' | 'failed';
const INTERVAL = 30 * 60 * 1000;
// Refused because a takeover or restore was running: try again once it is likely done.
const BUSY_RETRY = 30 * 1000;
// A dropped network line: retry automatically, resuming from the break point, a few times
// before asking the customer.
const NETWORK_RETRY = 3 * 1000;
const MAX_NETWORK_RETRIES = 3;
// A backstop so a stuck install never leaves the screen spinning forever with no way out.
const INSTALL_LIMIT = 15 * 60 * 1000;
// A confirmation that did not reach the host is sent again: unconfirmed, a new version is
// counted against when it ends.
const CONFIRM_ATTEMPTS = 3;
const CONFIRM_RETRY = 2 * 1000;
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
 * as soon as nothing else is running; an optional one waits for the customer. A mandatory
 * update can be put off whenever it is not actually installing, so the customer is never
 * locked out of the client; it then keeps offering from the header.
 */
export function useUpdater(blocked: boolean, onUpdated: (version: string) => void) {
  const [check, setCheck] = useState<UpdateCheck | null>(null);
  const [phase, setPhase] = useState<Phase>('idle');
  const [progress, setProgress] = useState<{ received: number; total: number } | null>(null);
  const [error, setError] = useState<ClientError | null>(null);
  const [retryAt, setRetryAt] = useState(0);
  const [postponed, setPostponed] = useState(false);
  const [resumed, setResumed] = useState(false);
  const [autoRetry, setAutoRetry] = useState(false);
  // Bumped by a timer when a scheduled retry is due, so the install effect runs again.
  const [tick, setTick] = useState(0);
  const notified = useRef(false);
  const networkRetries = useRef(0);
  const mandatoryRef = useRef(false);
  const attempt = useRef(0);
  const confirmed = useRef(false);
  const updatedLatest = useRef(onUpdated);
  updatedLatest.current = onUpdated;
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
    void listen<{ received?: unknown; total?: unknown; resumed?: unknown }>('update-progress', ({ payload }) => {
      const received = Number(payload?.received), total = Number(payload?.total);
      if (Number.isSafeInteger(received) && Number.isSafeInteger(total) && total > 0 && received >= 0 && received <= total) {
        // The host says whether it continued an interrupted download.
        setResumed(payload?.resumed === true);
        setProgress({ received, total });
      }
    }).then(unlisten => { if (disposed) unlisten(); else stop = unlisten; }).catch(() => {});
    return () => { disposed = true; stop?.(); };
  }, []);
  // Called once a page showing its host's local status has rendered: this build runs, renders
  // and its host answers, so a newly installed version has proven itself and takes its place
  // (a no-op for anything else). It depends on nothing remote: offline, or with the update
  // server down, a new version still confirms.
  const confirm = useCallback(() => {
    if (confirmed.current) return;
    confirmed.current = true;
    void (async () => {
      for (let i = 0; i < CONFIRM_ATTEMPTS; i++) {
        try { await native('update_confirm'); return; } catch { await new Promise(resolve => setTimeout(resolve, CONFIRM_RETRY)); }
      }
    })();
  }, []);
  const schedule = useCallback((delay: number) => {
    const at = Date.now() + delay;
    setRetryAt(at);
    setTimeout(() => setTick(t => t + 1), delay);
  }, []);
  const install = useCallback(async () => {
    const mine = ++attempt.current;
    setPhase('installing'); setError(null); setProgress(null); setPostponed(false); setResumed(false); setAutoRetry(false);
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        native('update_install'),
        new Promise((_, reject) => { timer = setTimeout(() => { void native('update_cancel').catch(() => {}); reject(toClientError({ code: 'SK-UPDATE-001' })); }, INSTALL_LIMIT); }),
      ]);
      if (mine !== attempt.current) return;
      // The host starts the new version and ends this one.
      networkRetries.current = 0;
      setPhase('restarting');
    } catch (e) {
      if (mine !== attempt.current) return;
      const failure = toClientError(e);
      // A takeover, restore or another install is running: fall back and try again shortly.
      if (failure.code === 'SK-LOCAL-002') { setPhase('idle'); schedule(BUSY_RETRY); return; }
      // A dropped line during a required update: resume automatically a few times before
      // asking. The download continues from the break point, so a retry does not start over.
      if (failure.code === 'SK-UPDATE-001' && mandatoryRef.current && networkRetries.current < MAX_NETWORK_RETRIES) {
        networkRetries.current += 1;
        setPhase('idle'); setAutoRetry(true); schedule(NETWORK_RETRY);
        return;
      }
      setError(failure); setPhase('failed');
    } finally { clearTimeout(timer); }
  }, [schedule]);
  const mandatory = check?.state === 'available' && check.mandatory === true;
  mandatoryRef.current = mandatory;
  useEffect(() => {
    if (mandatory && phase === 'idle' && !blocked && !postponed && retryAt <= Date.now()) void install();
  }, [mandatory, phase, blocked, postponed, retryAt, tick, install]);
  // Put an update off so the client is usable; it keeps offering from the header. During a
  // download this also stops it; what arrived stays and the next try continues from there.
  const dismiss = useCallback(() => {
    if (phase === 'installing') { ++attempt.current; void native('update_cancel').catch(() => {}); }
    setPhase('idle'); setError(null); setPostponed(true); setAutoRetry(false); setProgress(null);
  }, [phase]);
  const start = useCallback(() => { setPostponed(false); networkRetries.current = 0; void install(); }, [install]);
  const retry = useCallback(() => { networkRetries.current = 0; void install(); }, [install]);
  const available = check?.state === 'available' ? check : null;
  return {
    check, phase, progress, error, install, retry, dismiss, start, confirm, postponed, resumed, autoRetry, mandatory,
    /** An update the customer can start from the header: an optional one, or a mandatory one
     * that was put off. */
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
  const { check, phase, progress, error, resumed, autoRetry } = updater;
  const mandatory = check?.state === 'available' && check.mandatory === true;
  const title = phase === 'restarting' ? '正在重启 Superkiro' : phase === 'installing' ? '正在更新 Superkiro' : phase === 'failed' ? '更新未完成' : '需要更新 Superkiro';
  // The download's own progress; once it reaches the full size the host is verifying and
  // installing, which reports no further progress.
  // Rounded down: 100 only once every byte is in, so the pause button stays while any is due.
  const percent = progress && progress.total > 0 ? Math.min(100, Math.floor((progress.received / progress.total) * 100)) : null;
  const downloading = phase === 'installing' && percent !== null && percent < 100;
  const finishing = phase === 'installing' && percent === 100;
  const connecting = phase === 'installing' && percent === null;
  const indeterminate = connecting || finishing || (phase === 'idle');
  const barPercent = phase === 'restarting' ? 100 : downloading ? percent : finishing ? 100 : 0;
  return <section className="update-screen" aria-labelledby="update-title">
    <h1 id="update-title">{title}</h1>
    <p className="subtitle">{check?.version ? `新版本 ${check.version}` : '新版本'}{check?.current ? `（当前 ${check.current}）` : ''}{mandatory ? '为必需更新，' : '，'}完成后客户端会自动重启。Kiro 的连接配置不受影响。</p>
    <div className="panel spaced" role="status" aria-live="polite">
      {phase === 'failed' && error ? <><p>{error.message}</p><p className="muted">错误码：{error.code} · 反馈编号：{error.feedback_id}</p></>
        : <div className="update-visual">
          {phase === 'restarting'
            ? <div className="update-spinner" aria-hidden="true"/>
            : <div className="update-bar" role="progressbar" aria-label="更新进度" aria-valuemin={0} aria-valuemax={100} aria-valuenow={downloading ? percent! : undefined}>
                <div className={`update-bar-fill${indeterminate ? ' sliding' : ''}`} style={{ width: `${indeterminate ? 40 : barPercent}%` }}/>
              </div>}
          <div className="update-stat">{phase === 'restarting' ? '↻' : downloading ? `${percent}%` : finishing ? '校验并安装' : connecting ? '连接中' : '准备中'}</div>
          <p className="muted">{phase === 'restarting' ? '新版本已就绪，正在重新打开客户端…'
            : blocked ? '正在等待当前操作完成，完成后自动开始更新。'
              : autoRetry && phase === 'idle' ? '网络中断，正在自动断点续传重试…'
                : connecting ? '正在连接更新服务器…'
                  : downloading ? `已下载 ${megabytes(progress!.received)} / ${megabytes(progress!.total)} MB${resumed ? ' · 已从断点续传' : ''}`
                    : finishing ? '正在校验并安装新版本…' : '即将开始更新…'}</p>
        </div>}
    </div>
    {phase === 'failed' && <>
      <button className="primary full" onClick={updater.retry}>重试更新</button>
      <button className="full" onClick={openDownloads}>从官网下载新版 ↗</button>
    </>}
    {/* Restoring Kiro stays reachable until the client restarts; a restore simply makes the
        update wait for it. */}
    {phase !== 'restarting' && restore && <button className="full" onClick={restore}>还原 Kiro 配置</button>}
    {/* Even a required update can be put off while it is not installing the downloaded
        version, so the client is never locked; a download in progress stops and later
        continues where it was. Once downloaded, the new version starts moments later, and a
        button that could no longer stop it is not offered. */}
    {phase !== 'restarting' && !finishing && <button className="text full" onClick={updater.dismiss}>{phase === 'installing' ? '暂停更新，先进入客户端' : '暂时进入客户端'}</button>}
  </section>;
}
