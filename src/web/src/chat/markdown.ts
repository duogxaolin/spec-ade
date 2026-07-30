// Agent markdown → safe HTML (SPEC-004 §5.2).
//
// This is the ONLY function in the app allowed to produce a string for `v-html`.
// Everything it returns lands in the DOM without further checks, so every line
// here is a security boundary, not a formatting preference.
//
// Three layers, and all three stay even though any one of them looks sufficient:
//
//  1. `html: false` — markdown-it escapes raw HTML at the parser, so
//     `<script>alert(1)</script>` becomes text before anything else runs.
//  2. DOMPurify — `linkify` builds `<a href>` out of agent text, and the
//     highlighter injects markup as a string. Both are agent-controlled input to
//     an HTML string, which is exactly what a sanitizer is for. It also means a
//     future edit flipping `html: true` degrades output instead of becoming RCE.
//  3. A scheme allow-list — DOMPurify blocks `javascript:` by default but not
//     every `data:` shape, and `data:text/html` is script execution
//     ([SPEC-004 INVENTED-4]).
//
// Deliberately NOT allowed: `style` (CSS injection can exfiltrate via
// `background: url()` and can cover the page), any `on*` handler, `<iframe>`,
// `<form>`, `<input>`. None of them are reachable from CommonMark output, so
// allowing them would only widen the surface for no rendering benefit.

import MarkdownIt from 'markdown-it';
import DOMPurify from 'dompurify';

import { highlightToHtml } from './highlight';

/** Tags CommonMark + GFM tables can produce, plus `span` for the highlighter. */
const ALLOWED_TAGS = [
  'p', 'br', 'hr',
  'h1', 'h2', 'h3', 'h4', 'h5', 'h6',
  'strong', 'em', 's', 'del', 'ins', 'sub', 'sup',
  'blockquote', 'pre', 'code', 'span',
  'ul', 'ol', 'li',
  'table', 'thead', 'tbody', 'tr', 'th', 'td',
  'a', 'img',
];

/**
 * Attributes kept after sanitizing.
 *
 * `class` is needed for highlight.js tokens. It is safe here only because there
 * is no CSS in this app that turns a class name into script; it is still a
 * lesser risk than `style`, which is why that one stays out.
 */
const ALLOWED_ATTR = ['href', 'title', 'alt', 'src', 'class', 'colspan', 'rowspan', 'align'];

/** The only schemes a link may carry ([SPEC-004 INVENTED-4]). */
const SAFE_LINK_SCHEMES = new Set(['http:', 'https:', 'mailto:']);

/** Image sources: `data:` only for real image types, never `data:text/html`. */
const SAFE_IMAGE_DATA = /^data:image\/(png|jpeg|gif|webp|avif|bmp);base64,[a-z0-9+/=\s]+$/i;

const md = new MarkdownIt({
  // The single most valuable setting in this file — see the module docs.
  html: false,
  // Turns bare URLs in prose into links. Convenient, and the reason layer 3 is
  // not optional: the href comes straight from agent output.
  linkify: true,
  // Off on purpose: a lone newline inside a paragraph is not a line break in
  // CommonMark, and agents rely on that when they wrap prose at 80 columns.
  breaks: false,
  typographer: false,
  highlight: highlightToHtml,
});

let hooksInstalled = false;

/**
 * Install the scheme allow-list and link hardening.
 *
 * Done as a hook rather than by post-processing the HTML string: parsing HTML
 * with regexes to find attributes is how sanitizers get bypassed. The hook runs
 * on the parsed node, which is the only reliable place to inspect a URL.
 *
 * Lazy because DOMPurify needs a DOM. Importing this module under a `node`
 * test environment would otherwise throw at import time
 * (`DOMPurify.isSupported === false`, and `addHook` is not even defined).
 */
