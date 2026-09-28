// Addresses for the console's pages: #/cards?q=…&open=card-…, so a reload, a bookmark, a link
// or Back finds the same page, filters and open details. Only what a page names below is kept,
// only in values it accepts, and never anything that looks like a card code. Pure functions,
// so the rules can be tested without a browser.
import {looksLikeCardCode} from './cardCode';
import type {Intent, Tab} from './types';

export interface Route {tab: Tab; params: Record<string, string>}

const PATHS: Record<Tab, string> = {overview: 'overview', cards: 'cards', traces: 'traces', groups: 'groups', plans: 'plans', models: 'models',
  templates: 'templates', providers: 'providers', announcements: 'announcements', reconciliation: 'finance', security: 'security'};

type Check = (value: string) => boolean;
const text: Check = value => !!value && value.length <= 512 && !/[\x00-\x1f\x7f]/.test(value);
const search: Check = value => text(value) && !looksLikeCardCode(value);
const oneOf = (...values: string[]): Check => value => values.includes(value);
const minute: Check = value => /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/.test(value);

// Each page's address, in this order; a default (当前, 全部) is left out.
const PARAMS: Partial<Record<Tab, Record<string, Check>>> = {
  cards: {q: search, status: oneOf('UNACTIVATED', 'ACTIVE', 'FROZEN', 'BANNED', 'EXPIRED', 'ARCHIVED', 'VOIDED', 'ALL'),
    quick: oneOf('expiring', 'low'), group: text, open: text},
  traces: {card: text, q: search, status: oneOf('error', 'refused', 'client_aborted', 'in_progress', 'success'), range: oneOf('hour', 'day', 'custom'), from: minute, to: minute,
    // An empty reason is 未分类: a filter of its own.
    reason: value => value === '' || text(value), model: text, provider: text, open: text},
  // 模型与定价: the model a link named (a failing or loss-making model, a broken route).
  models: {model: text},
  providers: {provider: text, key: text, edit: text},
};

/** The parameter that says details are open: opening or closing them is a step Back undoes. */
export const OPEN_PARAM: Partial<Record<Tab, string>> = {cards: 'open', traces: 'open', providers: 'edit'};

function kept(tab: Tab, read: (name: string) => string | null | undefined): Record<string, string> {
  const params: Record<string, string> = {};
  for (const [name, check] of Object.entries(PARAMS[tab] ?? {})) {
    const value = read(name);
    if (typeof value === 'string' && check(value)) params[name] = value;
  }
  return params;
}

/** The page an address names (运营概览 for anything else), with the parameters it keeps. */
export function parseRoute(hash: string): Route {
  const match = /^#\/([a-z]+)\/?(?:\?(.*))?$/.exec(hash);
  const tab = match ? (Object.keys(PATHS) as Tab[]).find(key => PATHS[key] === match[1]) : undefined;
  if (!tab) return {tab: 'overview', params: {}};
  const query = new URLSearchParams(match![2] ?? '');
  return {tab, params: kept(tab, name => query.get(name))};
}

export function routeHash({tab, params}: Route): string {
  const query = new URLSearchParams(kept(tab, name => params[name])).toString();
  return `#/${PATHS[tab]}${query ? `?${query}` : ''}`;
}

/** Where a page starts from its address. */
export function intentOf({tab, params}: Route): Intent {
  if (tab === 'cards') return {cards: {status: params.status as never, quick: params.quick as never, search: params.q, group: params.group, open: params.open}};
  if (tab === 'traces') return {traces: {status: params.status as never, window: params.range as never, from: params.from, to: params.to, search: params.card ?? params.q,
    card: params.card, reason: params.reason, model: params.model, provider: params.provider, open: params.open}};
  if (tab === 'models') return {models: {model: params.model}};
  if (tab === 'providers') return {providers: {provider: params.provider, key: params.key, edit: params.edit}};
  return {};
}

/** A page's address for where it is (its state in the words of an Intent). */
export function routeOf(tab: Tab, intent: Intent = {}): Route {
  const cards = intent.cards, traces = intent.traces, providers = intent.providers, models = intent.models;
  const raw: Record<string, string | undefined> = tab === 'cards' ? {q: cards?.search?.trim(), status: cards?.status === 'CURRENT' ? undefined : cards?.status,
      quick: cards?.quick, group: cards?.group === 'ALL' ? undefined : cards?.group, open: cards?.open}
    : tab === 'traces' ? {card: traces?.card, q: traces?.card ? undefined : traces?.search?.trim(), status: traces?.status === 'ALL' ? undefined : traces?.status,
      range: traces?.window === 'all' ? undefined : traces?.window, from: traces?.window === 'custom' ? traces.from : undefined, to: traces?.window === 'custom' ? traces.to : undefined,
      reason: traces?.reason, model: traces?.model === 'ALL' ? undefined : traces?.model,
      provider: traces?.provider === 'ALL' ? undefined : traces?.provider, open: traces?.open}
    : tab === 'models' ? {model: models?.model}
    : tab === 'providers' ? {provider: providers?.provider, key: providers?.key, edit: providers?.edit} : {};
  return {tab, params: kept(tab, name => raw[name])};
}
