<script setup lang="ts">
// A file change from `ToolCallContent::Diff` ([SPEC-004 INVENTED-3]).
//
// Read-only and deliberately plain: this is the "what did the agent just do to my
// file" glance, not an editor. Accepting or rejecting a hunk is SPEC-005's merge
// editor, which is also where `@codemirror/merge` earns its size.

import { computed, ref } from 'vue';

import { lineDiff } from '../../chat/diff';
import type { DiffContent } from '../../chat/toolContent';

const props = defineProps<{ diff: DiffContent }>();

/** Long diffs collapse: a 400-line rewrite would push the reply off screen. */
const COLLAPSE_OVER = 40;
const expanded = ref(false);

const result = computed(() => lineDiff(props.diff.oldText, props.diff.newText));
const isNewFile = computed(() => !props.diff.oldText);
const hidden = computed(() =>
  expanded.value ? 0 : Math.max(0, result.value.lines.length - COLLAPSE_OVER),
);
const shown = computed(() =>
  expanded.value ? result.value.lines : result.value.lines.slice(0, COLLAPSE_OVER),
);

/** Last path segment; the full path is the `title`. */
const fileName = computed(() => props.diff.path.split('/').pop() || props.diff.path);
</script>

<template>
  <div class="diff">
    <header class="diff__head">
      <span class="diff__name" :title="diff.path">{{ fileName }}</span>
      <span v-if="isNewFile" class="diff__badge">mới</span>
      <span class="diff__stat diff__stat--add">+{{ result.added }}</span>
      <span class="diff__stat diff__stat--del">−{{ result.removed }}</span>
      <!-- Say so rather than showing a misaligned diff as if it were exact. -->
      <span v-if="result.truncated" class="diff__badge" title="File quá lớn để so từng dòng">
        rút gọn
      </span>
    </header>

    <table class="diff__body">
      <tbody>
        <tr v-for="(line, i) in shown" :key="i" :class="`diff__row--${line.type}`">
          <td class="diff__gutter">{{ line.oldLine ?? '' }}</td>
          <td class="diff__gutter">{{ line.newLine ?? '' }}</td>
          <td class="diff__sign">{{ line.type === 'add' ? '+' : line.type === 'remove' ? '−' : ' ' }}</td>
          <!-- `{{ }}` escapes: file contents are the least trustworthy text here,
               and no formatting is wanted anyway. -->
          <td class="diff__code">{{ line.text }}</td>
        </tr>
      </tbody>
    </table>

    <button v-if="hidden > 0" class="diff__more" @click="expanded = true">
      Hiện {{ hidden }} dòng còn lại
    </button>
    <button v-else-if="expanded && result.lines.length > COLLAPSE_OVER" class="diff__more" @click="expanded = false">
      Thu lại
    </button>
  </div>
</template>

<style scoped>
.diff {
  overflow: hidden;
  border: 1px solid #2c2c2c;
  border-radius: 4px;
  background: #161616;
}
.diff__head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-bottom: 1px solid #2c2c2c;
  background: #1e1e1e;
  font-size: 11px;
}
.diff__name {
  font-family: ui-monospace, Menlo, Consolas, monospace;
  color: #d0d0d0;
}
.diff__badge {
  padding: 0 5px;
  border-radius: 8px;
  background: #2e2e2e;
  color: #b0b0b0;
  font-size: 10px;
}
.diff__stat--add {
  color: #6fcf74;
}
.diff__stat--del {
  color: #e57373;
}
.diff__body {
  width: 100%;
  border-collapse: collapse;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  font-size: 11.5px;
  line-height: 1.5;
}
.diff__gutter {
  width: 1%;
  padding: 0 6px;
  color: #5a5a5a;
  text-align: right;
  user-select: none;
  white-space: nowrap;
}
.diff__sign {
  width: 1%;
  padding: 0 2px;
  color: #7a7a7a;
  user-select: none;
}
.diff__code {
  padding: 0 6px;
  /* `pre-wrap` not `pre`: a long line should wrap inside the card rather than
     force the whole transcript to scroll sideways. */
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.diff__row--add {
  background: rgba(70, 149, 74, 0.16);
}
.diff__row--add .diff__code {
  color: #b9e7bb;
}
.diff__row--remove {
  background: rgba(197, 82, 82, 0.16);
}
.diff__row--remove .diff__code {
  color: #f0bcbc;
}
.diff__more {
  display: block;
  width: 100%;
  padding: 3px;
  border: 0;
  border-top: 1px solid #2c2c2c;
  background: #1e1e1e;
  color: #9fc4ff;
  cursor: pointer;
  font: inherit;
  font-size: 11px;
}
</style>
