// @vitest-environment jsdom

import { describe, expect, it } from 'vitest';

import { hasMath, renderMathIn } from './math';

// SPEC-004 B24-B25. `renderMathIn` mutates a real DOM, so jsdom is required. The
// KaTeX import is genuine (not mocked): the point is that the dynamic import path
// works and that invalid LaTeX does not throw.

/** A container whose innerHTML is set as trusted markup, as MarkdownBlock does. */
function host(html: string): HTMLElement {
  const el = document.createElement('div');
  el.innerHTML = html;
  document.body.appendChild(el);
  return el;
}

describe('hasMath', () => {
  it('detects inline and block math', () => {
    expect(hasMath('cho $x = 1$ thì')).toBe(true);
    expect(hasMath('$$\\int_0^1 x\\,dx$$')).toBe(true);
  });

  // The gate exists to avoid a 300 KB download for a price or a shell variable.
  it('ignores a lone dollar sign', () => {
    expect(hasMath('giá $5')).toBe(false);
    expect(hasMath('echo $PATH')).toBe(false);
    expect(hasMath('')).toBe(false);
  });

  it('does not match across a newline for inline math', () => {
    expect(hasMath('$5 một cái\nvà $6 cái khác')).toBe(false);
  });
});

describe('renderMathIn', () => {
  it('renders inline math into KaTeX markup', async () => {
    const el = host('<p>cho $x^2 + y^2 = z^2$ nhé</p>');
    await renderMathIn(el);
    expect(el.querySelector('.katex')).not.toBeNull();
    expect(el.textContent).toContain('cho ');
    expect(el.textContent).toContain(' nhé');
  });

  it('renders block math in display mode', async () => {
    const el = host('<p>$$\\sum_{i=1}^{n} i$$</p>');
    await renderMathIn(el);
    expect(el.querySelector('.katex-display')).not.toBeNull();
  });

  it('leaves text with no math untouched', async () => {
    const el = host('<p>không có công thức</p>');
    const before = el.innerHTML;
    await renderMathIn(el);
    expect(el.innerHTML).toBe(before);
  });

  // The rule that protects copy-paste: $PATH in a shell block is not a formula.
  it('never touches text inside code or pre', async () => {
    const el = host('<pre><code>echo $PATH and $HOME</code></pre>');
    const before = el.innerHTML;
    await renderMathIn(el);
    expect(el.innerHTML).toBe(before);
    expect(el.querySelector('.katex')).toBeNull();
  });

  it('renders math in prose while ignoring an adjacent code block', async () => {
    const el = host('<p>với $a=1$</p><pre><code>x=$a</code></pre>');
    await renderMathIn(el);
    expect(el.querySelectorAll('.katex')).toHaveLength(1);
    expect(el.querySelector('code')!.textContent).toBe('x=$a');
  });

  it('renders several formulas in one text node', async () => {
    const el = host('<p>$a$ và $b$ và $c$</p>');
    await renderMathIn(el);
    expect(el.querySelectorAll('.katex').length).toBeGreaterThanOrEqual(3);
  });

  it('keeps the surrounding text in the right order', async () => {
    const el = host('<p>trước $x$ giữa $y$ sau</p>');
    await renderMathIn(el);
    const text = el.textContent ?? '';
    expect(text.indexOf('trước')).toBeLessThan(text.indexOf('giữa'));
    expect(text.indexOf('giữa')).toBeLessThan(text.indexOf('sau'));
  });

  // LLMs emit broken LaTeX routinely; KaTeX's default is to throw, which would
  // blank the whole transcript. throwOnError: false turns it into red source text.
  it('does not throw on invalid LaTeX', async () => {
    const el = host('<p>$\\frac{1}{$</p>');
    await expect(renderMathIn(el)).resolves.toBeUndefined();
  });

  it('renders invalid LaTeX as visible source rather than dropping it', async () => {
    const el = host('<p>$\\nosuchcommand{x}$</p>');
    await renderMathIn(el);
    expect(el.textContent).toContain('nosuchcommand');
  });

  // The debounced renderer can re-run over a subtree it already processed.
  it('is idempotent', async () => {
    const el = host('<p>cho $x = 1$</p>');
    await renderMathIn(el);
    const once = el.innerHTML;
    await renderMathIn(el);
    expect(el.innerHTML).toBe(once);
  });

  it('handles an empty container', async () => {
    await expect(renderMathIn(host(''))).resolves.toBeUndefined();
  });

  it('does not turn a lone dollar amount into math', async () => {
    const el = host('<p>giá $5 cho một cái</p>');
    await renderMathIn(el);
    expect(el.querySelector('.katex')).toBeNull();
  });
});
