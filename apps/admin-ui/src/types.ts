// Types shared by the workspace shell and its pages.
import type {MutableRefObject} from 'react';

export type Row = Record<string, unknown>;

export type Tab = 'overview' | 'cards' | 'traces' | 'groups' | 'models' | 'providers' | 'announcements' | 'reconciliation' | 'security';

export type CardTab = 'CURRENT' | 'UNACTIVATED' | 'ACTIVE' | 'FROZEN' | 'BANNED' | 'EXPIRED' | 'ARCHIVED' | 'VOIDED' | 'ALL';
export type CardQuickFilter = 'expiring' | 'low';
export type TraceTab = 'ALL' | 'error' | 'client_aborted' | 'in_progress' | 'success';
export type TraceWindow = 'hour' | 'day' | 'all';

/**
 * Where a page starts (from a link, or its address) and, reported back, where it is: its
 * filters and the details open. `open` is a card or request ID.
 */
export interface Intent {
  cards?: {status?: CardTab; quick?: CardQuickFilter; search?: string; group?: string; open?: string};
  /** `card`: the search is exactly this card's ID. `reason`: a failure class under 失败. */
  traces?: {status?: TraceTab; window?: TraceWindow; search?: string; card?: string; reason?: string; model?: string; provider?: string; open?: string};
  /** 上架模型 for this provider's upstream model (from 供应商与 Key). */
  models?: {list?: {providerId?: string; model?: string}};
  /** `provider` or `key`: what to point out; `edit`: the Key open in the editor. Key IDs are unique across providers. */
  providers?: {provider?: string; key?: string; edit?: string};
}

/** A page's report of where it is, kept in the address. */
export type ReportRoute = (intent: Intent) => void;

export interface ErrorAction {label: string; run: () => void}

/** Shows (or, with an empty text, clears) the error banner of the current page. */
export type ReportError = (text: string, action?: ErrorAction) => void;

/** What pages share with the shell to keep one write at a time and ignore late results. */
export interface WriteGuards {
  writing: MutableRefObject<boolean>;
  mounted: MutableRefObject<boolean>;
}

export interface RefreshOptions {keepSelection?: boolean}
export type Refresh = (options?: RefreshOptions) => Promise<void>;
