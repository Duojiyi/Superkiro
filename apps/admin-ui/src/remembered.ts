// What the owner types once and uses again when pricing from official prices: the retail
// multiplier (one rule for the business), each upstream's cost multiplier, and each model's
// official USD prices. Kept in this browser only, as a convenience: nothing depends on it.

type Rates = {retail: string; upstream: Record<string, string>};
const RATES_KEY = 'admin-listing-rates:v2', OFFICIAL_KEY = 'admin-official-prices:v1';

const read = (key: string): Record<string, unknown> => {
  try {const value = JSON.parse(localStorage.getItem(key) || '{}'); return value && typeof value === 'object' && !Array.isArray(value) ? value : {};}
  catch {return {};}
};
const write = (key: string, value: unknown) => {try {localStorage.setItem(key, JSON.stringify(value));} catch {/* remembered only as a convenience */}};

export function loadRates(): Rates {
  const saved = read(RATES_KEY), upstream = saved.upstream && typeof saved.upstream === 'object' ? saved.upstream as Record<string, unknown> : {};
  return {retail: String(saved.retail ?? ''), upstream: Object.fromEntries(Object.entries(upstream).map(([id, value]) => [id, String(value)]))};
}
export const saveRates = (rates: Rates) => write(RATES_KEY, rates);

/** A model's official prices (input, output, cache write, cache read), or four blanks. */
export function loadOfficial(modelId: string): string[] {
  const saved = read(OFFICIAL_KEY)[modelId];
  return Array.isArray(saved) && saved.length === 4 ? saved.map(String) : ['', '', '', ''];
}
export const saveOfficial = (modelId: string, prices: string[]) => write(OFFICIAL_KEY, {...read(OFFICIAL_KEY), [modelId]: prices});
