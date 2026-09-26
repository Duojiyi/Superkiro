// Types shared by the workspace shell and its pages.
import type {MutableRefObject} from 'react';

export type Row = Record<string, unknown>;

export type Tab = 'overview' | 'cards' | 'traces' | 'groups' | 'models' | 'providers' | 'announcements' | 'reconciliation' | 'security';

export type CardTab = 'CURRENT' | 'UNACTIVATED' | 'ACTIVE' | 'FROZEN' | 'BANNED' | 'EXPIRED' | 'ARCHIVED' | 'VOIDED' | 'ALL';
export type CardQuickFilter = 'expiring' | 'low';
export type TraceTab = 'ALL' | 'error' | 'client_aborted' | 'in_progress' | 'success';
export type TraceWindow = 'hour' | 'day' | 'all';

/** Where a link from another page wants a list to start. */
export interface Intent {
  cards?: {status?: CardTab; quick?: CardQuickFilter; search?: string};
  traces?: {status?: TraceTab; window?: TraceWindow; search?: string; open?: string};
}

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
