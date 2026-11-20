import { describe, expect, test } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

// A Tracon node serves this page under `/t/<token>/`, so every URL it uses
// must be relative: an absolute `/api/...` or `/hoja.svg` would miss the prefix.
const root = join(import.meta.dir, '..', '..');

function sources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap(entry => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sources(path);
    return /\.(svelte|ts)$/.test(entry.name) && !entry.name.endsWith('.test.ts') ? [path] : [];
  });
}

describe('relative URLs', () => {
  test('the page and its code use no root-relative URLs', () => {
    const files = [join(root, 'index.html'), ...sources(join(root, 'src'))];
    for (const file of files) {
      const text = readFileSync(file, 'utf8');
      expect({ file, hits: text.match(/(?:['"`]|(?:src|href)=["'])\/(?:api|assets|hoja\.svg|src)\b/g) }).toEqual({ file, hits: null });
    }
  });

  test('index.html has no inline script', () => {
    const html = readFileSync(join(root, 'index.html'), 'utf8');
    expect(html.match(/<script(?![^>]*\bsrc=)[^>]*>/g)).toBeNull();
  });

  test('vite builds with a relative base', () => {
    expect(readFileSync(join(root, 'vite.config.ts'), 'utf8')).toContain("base: './'");
  });
});
