import { describe, expect, test } from 'bun:test';
import type { FieldConfig, HojaRecord, SectionConfig } from './api';
import { collapse, eligible, fieldChanges, fromForm, href, lineDiff, move, parseRoute, patch, toForm, valueDiff, wordDiff } from './views';

const text: FieldConfig = { type: 'text', required: false, person_only: false };
const tags: FieldConfig = { type: 'tags', required: false, person_only: false };
const list: FieldConfig = { type: 'list', required: false, person_only: false };
const flag: FieldConfig = { type: 'bool', required: false, person_only: false };

function record(id: string, kind: string, fields: HojaRecord['fields'], deleted = false): HojaRecord {
  return { id, type: 't', kind, fields, reviewed: false, reviewed_at: null, version: 1, created_at: '', updated_at: '', deleted, created_by: null, updated_by: null };
}

describe('form values', () => {
  test('round-trip lists, tags and booleans', () => {
    expect(toForm(list, ['a, b', 'c'])).toBe('a, b\nc');
    expect(fromForm(list, 'a, b\n\nc ')).toEqual(['a, b', 'c']);
    expect(fromForm(tags, 'rust, go\nsql')).toEqual(['rust', 'go', 'sql']);
    expect(fromForm(text, '   ')).toBeNull();
    expect(fromForm(flag, false)).toBeNull();
    expect(fromForm(flag, true)).toBe(true);
  });

  test('patch carries only changed fields, with null to clear', () => {
    const fields = { text, tags };
    const current = { text: 'Hello', tags: ['a'] };
    expect(patch(fields, current, { text: 'Hello', tags: 'a' })).toEqual({});
    expect(patch(fields, current, { text: 'Hi', tags: '' })).toEqual({ text: 'Hi', tags: null });
  });
});

test('fieldChanges skips unchanged values', () => {
  expect(fieldChanges({ a: 'x', b: 'y' }, { a: 'x', b: 'z', c: null })).toEqual([{ name: 'b', before: 'y', after: 'z' }]);
});

test('eligible respects kind, where filters and deletion', () => {
  const section: SectionConfig = { name: 'done', kind: 'item', where: { status: 'done' } };
  const records = [
    record('1', 'item', { status: 'done' }),
    record('2', 'item', { status: 'next' }),
    record('3', 'note', { status: 'done' }),
    record('4', 'item', { status: 'done' }, true)
  ];
  expect(eligible(records, section, 't').map(r => r.id)).toEqual(['1']);
  // A section can select from another type; then only that type's records qualify.
  expect(eligible(records, { ...section, type: 'raid' }, 't')).toEqual([]);
  const raid = { ...record('5', 'item', { status: 'done' }), type: 'raid' };
  expect(eligible([...records, raid], { ...section, type: 'raid' }, 't').map(r => r.id)).toEqual(['5']);
});

test('move swaps neighbours and ignores out-of-range moves', () => {
  expect(move(['a', 'b', 'c'], 1, -1)).toEqual(['b', 'a', 'c']);
  expect(move(['a', 'b'], 0, -1)).toEqual(['a', 'b']);
});

test('lineDiff marks additions and removals', () => {
  expect(lineDiff('a\nb\nc', 'a\nc\nd')).toEqual([
    { kind: 'same', text: 'a' },
    { kind: 'remove', text: 'b' },
    { kind: 'same', text: 'c' },
    { kind: 'add', text: 'd' }
  ]);
});

test('routes round-trip through the hash', () => {
  for (const route of [
    { view: 'documents' as const },
    { view: 'document' as const, id: 'abc' },
    { view: 'records' as const, type: 'status-report', kind: 'item', id: 'x/y' },
    { view: 'approvals' as const },
    { view: 'types' as const }
  ]) {
    expect(parseRoute(href(route))).toEqual(route);
  }
  expect(parseRoute('#/nonsense')).toEqual({ view: 'documents' });
});

test('collapse folds unchanged runs outside the context', () => {
  const lines = lineDiff('1\n2\n3\n4\n5\n6', '1\n2\n3\n4\n5\nsix');
  expect(collapse(lines, 1).map(l => l.kind)).toEqual(['skip', 'same', 'remove', 'add']);
  expect(collapse(lines, 1)[0].text).toBe('4 unchanged lines');
});

test('wordDiff marks changed words and joins rewritten phrases', () => {
  expect(wordDiff('Led the migration to Postgres', 'Led the zero-downtime migration to Postgres 16')).toEqual([
    { kind: 'same', text: 'Led the ' },
    { kind: 'add', text: 'zero-downtime ' },
    { kind: 'same', text: 'migration to Postgres' },
    { kind: 'add', text: ' 16' }
  ]);
  expect(wordDiff('ship in March next year', 'ship in May this year')).toEqual([
    { kind: 'same', text: 'ship in ' },
    { kind: 'remove', text: 'March next' },
    { kind: 'add', text: 'May this' },
    { kind: 'same', text: ' year' }
  ]);
  expect(wordDiff('same', 'same')).toEqual([{ kind: 'same', text: 'same' }]);
});

test('valueDiff compares list entries and scalars', () => {
  expect(valueDiff(['!1', 'A-2'], ['!1', 'A-3'])).toEqual([
    { kind: 'same', text: '!1, ' },
    { kind: 'remove', text: 'A-2' },
    { kind: 'same', text: ', ' },
    { kind: 'add', text: 'A-3' }
  ]);
  expect(valueDiff(undefined, 'new')).toEqual([{ kind: 'add', text: 'new' }]);
  expect(valueDiff(true, null)).toEqual([{ kind: 'remove', text: 'yes' }]);
});
