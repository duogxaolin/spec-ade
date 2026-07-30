// KaTeX rendering over already-sanitized HTML (SPEC-004 §5.4).
//
// No `markdown-it-katex`: it is unmaintained (last release 2017, written against
// markdown-it 8) and was never installed. Post-processing the mounted DOM instead
// costs one tree walk and keeps math out of the sanitizer's path entirely — KaTeX
// output goes in as DOM nodes we built, not as an HTML string that has to be
// re-trusted.
//
// KaTeX is dynamically imported: the library plus its font CSS is ~300 KB, and a
// conversation with no math should not pay for it.

/** Matches `$$...$$` first — otherwise `$$x$$` would parse as an empty `$..$`. */
const BLOCK_MATH = /\$\$([^$]+?)\$\$/g;
const INLINE_MATH = /\$([^$\n]+?)\$/g;

/**
 * Only what this module calls.
 *
 * Deliberately narrower than `typeof import('katex')`: the published types declare
 * a self-referential `katex` property that the ESM default export does not carry,
 * so the full type cannot describe what `import('katex')` actually hands back. The
 * options type is KaTeX's own, so the call site stays checked against the real API.
 */
interface KatexApi {
  renderToString(tex: string, options?: import('katex').KatexOptions): string;
}

let katexModule: KatexApi | null = null;

/** True if `text` plausibly contains math — cheap gate before the dynamic import. */
export function hasMath(text: string): boolean {
  // Two `$` on one line, or a `$$` pair anywhere. A lone `$` (a price, a shell
  // prompt) must not trigger a 300 KB download.
  return /\$\$[^$]+\$\$/.test(text) || /\$[^$\n]+\$/.test(text);
}

/**
 * Replace math spans inside `root` with rendered KaTeX.
 *
 * Walks text nodes only, and skips anything inside `<code>`/`<pre>`: `$PATH` and
 * `$1` are shell syntax, and turning them into formulas would corrupt code the
 * user might copy ([SPEC-004 §9.7]).
 *
 * Idempotent by construction — KaTeX output contains no bare `$`, so a second call
 * finds nothing to do. That matters because the debounced renderer may re-run over
 * a partially processed subtree.
 */
export async function renderMathIn(root: HTMLElement): Promise<void> {
  const targets = collectTextNodes(root);
  if (targets.length === 0) return;

  const katex = await loadKatex();
  if (!katex) return;

  for (const node of targets) {
    const replacement = renderNode(katex, node.textContent ?? '');
    if (replacement) node.parentNode?.replaceChild(replacement, node);
  }
}

/** Text nodes outside code that contain at least one `$` pair. */
function collectTextNodes(root: HTMLElement): Text[] {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent) return NodeFilter.FILTER_REJECT;
      if (parent.closest('code, pre, .katex')) return NodeFilter.FILTER_REJECT;
      return hasMath(node.textContent ?? '') ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_REJECT;
    },
  });

  const found: Text[] = [];
  // Collected before mutating: replacing nodes mid-walk invalidates the walker.
  for (let n = walker.nextNode(); n !== null; n = walker.nextNode()) found.push(n as Text);
  return found;
}

/**
 * Build a fragment for one text node, or `null` if nothing matched.
 *
 * `throwOnError: false` is not optional. LLMs emit invalid LaTeX routinely, and
 * KaTeX's default is to throw — one bad formula would blank the whole transcript.
 * With it off, KaTeX renders the source in red, which tells the user what happened.
 */
function renderNode(katex: KatexApi, text: string): DocumentFragment | null {
  const fragment = document.createDocumentFragment();
  let cursor = 0;
  let matched = false;

  // Block pass first, so `$$` pairs are consumed before inline sees them.
  for (const match of matchAll(text)) {
    matched = true;
    if (match.index > cursor) {
      fragment.appendChild(document.createTextNode(text.slice(cursor, match.index)));
    }
    const span = document.createElement('span');
    // `renderToString` output is KaTeX's own markup, inserted into an element we
    // created and never handed back to the markdown sanitizer.
    span.innerHTML = katex.renderToString(match.source, {
      displayMode: match.display,
      throwOnError: false,
      output: 'html',
    });
    fragment.appendChild(span);
    cursor = match.index + match.length;
  }

  if (!matched) return null;
  if (cursor < text.length) fragment.appendChild(document.createTextNode(text.slice(cursor)));
  return fragment;
}

interface MathMatch {
  index: number;
  length: number;
  source: string;
  display: boolean;
}

/** Block matches, then inline matches in the gaps between them. */
function matchAll(text: string): MathMatch[] {
  const blocks: MathMatch[] = [];
  for (const m of text.matchAll(BLOCK_MATH)) {
    blocks.push({ index: m.index, length: m[0].length, source: m[1]!.trim(), display: true });
  }

  const inline: MathMatch[] = [];
  for (const m of text.matchAll(INLINE_MATH)) {
    const start = m.index;
    const end = start + m[0].length;
    const overlaps = blocks.some((b) => start < b.index + b.length && end > b.index);
    if (!overlaps) {
      inline.push({ index: start, length: m[0].length, source: m[1]!.trim(), display: false });
    }
  }

  return [...blocks, ...inline].sort((a, b) => a.index - b.index);
}

/** Load KaTeX once. A failure is non-fatal: the text stays as written. */
async function loadKatex(): Promise<KatexApi | null> {
  if (katexModule) return katexModule;
  try {
    const mod = await import('katex');
    await import('katex/dist/katex.min.css');
    katexModule = mod.default ?? mod;
    return katexModule;
  } catch (error) {
    console.warn('chat: KaTeX failed to load, leaving math as text', error);
    return null;
  }
}
