// @vitest-environment jsdom
//
// jsdom is mandatory here, not a convenience: DOMPurify reports
// `isSupported === false` without a DOM and `renderMarkdown` throws by design
// rather than return unsanitized HTML (SPEC-004 §7.1).
//
// Honest limit, restated from the spec: jsdom is not a browser's parser. These
// tests prove the CONFIGURATION is right — that the allow-lists and hooks do what
// they claim. They do NOT prove immunity to mutation XSS, which depends on real
// parser quirks. That check stays a manual, real-browser item (§8.2).

import { describe, expect, it } from 'vitest';

import { escapeHtml, renderMarkdown } from './markdown';

/**
 * Render, then parse the result as HTML and assert on the DOM.
 *
 * Substring assertions are the wrong tool for this file. `html: false` escapes a
 * payload into TEXT, so `&lt;img onerror=alert(1)&gt;` legitimately contains the
 * substring "onerror" while being completely inert. What matters is whether an
 * ELEMENT ends up with that attribute — which only a parse can answer.
 */
function renderToDom(source: string): HTMLElement {
  const host = document.createElement('div');
  host.innerHTML = renderMarkdown(source);
  return host;
}

/** Every attribute name present anywhere in the parsed output. */
function attributeNames(root: HTMLElement): string[] {
  const names: string[] = [];
  for (const el of root.querySelectorAll('*')) {
    for (const attr of el.attributes) names.push(attr.name.toLowerCase());
  }
  return names;
}

describe('renderMarkdown formatting', () => {
  it('returns an empty string for empty input', () => {
    expect(renderMarkdown('')).toBe('');
  });

  it('renders basic markdown', () => {
    const html = renderMarkdown('# Tiêu đề\n\nvăn bản **đậm**');
    expect(html).toContain('<h1>Tiêu đề</h1>');
    expect(html).toContain('<strong>đậm</strong>');
  });

  it('renders lists and tables', () => {
    expect(renderMarkdown('- a\n- b')).toContain('<li>a</li>');
    const table = renderMarkdown('| a | b |\n| - | - |\n| 1 | 2 |');
    expect(table).toContain('<table>');
    expect(table).toContain('<td>1</td>');
  });

  it('keeps a lone newline as a space, not a break (breaks: false)', () => {
    expect(renderMarkdown('one\ntwo')).not.toContain('<br>');
  });

  it('renders fenced code with a language class', () => {
    const html = renderMarkdown('```rust\nfn main() {}\n```');
    expect(html).toContain('<code class="language-rust">');
  });

  it('escapes code content instead of interpreting it', () => {
    const html = renderMarkdown('```\n<script>alert(1)</script>\n```');
    expect(html).toContain('&lt;script&gt;');
    expect(html).not.toContain('<script>');
  });
});

