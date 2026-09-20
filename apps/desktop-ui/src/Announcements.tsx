import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { api } from './bridge';
import './Announcements.css';

export interface Announcement {
  id: string;
  title: string;
  content: string;
  level: string;
  created_at: string | number;
  expires_at: string | number | null;
}
const STORAGE = 'superkiro.announcements.read.v1';
const INTERVAL = 5 * 60 * 1000;
// Exact content signature: no hash collisions, and no timestamp-only versioning.
const signature = (a: Announcement) => JSON.stringify([a.title, a.content, a.level]);
function loadRead(): Record<string, string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(STORAGE) || '{}');
    return value && typeof value === 'object' && !Array.isArray(value)
      ? Object.fromEntries(Object.entries(value).filter(([, v]) => typeof v === 'string')) : {};
  } catch { return {}; }
}
function parse(value: unknown): Announcement[] {
  const list = (value as { announcements?: unknown } | null)?.announcements;
  if (!Array.isArray(list) || list.some(a => !a || !['string', 'number'].includes(typeof a.id)
    || typeof a.title !== 'string' || typeof a.content !== 'string' || typeof a.level !== 'string'
    || !['string', 'number'].includes(typeof a.created_at)
    || !(a.expires_at === null || ['string', 'number'].includes(typeof a.expires_at)))) {
    throw new Error('Invalid announcements response');
  }
  return list.map(a => ({ ...a, id: String(a.id) }));
}

/** Split at Unicode code points; fits uses the actual rendered width/height. */
export function paginateText(text: string, fits: (text: string) => boolean): string[] {
  const chars = Array.from(text);
  const pages: string[] = [];
  for (let start = 0; start < chars.length;) {
    let low = 1, high = chars.length - start;
    while (low < high) {
      const mid = Math.ceil((low + high) / 2);
      if (fits(chars.slice(start, start + mid).join(''))) low = mid;
      else high = mid - 1;
    }
    pages.push(chars.slice(start, start + low).join(''));
    start += low;
  }
  return pages.length ? pages : [''];
}

function expiry(value: Announcement['expires_at']) {
  if (value === null) return Infinity;
  const seconds = Number(value);
  return Number.isFinite(seconds) ? seconds * 1000 : Date.parse(String(value));
}

