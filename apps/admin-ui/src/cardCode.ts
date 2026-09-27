// A card code as a customer quotes it (kiro-3eba-7810-…, or the hex alone, in any case, with
// dashes or spaces), turned into the card's ID the way the server makes it: the code normalised
// as crates/billing/src/card.rs normalises it, SHA-256, then `card-` and the first 16 hex digits
// (generator.rs). The code itself goes nowhere: not to the server, not into the address bar, the
// history or storage. Pure functions without imports, so the Node tests can load this file.

// Rust's str::trim removes Unicode White_Space, a slightly different set from String.trim's.
const SPACE = '[\\t\\n\\v\\f\\r \\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000]';
const EDGES = new RegExp(`^${SPACE}+|${SPACE}+$`, 'g');

/** normalize_card_code: trimmed, ASCII letters in lower case, dashes and spaces removed. */
export function normalizeCardCode(raw: string): string {
  return raw.replace(EDGES, '').replace(/[A-Z]/g, letter => letter.toLowerCase()).replace(/[- ]/g, '');
}

/**
 * The code as the server hashes it (kiro and 32 hex digits), or null when the text is not a
 * whole code. The hex alone gets the kiro every issued code starts with: it is hashed too.
 */
export function cardCodeOf(text: string): string | null {
  const code = normalizeCardCode(text);
  if (/^kiro[0-9a-f]{32}$/.test(code)) return code;
  return /^[0-9a-f]{32}$/.test(code) ? `kiro${code}` : null;
}

/**
 * Text that is, or may be the start of, a card code: kept out of addresses and history even
 * while it is being typed. A card ID has 16 hex digits and no dash inside them.
 */
export function looksLikeCardCode(text: string): boolean {
  const code = normalizeCardCode(text);
  return /^kiro[0-9a-f]+$/.test(code) || /^[0-9a-f]{17,}$/.test(code) || (/[- ]/.test(text.trim()) && /^[0-9a-f]{8,}$/.test(code));
}

/** The ID of the card this code belongs to (the code as cardCodeOf returns it). */
export async function cardIdForCode(code: string): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(code)));
  return `card-${Array.from(digest.slice(0, 8), byte => byte.toString(16).padStart(2, '0')).join('')}`;
}