// B1-B4: the XSS boundary. Each case names the layer it exercises.
describe('renderMarkdown XSS defence', () => {
  it('escapes a raw script tag at the parser (layer 1)', () => {
    const html = renderMarkdown('<script>alert(1)</script>');
    expect(html).not.toContain('<script');
    expect(html).toContain('&lt;script&gt;');
  });

  it('produces no element at all from an inline event handler payload', () => {
    const dom = renderToDom('<img src=x onerror=alert(1)>');
    // Layer 1 turned it into text, so there is no <img> to carry the handler.
    expect(dom.querySelector('img')).toBeNull();
    expect(attributeNames(dom)).toHaveLength(0);
    expect(dom.textContent).toContain('<img src=x onerror=alert(1)>');
  });

  // markdown-it's own `validateLink` rejects javascript:/data:/vbscript: before
  // the token is built, so the link syntax stays literal text. That is a stronger
  // outcome than a stripped href — there is no anchor to strip.
  it('never builds an anchor for a javascript: link, and keeps the text', () => {
    const dom = renderToDom('[bấm vào đây](javascript:alert(1))');
    expect(dom.querySelector('a')).toBeNull();
    expect(dom.textContent).toContain('bấm vào đây');
  });

  it('never builds an anchor for a data:text/html link', () => {
    const dom = renderToDom('[x](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)');
    expect(dom.querySelector('a')).toBeNull();
  });

  it('never builds an anchor for vbscript: or file:', () => {
    for (const scheme of ['vbscript:msgbox(1)', 'file:///etc/passwd']) {
      expect(renderToDom(`[x](${scheme})`).querySelector('a')).toBeNull();
    }
  });

  it('is not fooled by whitespace or case in the scheme', () => {
    for (const href of ['JaVaScRiPt:alert(1)', ' javascript:alert(1)', 'java\tscript:alert(1)']) {
      const anchor = renderToDom(`[x](${href})`).querySelector('a');
      expect(anchor?.getAttribute('href') ?? '').not.toMatch(/javascript/i);
    }
  });

  // These DO produce an anchor (the scheme is not on markdown-it's blocklist), so
  // here the hook is what has to strip the href. Different layer, same outcome.
  it('strips the href from a relative link, keeping the text', () => {
    for (const href of ['/api/projects', './local.md', 'ftp://x/y']) {
      const dom = renderToDom(`[nhãn](${href})`);
      expect(dom.querySelector('a')?.hasAttribute('href') ?? false).toBe(false);
      expect(dom.textContent).toContain('nhãn');
    }
  });

  it('keeps http, https and mailto links', () => {
    expect(renderMarkdown('[x](https://example.com/a)')).toContain('href="https://example.com/a"');
    expect(renderMarkdown('[x](http://example.com)')).toContain('href="http://example.com"');
    expect(renderMarkdown('[x](mailto:a@b.co)')).toContain('href="mailto:a@b.co"');
  });

  it('hardens every surviving link', () => {
    const html = renderMarkdown('[x](https://example.com)');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer nofollow"');
  });

  it('hardens links that linkify built from bare prose', () => {
    const html = renderMarkdown('xem https://example.com/a nhé');
    expect(html).toContain('<a href="https://example.com/a"');
    expect(html).toContain('noopener');
  });

  it('removes an image with an unsafe src entirely', () => {
    expect(renderMarkdown('![alt](javascript:alert(1))')).not.toContain('<img');
    expect(renderMarkdown('![alt](data:text/html,<script>alert(1)</script>)')).not.toContain('<img');
  });

  it('keeps an http image and a base64 image', () => {
    expect(renderMarkdown('![a](https://example.com/x.png)')).toContain('<img');
    const png = 'data:image/png;base64,iVBORw0KGgo=';
    expect(renderMarkdown(`![a](${png})`)).toContain('<img');
  });

  it('adds privacy attributes to surviving images', () => {
    const html = renderMarkdown('![a](https://example.com/x.png)');
    expect(html).toContain('loading="lazy"');
    expect(html).toContain('referrerpolicy="no-referrer"');
  });

  it('never emits a style attribute or a style element', () => {
    const dom = renderToDom('<div style="position:fixed;inset:0">x</div><style>body{}</style>');
    expect(dom.querySelector('style')).toBeNull();
    expect(attributeNames(dom)).not.toContain('style');
  });

  it('never emits an iframe, form or input element', () => {
    const dom = renderToDom(
      '<iframe src="https://evil"></iframe><form><input name=x></form>',
    );
    expect(dom.querySelector('iframe')).toBeNull();
    expect(dom.querySelector('form')).toBeNull();
    expect(dom.querySelector('input')).toBeNull();
  });

  it('never emits an svg element or its animation handlers', () => {
    const dom = renderToDom('<svg><animate onbegin=alert(1) attributeName=x></svg>');
    expect(dom.querySelector('svg')).toBeNull();
    expect(attributeNames(dom)).not.toContain('onbegin');
  });

  it('does not build a script element from a payload in a fence info string', () => {
    const dom = renderToDom('```"><script>alert(1)</script>\nx\n```');
    expect(dom.querySelector('script')).toBeNull();
  });

  // The single assertion that matters most: across the classic payload list, no
  // element in the output carries an event handler.
  it('never emits an on* attribute for any of the classic payloads', () => {
    const payloads = [
      '<a href="#" onclick="alert(1)">x</a>',
      '<body onload=alert(1)>',
      '<details open ontoggle=alert(1)>',
      '<img src=1 href=1 onerror="javascript:alert(1)"></img>',
      '<math><mi xlink:href="data:x,<script>alert(1)</script>">',
      '<x onafterscriptexecute=alert(1)><script>1</script>',
      '[x](https://example.com "onmouseover=alert(1)")',
    ];
    for (const payload of payloads) {
      const names = attributeNames(renderToDom(payload));
      expect(names.filter((n) => n.startsWith('on'))).toEqual([]);
      expect(renderToDom(payload).querySelector('script')).toBeNull();
    }
  });
});

describe('escapeHtml', () => {
  it('escapes every HTML-significant character', () => {
    expect(escapeHtml(`<a href="x">&'`)).toBe('&lt;a href=&quot;x&quot;&gt;&amp;&#39;');
  });

  it('escapes the ampersand first, so entities are not doubled wrong', () => {
    expect(escapeHtml('&lt;')).toBe('&amp;lt;');
  });

  it('leaves plain text untouched', () => {
    expect(escapeHtml('src/main.rs:42')).toBe('src/main.rs:42');
  });
});
