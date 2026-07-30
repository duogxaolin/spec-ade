<script setup lang="ts">
// One markdown block: the app's only `v-html` site (SPEC-004 §5.2, §9.1).
//
// Everything unsafe is concentrated here on purpose. The rule for reviewers is a
// single grep: `v-html` appearing anywhere else in the codebase is a bug, because
// `renderMarkdown` is the only function that has been audited to produce a string
// safe to insert.
//
// Streaming is handled by rendering the text in two parts (§5.3): the portion up to
// an unterminated fence goes through markdown, and the open fence's body is shown
// as plain `<pre>` bound with `{{ }}` (escaped by Vue, no sanitizer needed). Without
// the split, markdown-it reinterprets the whole tail every time a token lands and
// the block visibly flips between paragraph and code.

import { computed, onBeforeUnmount, ref, shallowRef, watch } from 'vue';

import { renderMarkdown } from '../../chat/markdown';
import { splitStreamingTail } from '../../chat/fences';
import { createDebouncedRenderer } from '../../chat/render';
import { hasMath, renderMathIn } from '../../chat/math';
import { isMermaidFence, renderMermaid } from '../../chat/mermaid';

const props = defineProps<{
  /** Raw agent markdown. May be mid-sentence and mid-fence. */
  source: string;
  /**
   * False while chunks are still arriving.
   *
   * Drives two things: whether the final render is flushed synchronously, and
   * whether a blinking cursor is shown.
   */
  streaming?: boolean;
}>();

/** Sanitized HTML for the stable part. Never assigned from anywhere else. */
const html = ref('');
/** The still-open fence, shown verbatim while it streams. */
const tail = shallowRef<{ info: string; code: string } | null>(null);
/** Host for the rendered markdown, needed to post-process math and mermaid. */
const host = ref<HTMLElement | null>(null);

// Non-reactive: the renderer holds a timer, and making it reactive would have Vue
// track a mutable handle for no reason.
const renderer = createDebouncedRenderer((source) => {
  const split = splitStreamingTail(source);
  html.value = renderMarkdown(split.stable);
  tail.value = split.tail ? { info: split.tail.info, code: split.tail.code } : null;
  // Deferred to the next microtask so `host` holds the freshly rendered DOM.
  void Promise.resolve().then(enhance);
});

watch(
  () => props.source,
  (source) => renderer.update(source),
  { immediate: true },
);

// The turn ended: render the last chunk now instead of leaving it on the timer.
watch(
  () => props.streaming,
  (streaming, was) => {
    if (was && !streaming) renderer.flush();
  },
);

onBeforeUnmount(() => renderer.dispose());

/**
 * Apply KaTeX and mermaid to the mounted markdown.
 *
 * Both run only over closed constructs: `tail` holds anything still open, so a
 * half-written diagram never reaches mermaid's parser (§9.4).
 */
async function enhance(): Promise<void> {
  const root = host.value;
  if (!root) return;

  if (hasMath(props.source)) {
    await renderMathIn(root);
  }

  // `mermaid` is not a registered highlight.js grammar, so `highlightToHtml`
  // returns '' and markdown-it's fallback emits `<code class="language-mermaid">`
  // (verified against markdown-it 14). `class` survives the sanitizer's
  // allow-list, so this selector is exact rather than a scan of every fence.
  const fences = root.querySelectorAll<HTMLElement>('pre > code[class*="language-"]');
  for (const code of fences) {
    if (!isMermaidCode(code)) continue;
    const pre = code.closest('pre');
    if (!pre || pre.dataset['mermaidDone'] === '1') continue;

    const svg = await renderMermaid(code.textContent ?? '');
    // Mark before checking the result so a diagram that fails to parse is not
    // retried on every subsequent chunk.
    pre.dataset['mermaidDone'] = '1';
    if (!svg) continue;

    const figure = document.createElement('figure');
    figure.className = 'md__mermaid';
    // `svg` came back through DOMPurify's SVG profile inside `renderMermaid`.
    figure.innerHTML = svg;
    pre.replaceWith(figure);
  }
}

