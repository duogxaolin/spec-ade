<script setup lang="ts">
// Blame gutter (C14, §5.7).
//
// A plain table rather than a CodeMirror gutter: attaching blame to the editor's
// own gutter is SPEC-008's job (§10 puts it out of scope here), and a table is
// what makes "which commit wrote this line" readable without opening the file in
// the editor at all.
//
// Consecutive lines from the same commit show the commit label once. That is not
// decoration — an unbroken run of one colour is how you see that a whole block
// arrived in a single change, which is the question blame is usually asked.

import { computed } from 'vue';

import type { GitBlame } from '../../api/git';
import { absoluteTime, relativeTime } from '../../git/relativeTime';

const props = defineProps<{
  blame: GitBlame | null;
  /** The file's text, so line content can sit next to its attribution. */
  content?: string;
}>();

const emit = defineEmits<{
  /** Open the commit that wrote a line. */
  commit: [oid: string];
  close: [];
}>();

interface BlameRow {
  line: number;
  oid: string;
  short: string;
  author: string;
  time: number;
  summary: string;
  /** First line of a run from this commit — only these show the label. */
  first: boolean;
  text: string;
}

const rows = computed<BlameRow[]>(() => {
  const blame = props.blame;
  if (!blame) return [];
  // Splitting on '\n' leaves a trailing empty element for a file ending in a
  // newline (which is most files); blame has no line for it, so indexing by
  // `line - 1` is safe and the extra element is simply never reached.
  const lines = props.content?.split('\n') ?? [];

  let previous: string | null = null;
  return blame.lines.map((entry) => {
    const first = entry.oid !== previous;
    previous = entry.oid;
    return {
      line: entry.line,
      oid: entry.oid,
      short: entry.short,
      author: entry.author,
      time: entry.time,
      summary: entry.summary,
      first,
      text: lines[entry.line - 1] ?? '',
    };
  });
});

/** Stable colour per commit, so runs are distinguishable at a glance. */
function tint(oid: string): string {
  // The first hex digits of the oid are already uniformly distributed, so they
  // make a good hue without hashing.
  const hue = (parseInt(oid.slice(0, 4), 16) || 0) % 360;
  return `hsl(${hue} 24% 22%)`;
}
</script>

<template>
  <section v-if="blame" class="blame">
    <header class="blame__head">
      <span class="blame__path" :title="blame.path">{{ blame.path }}</span>
      <span class="blame__count">{{ blame.lines.length }} dòng</span>
      <span class="blame__spacer" />
      <button type="button" class="blame__btn" aria-label="Đóng blame" @click="emit('close')">
        ✕
      </button>
    </header>

    <div class="blame__body">
      <table class="blame__table">
        <tbody>
          <tr v-for="row in rows" :key="row.line" class="blame__row">
            <td
              class="blame__meta"
              :style="{ background: tint(row.oid) }"
              :title="`${row.summary}\n${row.author} · ${absoluteTime(row.time)}`"
            >
              <template v-if="row.first">
                <button type="button" class="blame__oid" @click="emit('commit', row.oid)">
                  {{ row.short }}
                </button>
                <span class="blame__author">{{ row.author }}</span>
                <span class="blame__when">{{ relativeTime(row.time) }}</span>
              </template>
            </td>
            <td class="blame__lineno">{{ row.line }}</td>
            <td class="blame__text"><pre>{{ row.text }}</pre></td>
          </tr>
        </tbody>
      </table>
    </div>
  </section>
</template>

<style scoped>
.blame {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-top: 1px solid #2c2c2c;
  background: #161616;
}
.blame__head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-bottom: 1px solid #2c2c2c;
  color: #9e9e9e;
  font-size: 11px;
}
.blame__path {
  overflow: hidden;
  color: #d4d4d4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.blame__count {
  color: #6f6f6f;
}
.blame__spacer {
  flex: 1;
}
.blame__btn {
  padding: 2px 6px;
  border: 1px solid #333;
  border-radius: 3px;
  background: #1f1f1f;
  color: #c4c4c4;
  font-size: 11px;
  cursor: pointer;
}
.blame__body {
  min-height: 0;
  overflow: auto;
}
.blame__table {
  width: 100%;
  border-collapse: collapse;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}
.blame__meta {
  display: flex;
  gap: 6px;
  width: 240px;
  max-width: 240px;
  padding: 0 6px;
  overflow: hidden;
  color: #b0b0b0;
  white-space: nowrap;
}
.blame__oid {
  padding: 0;
  border: 0;
  background: none;
  color: #9ecbff;
  font: inherit;
  cursor: pointer;
}
.blame__oid:hover {
  text-decoration: underline;
}
.blame__author {
  overflow: hidden;
  text-overflow: ellipsis;
}
.blame__when {
  margin-left: auto;
  color: #7a7a7a;
}
.blame__lineno {
  width: 44px;
  padding: 0 6px;
  color: #6f6f6f;
  text-align: right;
  user-select: none;
}
.blame__text pre {
  margin: 0;
  color: #d4d4d4;
  white-space: pre;
}
</style>