function installHooks(): void {
  if (hooksInstalled || typeof DOMPurify.addHook !== 'function') return;
  hooksInstalled = true;

  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (!(node instanceof Element)) return;

    if (node.tagName === 'A') {
      const href = node.getAttribute('href');
      if (href !== null && !isSafeLink(href)) {
        // Strip the href but keep the text: dropping the whole element would
        // silently delete words the agent wrote.
        node.removeAttribute('href');
      }
      if (node.hasAttribute('href')) {
        // `noopener` matters even on localhost: the opened page gets
        // `window.opener` and can navigate this tab away otherwise.
        node.setAttribute('target', '_blank');
        node.setAttribute('rel', 'noopener noreferrer nofollow');
      }
    }

    if (node.tagName === 'IMG') {
      const src = node.getAttribute('src');
      if (src === null || !isSafeImageSrc(src)) {
        // An image with no source renders as broken alt text, which is worse
        // than nothing — remove the element entirely.
        node.remove();
        return;
      }
      // Remote images in agent output are a tracking channel (a unique URL per
      // render reports that the message was read). `loading="lazy"` does not
      // prevent that, so this only limits the layout cost.
      node.setAttribute('loading', 'lazy');
      node.setAttribute('referrerpolicy', 'no-referrer');
    }
  });
}

/** True if `href` parses to an allow-listed scheme. */
function isSafeLink(href: string): boolean {
  // Relative links are dropped: there is no meaningful base for a path an agent
  // invented, and `/api/...` would be a same-origin request the user did not ask
  // for ([SPEC-004 INVENTED-4]).
  let url: URL;
  try {
    url = new URL(href, 'https://spec-ade.invalid/');
  } catch {
    return false;
  }
  if (!SAFE_LINK_SCHEMES.has(url.protocol)) return false;
  // `new URL('/x', base)` succeeds with an https protocol, so an absolute-path
  // link would pass the scheme test. Require the original text to be absolute.
  return /^(https?:|mailto:)/i.test(href.trim());
}

function isSafeImageSrc(src: string): boolean {
  const value = src.trim();
  if (SAFE_IMAGE_DATA.test(value)) return true;
  return /^https?:\/\//i.test(value);
}

/**
 * Render agent markdown to sanitized HTML.
 *
 * Pure and synchronous: callers debounce it (§5.3) and tests can assert on the
 * exact string. KaTeX and mermaid are applied afterwards, on the mounted DOM,
 * because both need to measure or draw.
 */
export function renderMarkdown(source: string): string {
  if (!source) return '';
  installHooks();

  const rendered = md.render(source);

  // No DOM (a plain `node` test environment, or SSR): returning the unsanitized
  // string would be a silent XSS hole, and returning it escaped would be a
  // silent formatting bug. Fail loudly instead — this can only happen through a
  // configuration mistake.
  if (!DOMPurify.isSupported) {
    throw new Error(
      'renderMarkdown needs a DOM: DOMPurify is unsupported in this environment. ' +
        'Run this code under jsdom or a browser (SPEC-004 §7.1).',
    );
  }

  return DOMPurify.sanitize(rendered, {
    ALLOWED_TAGS,
    ALLOWED_ATTR,
    // Belt to layer 1's braces: even if raw HTML reached the sanitizer, these
    // never survive.
    FORBID_TAGS: ['style', 'script', 'iframe', 'object', 'embed', 'form', 'input'],
    FORBID_ATTR: ['style', 'srcset'],
    // `<template>`, `<slot>` etc. are not markdown output and DOMPurify's shadow
    // DOM handling is extra surface for no gain here.
    USE_PROFILES: { html: true },
    // Drop the contents of a removed tag rather than leaving orphan text that
    // would read as part of the reply.
    KEEP_CONTENT: true,
    RETURN_DOM: false,
    RETURN_DOM_FRAGMENT: false,
  });
}

/**
 * Render text with no markdown at all, HTML-escaped.
 *
 * For content whose type says it is plain (`resource_link` labels, error text
 * from a failed tool call). Going through `renderMarkdown` would let a `_` in a
 * filename turn into italics.
 */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}
