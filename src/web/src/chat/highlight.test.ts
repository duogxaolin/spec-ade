import { describe, expect, it } from 'vitest';

import { highlightToHtml, resolveLanguage } from './highlight';

// SPEC-004 B5-B6. No DOM needed: highlight.js emits a string.

describe('resolveLanguage', () => {
  it('resolves every explicitly registered grammar', () => {
    const registered = [
      'bash', 'css', 'diff', 'dockerfile', 'go', 'ini', 'java', 'javascript',
      'json', 'markdown', 'python', 'rust', 'sql', 'typescript', 'xml', 'yaml',
    ];
    for (const name of registered) expect(resolveLanguage(name)).toBe(name);
  });

  it('maps the aliases agents actually write', () => {
    expect(resolveLanguage('ts')).toBe('typescript');
    expect(resolveLanguage('tsx')).toBe('typescript');
    expect(resolveLanguage('js')).toBe('javascript');
    expect(resolveLanguage('rs')).toBe('rust');
    expect(resolveLanguage('py')).toBe('python');
    expect(resolveLanguage('sh')).toBe('bash');
    expect(resolveLanguage('shell')).toBe('bash');
    expect(resolveLanguage('html')).toBe('xml');
    expect(resolveLanguage('vue')).toBe('xml');
    expect(resolveLanguage('yml')).toBe('yaml');
    expect(resolveLanguage('toml')).toBe('ini');
    expect(resolveLanguage('md')).toBe('markdown');
    expect(resolveLanguage('patch')).toBe('diff');
    expect(resolveLanguage('golang')).toBe('go');
  });

  it('is case-insensitive', () => {
    expect(resolveLanguage('Rust')).toBe('rust');
    expect(resolveLanguage('TS')).toBe('typescript');
  });

  // Fences carry extras: ```ts title="a.ts"
  it('uses only the first token of the info string', () => {
    expect(resolveLanguage('ts title="a.ts"')).toBe('typescript');
    expect(resolveLanguage('  rust  ignore')).toBe('rust');
  });

  it('returns null for an empty info string', () => {
    expect(resolveLanguage('')).toBeNull();
    expect(resolveLanguage('   ')).toBeNull();
  });

  // Not registering a grammar is a bundle-size decision, and the fallback has to be
  // "plain code", never "guess" — highlightAuto would ship every language.
  it('returns null for a real language that is deliberately not bundled', () => {
    for (const name of ['haskell', 'perl', 'cobol', 'elixir']) {
      expect(resolveLanguage(name)).toBeNull();
    }
  });

  it('returns null for mermaid, which is rendered as a diagram instead', () => {
    expect(resolveLanguage('mermaid')).toBeNull();
  });
});

describe('highlightToHtml', () => {
  it('returns an empty string for an unknown language, deferring to markdown-it', () => {
    expect(highlightToHtml('some code', 'haskell')).toBe('');
    expect(highlightToHtml('some code', '')).toBe('');
  });

  it('wraps highlighted code with the language class', () => {
    const html = highlightToHtml('fn main() {}', 'rust');
    expect(html).toContain('<pre class="hljs">');
    expect(html).toContain('<code class="language-rust">');
  });

  it('emits highlight spans for real keywords', () => {
    expect(highlightToHtml('fn main() {}', 'rust')).toContain('<span class="hljs-');
  });

  it('uses the resolved name in the class, not the alias', () => {
    expect(highlightToHtml('const a = 1;', 'ts')).toContain('language-typescript');
  });

  // hljs escapes its input; the sanitizer in renderMarkdown is the second line.
  it('escapes HTML in the code it highlights', () => {
    const html = highlightToHtml('let x = "<script>alert(1)</script>";', 'javascript');
    expect(html).not.toContain('<script>');
    expect(html).toContain('&lt;script&gt;');
  });

  it('escapes an ampersand', () => {
    expect(highlightToHtml('a && b', 'javascript')).toContain('&amp;&amp;');
  });

  it('does not throw on code that does not match its declared language', () => {
    // ignoreIllegals: true — an agent mislabelling a fence must not blank the reply.
    expect(() => highlightToHtml('%%% not rust at all %%%', 'rust')).not.toThrow();
    expect(highlightToHtml('%%% not rust %%%', 'rust')).toContain('<code');
  });

  it('handles empty code', () => {
    expect(highlightToHtml('', 'rust')).toContain('<code class="language-rust">');
  });
});
