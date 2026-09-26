// Small building blocks shared by every page.
import {createContext, useContext, type ReactNode} from 'react';
import {createPortal} from 'react-dom';
import {shortId, type IdKind} from '../format';
import type {StatusView, Tone} from '../status';
import {IconChevronLeft, IconChevronRight, IconCopy, IconInfo} from './icons';
import {toast} from './toast';

export function StatusBadge({view, className}: {view: StatusView; className?: string}) {
  return <span className={`badge badge-${view.tone}${className ? ` ${className}` : ''}`} title={view.title}>{view.label}</span>;
}

export function Tag({tone = 'neutral', title, children}: {tone?: Tone; title?: string; children: ReactNode}) {
  return <span className={`tag tag-${tone}`} title={title}>{children}</span>;
}

/** 估: the number is an estimate; how it was made is in the tooltip. */
export function EstimateTag({title}: {title: string}) {
  return <span className="estimate-tag" tabIndex={0} role="note" aria-label={`估算：${title}`} data-tip={title}>估</span>;
}

/** ⓘ with a short explanation on hover or focus. */
export function InfoTip({text}: {text: string}) {
  return <span className="info-tip" tabIndex={0} role="img" aria-label={text} data-tip={text}><IconInfo/></span>;
}

export async function copyText(text: string, done = '已复制') {
  try {
    await navigator.clipboard.writeText(text);
    toast.success(done);
    return true;
  } catch {
    toast.error('复制失败，请手动选中复制');
    return false;
  }
}

/** A long ID shortened in the middle; the whole ID is in the tooltip and one click copies it. */
export function IdCell({value, kind = 'generic', onOpen, openTitle}: {value: unknown; kind?: IdKind; onOpen?: () => void; openTitle?: string}) {
  const text = typeof value === 'string' ? value : value == null ? '' : String(value);
  if (!text) return <span className="muted">—</span>;
  const display = shortId(text, kind);
  return <span className="id-cell">
    {onOpen
      ? <button type="button" className="id-text id-link" title={openTitle ? `${text}（${openTitle}）` : text} onClick={event => {event.stopPropagation(); onOpen();}}>{display}</button>
      : <span className="id-text" title={text}>{display}</span>}
    <button type="button" className="id-copy" aria-label={`复制 ${text}`} title="复制"
      onClick={event => {event.stopPropagation(); void copyText(text);}}><IconCopy/></button>
  </span>;
}

export function CopyButton({text, label = '复制', done}: {text: string; label?: string; done?: string}) {
  return <button type="button" className="btn btn-small" onClick={() => void copyText(text, done)}><IconCopy/>{label}</button>;
}

/** Placeholder rows while a list loads, so the page does not jump when it arrives. */
export function Skeleton({rows = 4}: {rows?: number}) {
  return <div className="skeleton" role="status" aria-label="正在加载">
    {Array.from({length: rows}, (_, index) => <span key={index} className="skeleton-bar"/>)}
  </div>;
}

/**
 * Loading, failed and truly empty are different: a list that could not be read is never
 * shown as having no records.
 */
export function ListState({loading, failed, empty, onRetry, action}: {loading: boolean; failed?: boolean; empty: string; onRetry: () => void; action?: ReactNode}) {
  if (loading) return <Skeleton/>;
  if (failed) return <div className="list-state" role="status"><p>加载失败</p><button type="button" className="btn btn-small" onClick={onRetry}>重试</button></div>;
  return <div className="list-state"><p>{empty}</p>{action}</div>;
}

/** A table cell spanning the whole row for loading, failure and empty states. */
export function TableState({colSpan, ...props}: Parameters<typeof ListState>[0] & {colSpan: number}) {
  return <tr className="state-row"><td colSpan={colSpan}><ListState {...props}/></td></tr>;
}

export interface TabOption<T extends string> {value: T; label: string; count?: number; tone?: Tone}

/** Filter tabs with live counts. */
export function FilterTabs<T extends string>({label, value, options, onChange, disabled}: {label: string; value: T; options: TabOption<T>[]; onChange: (value: T) => void; disabled?: boolean}) {
  return <div className="filter-tabs" role="tablist" aria-label={label}>
    {options.map(option => <button key={option.value} type="button" role="tab" data-value={option.value} aria-selected={option.value === value}
      disabled={disabled} className={`filter-tab${option.value === value ? ' is-active' : ''}`} onClick={() => onChange(option.value)}>
      {option.label}
      {option.count !== undefined && <span className={`tab-count${option.tone && option.count ? ` tab-count-${option.tone}` : ''}`}>{option.count}</span>}
    </button>)}
  </div>;
}

/** 1–50 / 55 ‹ ›, shown only when there is more than one page. */
export function Pager({page, pageSize, total, onPage, disabled}: {page: number; pageSize: number; total: number; onPage: (page: number) => void; disabled?: boolean}) {
  const pages = Math.max(1, Math.ceil(total / pageSize));
  if (pages <= 1) return null;
  const first = page * pageSize + 1, last = Math.min(total, (page + 1) * pageSize);
  return <div className="pager">
    <span className="pager-range">{first}–{last} / {total}</span>
    <button type="button" className="btn-icon" aria-label="上一页" title="上一页" disabled={disabled || page === 0} onClick={() => onPage(page - 1)}><IconChevronLeft/></button>
    <button type="button" className="btn-icon" aria-label="下一页" title="下一页" disabled={disabled || page + 1 >= pages} onClick={() => onPage(page + 1)}><IconChevronRight/></button>
  </div>;
}

export function Switch({checked, label, onChange, disabled, busy, title}: {checked: boolean; label: string; onChange: (next: boolean) => void; disabled?: boolean; busy?: boolean; title?: string}) {
  return <button type="button" role="switch" aria-checked={checked} aria-label={label} aria-busy={busy || undefined} title={title}
    disabled={disabled} className={`switch${checked ? ' is-on' : ''}${busy ? ' is-busy' : ''}`} onClick={() => onChange(!checked)}>
    <span className="switch-track"><span className="switch-knob"/></span>
    <span className="switch-text">{checked ? '启用' : '停用'}</span>
  </button>;
}

/** The topbar's right side, where each page puts its main actions. */
export const TopbarSlotContext = createContext<HTMLElement | null>(null);

export function TopbarActions({children}: {children: ReactNode}) {
  const slot = useContext(TopbarSlotContext);
  return slot ? createPortal(children, slot) : null;
}
