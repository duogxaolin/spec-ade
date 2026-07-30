<script setup lang="ts">
// One tool call: header always visible, payload behind a disclosure (SPEC-004 §5.1).
//
// The header answers "what is the agent doing and did it work"; the body answers
// "show me". Collapsed by default because a completed read of a 500-line file is
// noise once it has succeeded — the interesting cases are failures and diffs, and
// those are what the header's status makes findable.

import { computed, ref } from 'vue';

import type { ToolCallPayload } from '../../api/acp';
import {
  kindIcon,
  parseToolContents,
  parseToolLocations,
  statusGlyph,
  statusLabel,
  toolStatus,
} from '../../chat/toolContent';
import MarkdownBlock from './MarkdownBlock.vue';
import ToolDiff from './ToolDiff.vue';

const props = defineProps<{
  call: ToolCallPayload;
  /** Start open. A group of one opens; a group of several starts closed. */
  defaultOpen?: boolean;
}>();

const emit = defineEmits<{
  /** A location was clicked — the pane decides what opening a file means. */
  (event: 'open-location', payload: { path: string; line: number | null }): void;
}>();

const open = ref(props.defaultOpen ?? false);

const contents = computed(() => parseToolContents(props.call.content));
const locations = computed(() => parseToolLocations(props.call.locations));
const status = computed(() => toolStatus(props.call.status));
/** Failures open themselves: an error the user has to click to find is hidden. */
const forceOpen = computed(() => status.value.known === 'failed');
const isOpen = computed(() => open.value || forceOpen.value);

const title = computed(
  () => props.call.title ?? props.call.kind ?? props.call.toolCallId,
);
const hasBody = computed(() => contents.value.length > 0 || locations.value.length > 0);

/** `rawOutput` is free-form JSON; shown only when there is nothing better. */
const rawOutput = computed(() => {
  if (contents.value.length > 0 || props.call.rawOutput === undefined) return null;
  try {
    return JSON.stringify(props.call.rawOutput, null, 2);
  } catch {
    // Cyclic or otherwise unserializable: not worth a crash over a debug view.
    return null;
  }
});

function textOf(block: { type: string }): string {
  return 'text' in block && typeof block.text === 'string' ? block.text : '';
}

function imageSrc(block: { type: string }): string | null {
  // [SPEC-004 INVENTED-6]: built as an attribute binding, never through markdown.
  if (!('data' in block) || typeof block.data !== 'string') return null;
  const mime = 'mimeType' in block && typeof block.mimeType === 'string' ? block.mimeType : '';
  // Only real image types — `data:text/html` in an `<img src>` is inert in every
  // current browser, but the allow-list costs nothing and removes the question.
  if (!/^image\/(png|jpeg|gif|webp|avif|bmp)$/.test(mime)) return null;
  return `data:${mime};base64,${block.data}`;
}

function linkOf(block: { type: string }): { uri: string; label: string } | null {
  if (!('uri' in block) || typeof block.uri !== 'string') return null;
  const label =
    ('title' in block && typeof block.title === 'string' && block.title) ||
    ('name' in block && typeof block.name === 'string' && block.name) ||
    block.uri;
  return { uri: block.uri, label };
}
</script>

<template>
  <article class="tc" :class="`tc--${status.known ?? 'other'}`">
    <button
      class="tc__head"
      :disabled="!hasBody && !rawOutput"
      :aria-expanded="isOpen"
      @click="open = !open"
    >
      <span class="tc__glyph" aria-hidden="true">{{ statusGlyph(call.status) }}</span>
      <span class="tc__icon" aria-hidden="true">{{ kindIcon(call.kind) }}</span>
      <span class="tc__title">{{ title }}</span>
      <span class="tc__status">{{ statusLabel(call.status) }}</span>
      <span v-if="hasBody || rawOutput" class="tc__chevron" aria-hidden="true">
        {{ isOpen ? '▾' : '▸' }}
      </span>
    </button>

    <div v-if="isOpen" class="tc__body">
      <!-- Locations first: they are the "jump to what changed" affordance and
           belong above a possibly long payload. -->
      <div v-if="locations.length" class="tc__locations">
        <button
          v-for="(loc, i) in locations"
          :key="i"
          class="tc__loc"
          :title="loc.path"
          @click="emit('open-location', { path: loc.path, line: loc.line ?? null })"
        >
          {{ loc.path.split('/').pop() }}<span v-if="loc.line">:{{ loc.line }}</span>
        </button>
      </div>

      <template v-for="(item, i) in contents" :key="i">
        <ToolDiff v-if="item.type === 'diff'" :diff="item" />

        <!-- A terminal is referenced by id; wiring it to a live PtyManager view
             needs `terminal/*` reverse calls, which SPEC-003 deliberately did not
             advertise. Naming it beats rendering nothing. -->
        <p v-else-if="item.type === 'terminal'" class="tc__note">
          terminal {{ item.terminalId }} (xem ở tab Terminal)
        </p>

        <template v-else-if="item.type === 'content'">
          <MarkdownBlock
            v-if="item.content.type === 'text'"
            :source="textOf(item.content)"
          />
          <img
            v-else-if="item.content.type === 'image' && imageSrc(item.content)"
            class="tc__image"
            :src="imageSrc(item.content)!"
            alt="Ảnh từ tool call"
            loading="lazy"
          />
          <a
            v-else-if="item.content.type === 'resource_link' && linkOf(item.content)"
            class="tc__link"
            :href="linkOf(item.content)!.uri"
            target="_blank"
            rel="noopener noreferrer nofollow"
          >{{ linkOf(item.content)!.label }}</a>
          <p v-else class="tc__note">nội dung chưa hỗ trợ: {{ item.content.type }}</p>
        </template>

        <p v-else class="tc__note">{{ item.label }}</p>
      </template>

      <pre v-if="rawOutput" class="tc__raw"><code>{{ rawOutput }}</code></pre>
    </div>
  </article>
</template>

<style scoped>
.tc {
  border: 1px solid #2c2c2c;
  border-left: 2px solid #4c7ecf;
  border-radius: 4px;
  background: #1b1b1b;
}
.tc--completed {
  border-left-color: #4c9350;
}
.tc--failed {
  border-left-color: #c55252;
}
.tc--in_progress {
  border-left-color: #d3a83c;
}
.tc__head {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  padding: 5px 8px;
  border: 0;
  background: none;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
  text-align: left;
}
.tc__head:disabled {
  cursor: default;
}
.tc__glyph {
  color: #9e9e9e;
}
.tc--completed .tc__glyph {
  color: #6fcf74;
}
.tc--failed .tc__glyph {
  color: #e57373;
}
.tc__title {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  color: #d8d8d8;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tc__status,
.tc__chevron {
  color: #8a8a8a;
  font-size: 11px;
}
.tc__body {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 0 8px 8px;
}
.tc__locations {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}
.tc__loc {
  padding: 1px 6px;
  border: 1px solid #33445e;
  border-radius: 8px;
  background: #1d2735;
  color: #9fc4ff;
  cursor: pointer;
  font: inherit;
  font-size: 11px;
}
.tc__note {
  margin: 0;
  color: #9e9e9e;
  font-size: 11.5px;
}
.tc__link {
  color: #7fb3ff;
  font-size: 12px;
  overflow-wrap: anywhere;
}
.tc__image {
  max-width: 100%;
  border-radius: 4px;
}
.tc__raw {
  margin: 0;
  padding: 6px 8px;
  max-height: 240px;
  overflow: auto;
  border-radius: 4px;
  background: #141414;
  color: #b6b6b6;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  font-size: 11px;
}
</style>