export function Announcements({ blocked }: { blocked: boolean }) {
  const titleId = useId();
  const dialog = useRef<HTMLDialogElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const viewport = useRef<HTMLDivElement>(null);
  const measure = useRef<HTMLDivElement>(null);
  const opener = useRef<HTMLElement | null>(null);
  const attempted = useRef(new Set<string>());
  const [read, setRead] = useState(loadRead);
  const [fetchedItems, setItems] = useState<Announcement[]>([]);
  const [now, setNow] = useState(Date.now);
  const items = useMemo(() => fetchedItems.filter(a => expiry(a.expires_at) > Math.max(now, Date.now())), [fetchedItems, now]);
  useEffect(() => {
    const next = Math.min(...fetchedItems.map(a => expiry(a.expires_at)).filter(time => time > Date.now()));
    if (!Number.isFinite(next)) return;
    const timer = setTimeout(() => setNow(Date.now()), Math.min(2147483647, Math.max(0, next - Date.now())));
    return () => clearTimeout(timer);
  }, [fetchedItems, now]);
  const [status, setStatus] = useState<'loading' | 'ready' | 'error'>('loading');
  const [reload, setReload] = useState(0);
  const [open, setOpen] = useState<'auto' | 'manual' | null>(null);
  const [index, setIndex] = useState(0);
  const [page, setPage] = useState(0);
  const [pages, setPages] = useState<string[]>([]);
  const [measuredVersion, setMeasuredVersion] = useState('');
  const current = items[index];
  const version = current ? signature(current) : '';
  const text = current ? `${current.title}\n\n${current.content}` : '';
  const unread = items.filter(a => read[String(a.id)] !== signature(a)).length;

  useEffect(() => {
    let disposed = false, pending = false;
    async function refresh() {
      if (pending) return;
      pending = true;
      try {
        const next = parse(await api<unknown>('/api/announcements'));
        if (!disposed) {
          setItems(previous => JSON.stringify(previous) === JSON.stringify(next) ? previous : next);
          setStatus('ready');
        }
      } catch { if (!disposed) setStatus('error'); }
      finally { pending = false; }
    }
    void refresh();
    const timer = setInterval(() => void refresh(), INTERVAL);
    return () => { disposed = true; clearInterval(timer); };
  }, [reload]);

  useEffect(() => {
    const sync = () => setRead(loadRead());
    window.addEventListener('storage', sync);
    return () => window.removeEventListener('storage', sync);
  }, []);

  useEffect(() => {
    function tryOpen() {
      const other = Array.from(document.querySelectorAll('dialog[open]')).some(el => el !== dialog.current);
      if (open === 'auto' && (blocked || other)) { setOpen(null); return; }
      if (blocked || open || other || status !== 'ready' || document.visibilityState !== 'visible'
        || !document.hasFocus() || document.activeElement?.closest('input, textarea, select, [contenteditable]:not([contenteditable="false"]), [role="textbox"]')) return;
      const next = items.findIndex(a => read[String(a.id)] !== signature(a)
        && !attempted.current.has(JSON.stringify([a.id, signature(a)])));
      if (next < 0) return;
      attempted.current.add(JSON.stringify([items[next].id, signature(items[next])]));
      setIndex(next); setPage(0); setOpen('auto');
    }
    tryOpen();
    // Defer until an existing dialog/input releases focus. No timer steals focus.
    const observer = new MutationObserver(tryOpen);
    observer.observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ['open'] });
    document.addEventListener('focusin', tryOpen);
    document.addEventListener('visibilitychange', tryOpen);
    window.addEventListener('focus', tryOpen);
    return () => {
      observer.disconnect();
      document.removeEventListener('focusin', tryOpen);
      document.removeEventListener('visibilitychange', tryOpen);
      window.removeEventListener('focus', tryOpen);
    };
  }, [blocked, open, items, read, status]);

  useLayoutEffect(() => {
    const el = dialog.current!;
    if (!open) { if (el.open) el.close(); return; }
    opener.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    el.showModal();
    closeButton.current?.focus();
    return () => {
      if (el.open) el.close();
      // Never move focus out of another dialog that has just opened.
      if (!document.querySelector('dialog[open]') && opener.current?.isConnected) opener.current.focus();
    };
  }, [open]);

  useLayoutEffect(() => {
    if (!open || !current || status !== 'ready') { setPages([]); return; }
    const box = viewport.current!, probe = measure.current!;
    function layout() {
      if (!box.clientWidth || !box.clientHeight) { setPages([]); return; }
      setPages(paginateText(text, candidate => {
        probe.textContent = candidate;
        return probe.scrollHeight <= box.clientHeight && probe.scrollWidth <= box.clientWidth;
      }));
      setMeasuredVersion(version);
      setPage(0);
    }
    layout();
    const observer = new ResizeObserver(layout);
    observer.observe(box);
    document.fonts?.ready.then(() => { if (box.isConnected) layout(); });
    return () => observer.disconnect();
  }, [open, text, current, status, version]);

  useEffect(() => { if (index >= items.length) setIndex(0); }, [items, index]);
  useEffect(() => {
    if (!open || status !== 'ready' || !current || measuredVersion !== version || !pages.length || page !== pages.length - 1
      || read[String(current.id)] === version) return;
    const next = { ...loadRead(), ...read, [String(current.id)]: version };
    setRead(next);
    try { localStorage.setItem(STORAGE, JSON.stringify(next)); } catch { /* Storage may be disabled; keep session read state. */ }
  }, [open, current, version, pages, page, read, status, measuredVersion]);

  function dismiss() {
    items.forEach(a => attempted.current.add(JSON.stringify([a.id, signature(a)])));
    setOpen(null);
  }
  function moveItem(delta: number) { setIndex(index + delta); setPage(0); }
  return <>
    <button type="button" className="announcements-trigger" onClick={() => { setIndex(0); setPage(0); setOpen('manual'); }}>
      公告{unread > 0 && <span className="announcements-dot" aria-label={`${unread} 条未读公告`} />}
    </button>
    <dialog ref={dialog} className="announcements-dialog" aria-labelledby={titleId}
      onCancel={event => { event.preventDefault(); dismiss(); }} onClose={event => { if (!event.currentTarget.open) dismiss(); }}>
      <div className="announcements-heading"><h2 id={titleId}>公告</h2><button ref={closeButton} type="button" onClick={dismiss}>关闭</button></div>
      {status !== 'ready' ? <div className="announcements-state" role="status">
        {status === 'loading' ? '正在获取公告…' : '公告获取失败，无法确认最新公告。'}
        {status === 'error' && <button type="button" onClick={() => { setStatus('loading'); setReload(n => n + 1); }}>重试</button>}
      </div> : !current ? <div className="announcements-state" role="status">暂无公告</div> : <>
        <div className="announcements-meta">第 {index + 1} / {items.length} 条 · {({ info: '通知', warning: '重要', critical: '紧急' } as Record<string, string>)[current.level] || '通知'}</div>
        <div ref={viewport} className="announcements-viewport">
          <div className="announcements-text" aria-live="polite">{pages[page] || ''}</div>
          <div ref={measure} className="announcements-text announcements-measure" aria-hidden="true" />
        </div>
        <div className="announcements-pagination">
          <button type="button" disabled={page === 0} onClick={() => setPage(page - 1)}>上一页</button>
          <span aria-live="polite">{pages.length ? page + 1 : 0} / {pages.length} 页</span>
          <button type="button" disabled={page >= pages.length - 1} onClick={() => setPage(page + 1)}>下一页</button>
        </div>
        <div className="announcements-pagination">
          <button type="button" disabled={index === 0} onClick={() => moveItem(-1)}>上一条</button>
          <button type="button" disabled={index >= items.length - 1} onClick={() => moveItem(1)}>下一条</button>
        </div>
      </>}
    </dialog>
  </>;
}



