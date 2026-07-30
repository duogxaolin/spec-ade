// Extension → CodeMirror language mode (SPEC-002 §5.7).
//
// Imports are STATIC by design ([INVENTED-17]). Dynamic `import()` makes fast tab
// switching race: the awaited extension can land on a state that has already been
// replaced, so the wrong file gets the wrong highlighting. A few dozen KB of
// bundle is a fair price for an app served from localhost.

import type { Extension } from '@codemirror/state';
import { StreamLanguage } from '@codemirror/language';

import { css } from '@codemirror/lang-css';
import { html } from '@codemirror/lang-html';
import { javascript } from '@codemirror/lang-javascript';
import { json } from '@codemirror/lang-json';
import { markdown } from '@codemirror/lang-markdown';
import { python } from '@codemirror/lang-python';
import { rust } from '@codemirror/lang-rust';
import { vue } from '@codemirror/lang-vue';
import { xml } from '@codemirror/lang-xml';
import { yaml } from '@codemirror/lang-yaml';

// Long-tail modes come from `legacy-modes`, which `04:37` explicitly allows.
import { toml } from '@codemirror/legacy-modes/mode/toml';
import { shell } from '@codemirror/legacy-modes/mode/shell';
import { properties } from '@codemirror/legacy-modes/mode/properties';
import { dockerFile } from '@codemirror/legacy-modes/mode/dockerfile';

/**
 * Modes keyed by lowercase extension. Anything absent falls back to plaintext —
 * an unhighlighted file still opens, which beats refusing to show it.
 */
const BY_EXTENSION: Record<string, () => Extension> = {
  js: () => javascript(),
  jsx: () => javascript({ jsx: true }),
  mjs: () => javascript(),
  cjs: () => javascript(),
  ts: () => javascript({ typescript: true }),
  tsx: () => javascript({ typescript: true, jsx: true }),
  json: () => json(),
  jsonc: () => json(),
  html: () => html(),
  htm: () => html(),
  css: () => css(),
  md: () => markdown(),
  markdown: () => markdown(),
  rs: () => rust(),
  py: () => python(),
  vue: () => vue(),
  yml: () => yaml(),
  yaml: () => yaml(),
  xml: () => xml(),
  svg: () => xml(),
  toml: () => StreamLanguage.define(toml),
  sh: () => StreamLanguage.define(shell),
  bash: () => StreamLanguage.define(shell),
  zsh: () => StreamLanguage.define(shell),
  ini: () => StreamLanguage.define(properties),
};

/**
 * Filenames with no useful extension. `Dockerfile` and `.gitignore` are the
 * common cases where the whole name is the type.
 */
const BY_FILENAME: Record<string, () => Extension> = {
  dockerfile: () => StreamLanguage.define(dockerFile),
  '.gitignore': () => StreamLanguage.define(properties),
  '.env': () => StreamLanguage.define(properties),
};

/** The language extension for a path, or `[]` for plaintext. */
export function languageFor(path: string): Extension {
  const name = (path.split('/').pop() ?? path).toLowerCase();

  const byName = BY_FILENAME[name];
  if (byName) return byName();

  // `.gitignore` has no extension in the `a.b` sense; guard the dotfile case so
  // `.env.local` doesn't get "local" as its extension.
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return [];

  const ext = name.slice(dot + 1);
  const byExt = BY_EXTENSION[ext];
  return byExt ? byExt() : [];
}
