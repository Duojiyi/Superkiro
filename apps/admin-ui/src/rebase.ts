// 重新加载并保留修改: the edits in a draft, made on the configuration read back then, applied again
// onto the configuration the server holds now. An edit applies where that field still is what it
// was when the draft began; a field someone else has changed since keeps the server's value and
// is reported, as is an entry deleted meanwhile or a new one whose ID has been taken. Pure
// functions, so the rules can be tested without a browser.

type Row = Record<string, unknown>;

export interface Skipped {
  /** The entry, as the draft has it. */
  row: Row;
  /** For `changed`: the field, and what the server has now. */
  field?: string;
  server?: unknown;
  reason: 'changed' | 'removed' | 'exists';
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** One list of entries (groups, models or rate cards) rebased: the server's, with the draft's edits applied. */
export function rebaseRows(base: Row[], draft: Row[], server: Row[]): {rows: Row[]; applied: number; skipped: Skipped[]} {
  const rows = server.map(row => ({...row}));
  const skipped: Skipped[] = [];
  let applied = 0;
  for (const edit of draft) {
    const before = base.find(row => row.id === edit.id), now = rows.find(row => row.id === edit.id);
    if (!before) {
      if (now) skipped.push({row: edit, reason: 'exists'});
      else {rows.push({...edit}); applied++;}
      continue;
    }
    if (same(before, edit)) continue;
    if (!now) {skipped.push({row: edit, reason: 'removed'}); continue;}
    let changed = false;
    for (const field of new Set([...Object.keys(before), ...Object.keys(edit)])) {
      if (same(before[field], edit[field]) || same(now[field], edit[field])) continue;
      if (!same(now[field], before[field])) {skipped.push({row: edit, field, server: now[field], reason: 'changed'}); continue;}
      if (field in edit) now[field] = edit[field]; else delete now[field];
      changed = true;
    }
    if (changed) applied++;
  }
  return {rows, applied, skipped};
}

/**
 * A whole draft rebased: each list of entries as above; new price versions are kept unless the
 * server now has one with the same ID; other keys (settings) as the draft has them.
 */
export function rebaseDraft(base: Record<string, unknown>, draft: Record<string, unknown>, server: Record<string, unknown>): {draft: Record<string, unknown>; applied: number; skipped: Skipped[]} {
  const next: Record<string, unknown> = {...draft};
  const skipped: Skipped[] = [];
  let applied = 0;
  const rows = (value: unknown) => Array.isArray(value) ? value.filter((row): row is Row => !!row && typeof row === 'object' && !Array.isArray(row)) : [];
  for (const [section, value] of Object.entries(draft)) {
    if (section === 'versions') {
      const taken = new Set(rows(server.versions).map(version => version.id));
      next.versions = rows(value).filter(version => {
        if (!taken.has(version.id)) {applied++; return true;}
        skipped.push({row: version, reason: 'exists'});
        return false;
      });
    } else if (Array.isArray(value)) {
      const result = rebaseRows(rows(base[section]), rows(value), rows(server[section]));
      next[section] = result.rows; applied += result.applied; skipped.push(...result.skipped);
    }
  }
  return {draft: next, applied, skipped};
}
