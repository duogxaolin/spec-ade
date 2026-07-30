// Unit tests for extension → language mapping (SPEC-002 test matrix, "FE unit").
//
// Worth testing because the failure mode is quiet: a wrong or missing mode still
// opens the file, just without highlighting, so nobody notices until someone
// complains that `.tsx` looks like plain text.

import { describe, expect, it } from 'vitest';

import { languageFor } from './languages';

/** An `Extension` is a nested array; `[]` is the plaintext fallback. */
function isPlaintext(ext: unknown): boolean {
  return Array.isArray(ext) && ext.length === 0;
}

describe('languageFor', () => {
  it('maps every extension named in the spec table', () => {
    const mapped = [
      'a.js', 'a.jsx', 'a.mjs', 'a.cjs', 'a.ts', 'a.tsx',
      'a.json', 'a.jsonc',
      'a.html', 'a.htm',
      'a.css',
      'a.md', 'a.markdown',
      'a.rs',
      'a.py',
      'a.vue',
      'a.yml', 'a.yaml',
      'a.xml', 'a.svg',
      'a.toml', 'a.sh', 'a.bash', 'a.ini',
    ];
    for (const path of mapped) {
      expect(isPlaintext(languageFor(path)), `${path} must have a mode`).toBe(false);
    }
  });

  it('falls back to plaintext for unknown and extensionless files', () => {
    expect(isPlaintext(languageFor('notes.qqq'))).toBe(true);
    expect(isPlaintext(languageFor('LICENSE'))).toBe(true);
    expect(isPlaintext(languageFor('src/bin/tool'))).toBe(true);
  });

  it('resolves by extension regardless of directory depth or case', () => {
    expect(isPlaintext(languageFor('src/server/src/main.RS'))).toBe(false);
    expect(isPlaintext(languageFor('deep/nested/path/app.Vue'))).toBe(false);
  });

  it('treats whole-name files as their own type', () => {
    // `Dockerfile` has no extension at all, and `.gitignore` is all "extension"
    // — both would land on plaintext under naive `split('.')` logic.
    expect(isPlaintext(languageFor('Dockerfile'))).toBe(false);
    expect(isPlaintext(languageFor('.gitignore'))).toBe(false);
  });

  it('does not read a dotfile suffix as an extension', () => {
    // `.env.local`'s "extension" is `local`, which maps to nothing; the point is
    // that it resolves to plaintext instead of throwing or matching `.env`.
    expect(isPlaintext(languageFor('.env.local'))).toBe(true);
  });
});
