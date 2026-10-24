import type { FieldConfig, Fields, HojaRecord, KindConfig, Patch, SectionConfig, TypeConfig, Value } from './api';

export function titleOf(name: string, config?: { title?: string }): string {
  if (config?.title) return config.title;
  const spaced = name.replace(/[_-]+/g, ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

export function labelField(kind: KindConfig): string | null {
  if (kind.label) return kind.label;
  return Object.entries(kind.fields).find(([, f]) => f.type === 'string' || f.type === 'text')?.[0] ?? null;
}

export function display(value: Value | undefined | null): string {
  if (value === undefined || value === null) return '';
  if (Array.isArray(value)) return value.join(', ');
  if (typeof value === 'boolean') return value ? 'yes' : 'no';
  return value;
}

export function recordLabel(config: TypeConfig | null | undefined, record: HojaRecord): string {
  const kind = config?.kinds[record.kind];
  const field = kind ? labelField(kind) : null;
  const text = field ? display(record.fields[field]) : '';
  return text || `${record.kind} ${record.id.slice(0, 8)}`;
}

export function filterRecords(records: HojaRecord[], query: string): HojaRecord[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return records;
  return records.filter(record => Object.values(record.fields).some(v => display(v).toLowerCase().includes(needle)));
}

/** Records a section may hold: the right kind, and matching its `where` filter. */
export function eligible(records: HojaRecord[], section: SectionConfig): HojaRecord[] {
  return records.filter(record => record.kind === section.kind && !record.deleted
    && Object.entries(section.where ?? {}).every(([field, expected]) => record.fields[field] === expected));
}

export function move<T>(list: T[], index: number, delta: number): T[] {
  const target = index + delta;
  if (index < 0 || index >= list.length || target < 0 || target >= list.length) return list;
  const next = [...list];
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

/** The text a form control shows for a stored value. */
export function toForm(field: FieldConfig, value: Value | undefined): string | boolean {
  if (field.type === 'bool') return value === true;
  if (field.type === 'tags' || field.type === 'list') return Array.isArray(value) ? value.join('\n') : '';
  return typeof value === 'string' ? value : '';
}

/** A form control's text back as a value; null clears the field. */
export function fromForm(field: FieldConfig, form: string | boolean): Value | null {
  if (field.type === 'bool') return form === true ? true : null;
  const text = typeof form === 'string' ? form : '';
  if (field.type === 'tags' || field.type === 'list') {
    // Tags may be typed comma-separated; list entries are one per line and may contain commas.
    const items = text.split(field.type === 'tags' ? /[\n,]/ : /\n/).map(s => s.trim()).filter(Boolean);
    return items.length ? items : null;
  }
  return text.trim() ? text : null;
}

function same(a: Value | null | undefined, b: Value | null | undefined): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null);
}

/** Only the fields that changed, so a revision shows what the person actually edited. */
export function patch(fields: Record<string, FieldConfig>, current: Fields, form: Record<string, string | boolean>): Patch {
  const out: Patch = {};
  for (const [name, field] of Object.entries(fields)) {
    if (!(name in form)) continue;
    const next = fromForm(field, form[name]);
    if (!same(next, current[name])) out[name] = next;
  }
  return out;
}

export function formOf(fields: Record<string, FieldConfig>, values: Fields): Record<string, string | boolean> {
  return Object.fromEntries(Object.entries(fields).map(([name, field]) => [name, toForm(field, values[name])]));
}

export interface FieldChange { name: string; before: string; after: string }

export function fieldChanges(before: Fields | undefined, after: Patch | Fields | undefined): FieldChange[] {
  return Object.entries(after ?? {})
    .filter(([name, value]) => !same(value, before?.[name]))
    .map(([name, value]) => ({ name, before: display(before?.[name]), after: display(value ?? undefined) }));
}

export type DiffLine = { kind: 'same' | 'add' | 'remove' | 'skip'; text: string };

/** A line diff by longest common subsequence; type files are small. */
export function lineDiff(before: string, after: string): DiffLine[] {
  const a = before.split('\n');
  const b = after.split('\n');
  const lcs: number[][] = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i--) {
    for (let j = b.length - 1; j >= 0; j--) {
      lcs[i][j] = a[i] === b[j] ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length && j < b.length) {
    if (a[i] === b[j]) { out.push({ kind: 'same', text: a[i] }); i++; j++; }
    else if (lcs[i + 1][j] >= lcs[i][j + 1]) out.push({ kind: 'remove', text: a[i++] });
    else out.push({ kind: 'add', text: b[j++] });
  }
  while (i < a.length) out.push({ kind: 'remove', text: a[i++] });
  while (j < b.length) out.push({ kind: 'add', text: b[j++] });
  return out;
}

/** Keeps `context` unchanged lines around each change and folds the rest. */
export function collapse(lines: DiffLine[], context = 3): DiffLine[] {
  const near = lines.map((_, i) => lines.slice(Math.max(0, i - context), i + context + 1).some(l => l.kind !== 'same'));
  const out: DiffLine[] = [];
  let skipped = 0;
  lines.forEach((line, i) => {
    if (near[i]) {
      if (skipped) out.push({ kind: 'skip', text: `${skipped} unchanged line${skipped === 1 ? '' : 's'}` });
      skipped = 0;
      out.push(line);
    } else skipped++;
  });
  if (skipped) out.push({ kind: 'skip', text: `${skipped} unchanged line${skipped === 1 ? '' : 's'}` });
  return out;
}

export function authorText(by: { author: string; agent?: string | null } | null | undefined): string {
  if (!by) return '';
  if (by.author === 'person') return 'you';
  if (by.author === 'agent_accepted') return `${by.agent ?? 'agent'}, accepted by you`;
  return by.agent ?? 'agent';
}

export function when(iso: string | null | undefined): string {
  if (!iso) return '';
  const date = new Date(iso);
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' });
}

export type Route =
  | { view: 'documents' }
  | { view: 'document'; id: string }
  | { view: 'records'; type: string; kind: string; id?: string }
  | { view: 'approvals' }
  | { view: 'types' };

export function parseRoute(hash: string): Route {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean).map(decodeURIComponent);
  switch (parts[0]) {
    case 'documents': return parts[1] ? { view: 'document', id: parts[1] } : { view: 'documents' };
    case 'records': if (parts[1] && parts[2]) return { view: 'records', type: parts[1], kind: parts[2], id: parts[3] }; break;
    case 'approvals': return { view: 'approvals' };
    case 'types': return { view: 'types' };
  }
  return { view: 'documents' };
}

export function href(route: Route): string {
  const e = encodeURIComponent;
  switch (route.view) {
    case 'documents': return '#/documents';
    case 'document': return `#/documents/${e(route.id)}`;
    case 'records': return `#/records/${e(route.type)}/${e(route.kind)}${route.id ? `/${e(route.id)}` : ''}`;
    case 'approvals': return '#/approvals';
    case 'types': return '#/types';
  }
}
