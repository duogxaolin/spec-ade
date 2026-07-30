// @vitest-environment jsdom

import { describe, expect, it, vi } from 'vitest';

import { isMermaidFence, renderMermaid, sanitizeSvg } from './mermaid';

// SPEC-004 B26-B27.
//
// Split deliberately. `isMermaidFence` is pure and fully covered. `renderMermaid`
// calls the real mermaid, which measures text with `getBBox`/`getComputedTextLength`
// — APIs jsdom does not implement. Rather than mock mermaid (which would test the
// mock, not the code) the contract asserted here is the one that actually matters
// for the UI: it resolves to either sanitized SVG or null, and NEVER throws or
// leaves an orphan error node in the document. That holds whether or not jsdom can
// draw. Visual correctness of the SVG is a real-browser check (§8.2).

describe('isMermaidFence', () => {
  it('accepts a plain mermaid fence', () => {
    expect(isMermaidFence('mermaid')).toBe(true);
  });

  it('is case-insensitive and tolerates whitespace', () => {
    expect(isMermaidFence('  Mermaid ')).toBe(true);
    expect(isMermaidFence('MERMAID')).toBe(true);
  });

  it('accepts a mermaid fence with extra info tokens', () => {
    expect(isMermaidFence('mermaid theme=dark')).toBe(true);
  });

  it('rejects every other language', () => {
    for (const info of ['rust', 'ts', '', '   ', 'mermaidjs', 'not-mermaid']) {
      expect(isMermaidFence(info)).toBe(false);
    }
  });
});

describe('renderMermaid', () => {
  it('returns null for empty source without loading mermaid', async () => {
    await expect(renderMermaid('')).resolves.toBeNull();
    await expect(renderMermaid('   \n  ')).resolves.toBeNull();
  });

  it('returns null for source that cannot parse', async () => {
    // `parse({suppressErrors: true})` returns falsy rather than throwing.
    await expect(renderMermaid('this is not a diagram at all')).resolves.toBeNull();
  });

  // The contract the UI depends on: a diagram never breaks the transcript. The
  // console.warn spy is not an assertion, it keeps jsdom's expected getBBox
  // failures out of the test output.
  it('resolves to a string or null for every source, and never rejects', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    try {
      const sources = [
        'graph TD; A-->B;',
        'sequenceDiagram\n  A->>B: hi',
        'graph TD; A[<script>alert(1)</script>]-->B;',
        'flowchart LR\n  a --> b',
        '```',
        'graph',
        'graph TD; A-->',
      ];
      for (const source of sources) {
        const result = await renderMermaid(source);
        expect(result === null || typeof result === 'string').toBe(true);
      }
    } finally {
      warn.mockRestore();
    }
  });

  it('leaves no error node attached to the document body', async () => {
    // `render` on invalid source can orphan a node; `parse` first is what prevents
    // it. If that ordering regresses, the body grows an unstyled error block.
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const before = document.body.childNodes.length;
    await renderMermaid('graph TD; A-->');
    warn.mockRestore();
    expect(document.body.childNodes.length).toBe(before);
  });
});

// B27. Tested directly rather than through renderMermaid — see the export comment
// in mermaid.ts. These are the assertions that make the SVG path safe.
describe('sanitizeSvg', () => {
  /** Parse sanitized markup and report every attribute name in it. */
  function attributeNames(svg: string): string[] {
    const host = document.createElement('div');
    host.innerHTML = svg;
    const names: string[] = [];
    for (const el of host.querySelectorAll('*')) {
      for (const attr of el.attributes) names.push(attr.name.toLowerCase());
    }
    return names;
  }

  it('keeps the geometry a diagram needs', () => {
    const svg = sanitizeSvg(
      '<svg viewBox="0 0 10 10"><g><path d="M0 0L5 5"/><rect x="1" y="2" width="3" height="4"/><text x="1" y="2">nhãn</text></g></svg>',
    );
    expect(svg).toContain('<svg');
    expect(svg).toContain('<path');
    expect(svg).toContain('viewBox');
    expect(svg).toContain('nhãn');
  });

  it('strips a script element inside the svg', () => {
    const svg = sanitizeSvg('<svg><script>alert(1)</script><rect/></svg>')!;
    expect(svg).not.toContain('<script');
    expect(svg).toContain('<rect');
  });

  // foreignObject embeds arbitrary HTML inside SVG — the classic sanitizer bypass.
  // It is why htmlLabels is disabled in initialize().
  it('strips foreignObject', () => {
    const svg = sanitizeSvg('<svg><foreignObject><div>x</div></foreignObject></svg>')!;
    expect(svg.toLowerCase()).not.toContain('foreignobject');
  });

  it('strips event handlers', () => {
    const svg = sanitizeSvg('<svg><rect onclick="alert(1)" onload="alert(2)"/></svg>')!;
    expect(attributeNames(svg).filter((n) => n.startsWith('on'))).toEqual([]);
  });

  it('strips an animate/onbegin payload', () => {
    const svg = sanitizeSvg('<svg><animate onbegin="alert(1)" attributeName="x"/></svg>')!;
    expect(attributeNames(svg)).not.toContain('onbegin');
  });

  it('strips a style element', () => {
    const svg = sanitizeSvg('<svg><style>@import url(//evil)</style><rect/></svg>')!;
    expect(svg).not.toContain('<style');
  });

  it('strips a javascript: href on an anchor inside the svg', () => {
    const svg = sanitizeSvg('<svg><a href="javascript:alert(1)"><rect/></a></svg>')!;
    expect(svg.toLowerCase()).not.toContain('javascript:');
  });

  it('returns a string for empty input rather than throwing', () => {
    expect(sanitizeSvg('')).toBe('');
  });
});
