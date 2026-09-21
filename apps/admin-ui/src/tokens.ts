// Decimal token units, never model capability inference. Invalid input stays editable.
export function parseTokenInput(raw: string): number | string {
  const match = raw.trim().match(/^(\d+(?:\.\d+)?)\s*([kKmM]?)$/);
  if (!match) return raw;
  const [whole, fraction = ''] = match[1].split('.');
  const scale = match[2].toLowerCase() === 'm' ? 6 : match[2] ? 3 : 0;
  const digits = fraction.replace(/0+$/, '');
  if (digits.length > scale) return raw;
  const value = Number(whole + digits.padEnd(scale, '0'));
  return Number.isSafeInteger(value) && value > 0 ? value : raw;
}
export function formatTokens(value: unknown): string {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value <= 0) return '请输入正整数 Tokens';
  const compact = value >= 1_000_000 ? `${value / 1_000_000}M` : value >= 1_000 ? `${value / 1_000}K` : String(value);
  return `${compact} Tokens（${value.toLocaleString('en-US')}）`;
}
