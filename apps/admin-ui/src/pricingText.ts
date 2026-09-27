// How pricing figures read on 模型与定价: ¥ and $ per million tokens, margins, multipliers.
// Pure functions, so the Node tests can load this file directly.
type Row = Record<string, unknown>;

const trim = (text: string) => text.includes('.') ? text.replace(/0+$/, '').replace(/\.$/, '') : text;
/** ¥ per million tokens: two decimals, four below ¥0.01. */
export const yuanText = (value: number | null | undefined) => value === null || value === undefined || !Number.isFinite(value) ? '—'
  : `¥${trim(value !== 0 && Math.abs(value) < 0.01 ? value.toFixed(4) : value.toFixed(2))}`;
/** Official USD, as typed: $4 · $0.3. */
export const usdText = (value: number | null | undefined) => value === null || value === undefined || !Number.isFinite(value) ? '—' : `$${trim(value.toFixed(6))}`;
/** A margin (a fraction) in whole percent: 60% · −15%. */
export const marginText = (value: number | null | undefined) => value === null || value === undefined || !Number.isFinite(value) ? '—'
  : `${value < 0 ? '−' : ''}${Math.abs(Math.round(value * 100))}%`;
/** Red below 0, amber below 20%. */
export const marginTone = (value: number | null | undefined) => value === null || value === undefined ? '' : value < 0 ? 'is-loss' : value < 0.2 ? 'is-thin' : '';
/** A multiplier: ×0.24. */
export const timesText = (value: number | null | undefined) => value === null || value === undefined || !Number.isFinite(value) ? '—' : `×${trim(value.toFixed(6))}`;
/** A number typed into a field: null when blank, NaN when it is not a plain non-negative number. */
export function typedNumber(text: string): number | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  return /^[+]?\d*\.?\d+(e[+-]?\d+)?$/i.test(trimmed) ? Number(trimmed) : NaN;
}
/** A provider's name, else its ID. */
export const providerName = (providers: Row[], id: unknown) => String(providers.find(provider => provider.id === id)?.name ?? id ?? '');