/** True when this `<code>` carries a mermaid language class. */
function isMermaidCode(code: HTMLElement): boolean {
  for (const cls of code.classList) {
    if (cls.startsWith('language-') && isMermaidFence(cls.slice('language-'.length))) return true;
  }
  return false;
}

const showCursor = computed(() => props.streaming === true);
</script>

<template>
  <div class="md">
    <!-- eslint-disable-next-line vue/no-v-html -- see the module docs: this is
         the audited boundary, and `renderMarkdown` sanitizes unconditionally. -->
    <div ref="host" class="md__body" v-html="html" />

    <!-- An unterminated fence, escaped by Vue rather than sanitized: `{{ }}`
         cannot produce markup at all, which is a stronger guarantee than a
         sanitizer and the right tool when no formatting is wanted (§5.3). -->
    <pre v-if="tail" class="md__streaming-fence"><code>{{ tail.code }}</code></pre>

    <span v-if="showCursor" class="md__cursor" aria-hidden="true" />
  </div>
</template>

<style scoped>
.md {
  min-width: 0;
}
.md__body :deep(p) {
  margin: 0 0 8px;
}
.md__body :deep(p:last-child) {
  margin-bottom: 0;
}
.md__body :deep(h1),
.md__body :deep(h2),
.md__body :deep(h3),
.md__body :deep(h4) {
  margin: 12px 0 6px;
  font-size: 1.05em;
  font-weight: 600;
}
.md__body :deep(ul),
.md__body :deep(ol) {
  margin: 0 0 8px;
  padding-left: 20px;
}
.md__body :deep(li) {
  margin: 2px 0;
}
.md__body :deep(blockquote) {
  margin: 0 0 8px;
  padding-left: 10px;
  border-left: 2px solid #3a3a3a;
  color: #b0b0b0;
}
.md__body :deep(a) {
  color: #7fb3ff;
}
.md__body :deep(code) {
  padding: 1px 4px;
  border-radius: 3px;
  background: #262626;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  font-size: 0.9em;
}
.md__body :deep(pre) {
  margin: 0 0 8px;
  padding: 8px 10px;
  overflow-x: auto;
  border: 1px solid #2c2c2c;
  border-radius: 4px;
  background: #1a1a1a;
}
.md__body :deep(pre code) {
  padding: 0;
  background: none;
}
.md__body :deep(table) {
  margin: 0 0 8px;
  border-collapse: collapse;
  font-size: 0.92em;
}
.md__body :deep(th),
.md__body :deep(td) {
  padding: 3px 8px;
  border: 1px solid #333;
  text-align: left;
}
.md__body :deep(th) {
  background: #242424;
}
.md__body :deep(img) {
  max-width: 100%;
  border-radius: 4px;
}
.md__body :deep(.md__mermaid) {
  margin: 0 0 8px;
  overflow-x: auto;
}
/* Mermaid sizes its SVG to the viewport it thinks it has; clamp it to the pane. */
.md__body :deep(.md__mermaid svg) {
  max-width: 100%;
  height: auto;
}
.md__streaming-fence {
  margin: 0 0 8px;
  padding: 8px 10px;
  overflow-x: auto;
  border: 1px solid #2c2c2c;
  border-radius: 4px;
  background: #1a1a1a;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  font-size: 12px;
}
.md__cursor {
  display: inline-block;
  width: 7px;
  height: 1em;
  margin-left: 2px;
  background: #7fb3ff;
  vertical-align: text-bottom;
  animation: md-blink 1s step-end infinite;
}
@keyframes md-blink {
  50% {
    opacity: 0;
  }
}
/* Reduced-motion users should not get a pulsing block: show it steady instead. */
@media (prefers-reduced-motion: reduce) {
  .md__cursor {
    animation: none;
  }
}
</style>
