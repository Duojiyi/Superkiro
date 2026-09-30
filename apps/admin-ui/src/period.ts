// The periods 财务对账 reports on, in the operator's local time: from the start of the first day up
// to (not including) the start of the day after the last, as the server takes fromSecs and toSecs.
// And the ledger CSV cut to such a period, byte for byte. Pure functions, so the rules can be
// tested without a browser.

export type PeriodKind = 'all' | 'today' | 'yesterday' | 'month' | 'lastMonth' | 'custom';
export interface PeriodRange {fromSecs?: number; toSecs?: number}

export const PERIOD_LABEL: Record<PeriodKind, string> = {all: '累计', today: '今天', yesterday: '昨天', month: '本月', lastMonth: '上月', custom: '自定义'};

const pad = (value: number) => String(value).padStart(2, '0');
const secs = (date: Date) => Math.floor(date.getTime() / 1000);
const day = (year: number, month: number, date: number) => new Date(year, month, date);

/** A local date as a date input shows it: 2026-09-27. */
export const dateInput = (date: Date) => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;

/** The start of a local day given as 2026-09-27, or null. */
function dayStart(text: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!match) return null;
  const date = day(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return date.getDate() === Number(match[3]) ? date : null;
}

/**
 * A period's bounds: 今天 from today's midnight to tomorrow's, 昨天 the day before, 本月 and 上月 by
 * calendar month, 自定义 from its first day to the end of its last, 累计 unbounded (every entry the
 * ledger keeps). Null for a custom period that is not one (a missing date, or the last before the first).
 */
export function periodRange(kind: PeriodKind, now = new Date(), custom?: {from: string; to: string}): PeriodRange | null {
  const [y, m, d] = [now.getFullYear(), now.getMonth(), now.getDate()];
  switch (kind) {
    case 'all': return {};
    case 'today': return {fromSecs: secs(day(y, m, d)), toSecs: secs(day(y, m, d + 1))};
    case 'yesterday': return {fromSecs: secs(day(y, m, d - 1)), toSecs: secs(day(y, m, d))};
    case 'month': return {fromSecs: secs(day(y, m, 1)), toSecs: secs(day(y, m + 1, 1))};
    case 'lastMonth': return {fromSecs: secs(day(y, m - 1, 1)), toSecs: secs(day(y, m, 1))};
    case 'custom': {
      const from = custom ? dayStart(custom.from) : null, to = custom ? dayStart(custom.to) : null;
      if (!from || !to || to < from) return null;
      return {fromSecs: secs(from), toSecs: secs(day(to.getFullYear(), to.getMonth(), to.getDate() + 1))};
    }
  }
}

/** A period in words: 2026-09-01 至 2026-09-30, or 2026-09-27 for one day; 累计 names the kept ledger. */
export function periodText(range: PeriodRange): string {
  if (range.fromSecs === undefined && range.toSecs === undefined) return '累计（全部保留账本）';
  const first = range.fromSecs !== undefined ? dateInput(new Date(range.fromSecs * 1000)) : '最早';
  const last = range.toSecs !== undefined ? dateInput(new Date((range.toSecs - 1) * 1000)) : '现在';
  return first === last ? first : `${first} 至 ${last}`;
}

/**
 * The offsets of each record of a CSV (RFC 4180: quoted cells may hold commas, quotes and line
 * breaks), with its cells: the file can be cut to some records without rewriting any.
 */
export function csvRecords(text: string): Array<{start: number; end: number; cells: string[]}> {
  const records: Array<{start: number; end: number; cells: string[]}> = [];
  let start = 0, cells: string[] = [], cell = '', quoted = false, index = 0;
  const finish = (end: number) => {cells.push(cell); records.push({start, end, cells}); cells = []; cell = '';};
  while (index < text.length) {
    const char = text[index];
    if (quoted) {
      if (char === '"' && text[index + 1] === '"') {cell += '"'; index += 2; continue;}
      if (char === '"') quoted = false; else cell += char;
      index++;
    } else if (char === '"') {quoted = true; index++;}
    else if (char === ',') {cells.push(cell); cell = ''; index++;}
    else if (char === '\n' || char === '\r') {
      finish(index);
      index += char === '\r' && text[index + 1] === '\n' ? 2 : 1;
      start = index;
    } else {cell += char; index++;}
  }
  if (index > start || cells.length || cell) finish(text.length);
  return records;
}

/**
 * The ledger CSV with only the entries of a period (by its ts column, from inclusive, to exclusive),
 * each kept byte for byte under the header. Null when the file has no ts column.
 */
export function ledgerForPeriod(csv: string, range: PeriodRange): {csv: string; entries: number} | null {
  const records = csvRecords(csv);
  if (!records.length) return null;
  const column = records[0].cells.indexOf('ts');
  if (column < 0) return null;
  const newline = /\r\n/.test(csv) ? '\r\n' : '\n';
  const kept = records.slice(1).filter(record => {
    const ts = Number(record.cells[column]);
    return Number.isFinite(ts) && (range.fromSecs === undefined || ts >= range.fromSecs) && (range.toSecs === undefined || ts < range.toSecs);
  });
  const slice = (record: {start: number; end: number}) => csv.slice(record.start, record.end);
  return {csv: [slice(records[0]), ...kept.map(slice)].join(newline) + newline, entries: kept.length};
}
