// Mermaid diagrams from closed ```mermaid fences (SPEC-004 §5.2, §5.3).
//
// Mermaid is the heaviest thing in this app: v11 pulls ~30 d3 packages and roughly
// doubles the bundle. It is `import()`ed on first use only, so a session that never
// draws a diagram never downloads it.
//
// Security: mermaid generates SVG and injects it into the DOM itself, so it sits
// OUTSIDE `renderMarkdown`'s guarantees. Two mitigations, both required:
//
//  1. `securityLevel: 'strict'` — mermaid HTML-escapes node labels and refuses
//     `click` directives, which is the documented XSS vector in diagram source.
//  2. Its SVG output still goes through DOMPurify, with an SVG profile rather than
//     the markdown allow-list. Reusing the markdown profile would strip the
//     geometry and render an empty box; loosening the markdown profile to fit SVG
//     would widen every chat bubble's surface. Two profiles is the only honest
//     answer.
//
// Never called on an open fence: mermaid throws a parse error on every partial
// token, and the console noise hides real failures.

import DOMPurify from 'dompurify';

type MermaidApi = typeof import('mermaid').default;

let mermaidPromise: Promise<MermaidApi | null> | null = null;
let idCounter = 0;

/** Fence info strings that mean "draw this". */
export function isMermaidFence(info: string): boolean {
  return info.trim().split(/\s+/)[0]?.toLowerCase() === 'mermaid';
}

/**
 * Render diagram source to sanitized SVG markup.
 *
 * Returns `null` when mermaid is unavailable or the source does not parse — the
 * caller shows the original text, which is more useful than an error box because
 * the source of a broken diagram is still readable.
 */
export async function renderMermaid(source: string): Promise<string | null> {
  const trimmed = source.trim();
  if (!trimmed) return null;

  const mermaid = await loadMermaid();
  if (!mermaid) return null;

  // Ids must be unique per render: mermaid keys internal defs (markers, gradients)
  // off the id, and a collision makes two diagrams on one page share arrowheads.
  const id = `mermaid-${++idCounter}`;

  try {
    // `parse` first with `suppressErrors`: `render` on invalid source can leave an
    // orphan error node attached to the document body.
    const parsed = await mermaid.parse(trimmed, { suppressErrors: true });
    if (!parsed) return null;

    const { svg } = await mermaid.render(id, trimmed);
    return sanitizeSvg(svg);
  } catch (error) {
    console.warn('chat: mermaid render failed', error);
    return null;
  }
}

/**
 * Sanitize mermaid's SVG with an SVG-specific profile.
 *
 * `svgFilters` stays off — filters can reference external resources, and no
 * mermaid diagram type needs them.
 *
 * Exported for tests: this is the security boundary for diagram output, and it
 * cannot be reached through `renderMermaid` under jsdom (mermaid needs `getBBox`
 * to draw, so it never gets as far as producing SVG). Testing it directly is the
 * only way to cover it without mocking the thing under test.
 */
export function sanitizeSvg(svg: string): string | null {
  if (!DOMPurify.isSupported) return null;
  return DOMPurify.sanitize(svg, {
    USE_PROFILES: { svg: true, svgFilters: false },
    // `foreignObject` embeds arbitrary HTML inside SVG, which is a sanitizer
    // bypass classic. Mermaid uses it for `htmlLabels`, so those are disabled in
    // `initialize` below and this stays forbidden.
    FORBID_TAGS: ['foreignObject', 'script', 'style'],
    FORBID_ATTR: ['onload', 'onerror', 'onclick'],
    RETURN_DOM: false,
  });
}

/** Load and configure mermaid once. */
function loadMermaid(): Promise<MermaidApi | null> {
  mermaidPromise ??= (async () => {
    try {
      const mod = await import('mermaid');
      const mermaid = mod.default;
      mermaid.initialize({
        startOnLoad: false,
        // See the module docs: this is what makes diagram source untrusted-safe.
        securityLevel: 'strict',
        // SVG `<text>` instead of `<foreignObject>` HTML, so the sanitizer above
        // can forbid `foreignObject` without breaking labels.
        htmlLabels: false,
        flowchart: { htmlLabels: false },
        theme: 'dark',
        // The chat pane is dark and mermaid's own font stack does not match.
        fontFamily: 'inherit',
      });
      return mermaid;
    } catch (error) {
      console.warn('chat: mermaid failed to load, showing diagram source', error);
      return null;
    }
  })();
  return mermaidPromise;
}
