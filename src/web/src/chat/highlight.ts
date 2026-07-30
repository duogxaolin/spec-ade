// Syntax highlighting for chat code fences (SPEC-004 §5.5).
//
// `highlight.js/lib/core` + explicit registration, never `import hljs from
// 'highlight.js'`: the barrel pulls all 192 bundled languages, and this bundle is
// already 1.2 MB. The list below mirrors `editor/languages.ts` so a file opened in
// the editor and the same file quoted in chat are coloured by the same set.

import hljs from 'highlight.js/lib/core';

import bash from 'highlight.js/lib/languages/bash';
import css from 'highlight.js/lib/languages/css';
import diff from 'highlight.js/lib/languages/diff';
import dockerfile from 'highlight.js/lib/languages/dockerfile';
import go from 'highlight.js/lib/languages/go';
import ini from 'highlight.js/lib/languages/ini';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import markdown from 'highlight.js/lib/languages/markdown';
import python from 'highlight.js/lib/languages/python';
import rust from 'highlight.js/lib/languages/rust';
import sql from 'highlight.js/lib/languages/sql';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';

// `ini` covers TOML well enough to be worth the alias; `xml` is what highlight.js
// calls its HTML grammar.
hljs.registerLanguage('bash', bash);
hljs.registerLanguage('css', css);
hljs.registerLanguage('diff', diff);
hljs.registerLanguage('dockerfile', dockerfile);
hljs.registerLanguage('go', go);
hljs.registerLanguage('ini', ini);
hljs.registerLanguage('java', java);
hljs.registerLanguage('javascript', javascript);
hljs.registerLanguage('json', json);
hljs.registerLanguage('markdown', markdown);
hljs.registerLanguage('python', python);
hljs.registerLanguage('rust', rust);
hljs.registerLanguage('sql', sql);
hljs.registerLanguage('typescript', typescript);
hljs.registerLanguage('xml', xml);
hljs.registerLanguage('yaml', yaml);

/** Fence info strings agents write, mapped to a registered grammar name. */
const ALIASES: Record<string, string> = {
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  ts: 'typescript',
  tsx: 'typescript',
  rs: 'rust',
  py: 'python',
  sh: 'bash',
  shell: 'bash',
  zsh: 'bash',
  console: 'bash',
  html: 'xml',
  vue: 'xml',
  svg: 'xml',
  yml: 'yaml',
  toml: 'ini',
  conf: 'ini',
  md: 'markdown',
  patch: 'diff',
  jsonc: 'json',
  golang: 'go',
};

/** The grammar for a fence info string, or `null` to leave the code plain. */
export function resolveLanguage(info: string): string | null {
  // Fences carry extras after the language (` ```ts title="a.ts" `), so only the
  // first token is the language.
  const name = info.trim().split(/\s+/)[0]?.toLowerCase() ?? '';
  if (!name) return null;
  const resolved = ALIASES[name] ?? name;
  return hljs.getLanguage(resolved) ? resolved : null;
}

/**
 * markdown-it's `highlight` callback.
 *
 * Returning `''` tells markdown-it to escape the code itself and wrap it in its
 * own `<pre><code>`. That is the right fallback for an unknown language: the code
 * still shows, just without colour ([SPEC-004 §5.5]).
 *
 * The returned HTML is a string built here and inserted by markdown-it without
 * escaping, which is exactly why `renderMarkdown` still runs DOMPurify over the
 * result — `hljs` only ever emits `<span class>`, but that is an invariant of a
 * dependency, not something this file can prove.
 */
export function highlightToHtml(code: string, info: string): string {
  const language = resolveLanguage(info);
  if (!language) return '';
  try {
    const { value } = hljs.highlight(code, { language, ignoreIllegals: true });
    return `<pre class="hljs"><code class="language-${language}">${value}</code></pre>`;
  } catch {
    // A grammar can throw on pathological input. Plain code beats a blank reply.
    return '';
  }
}
