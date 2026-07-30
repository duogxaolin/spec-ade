<script setup lang="ts">
// The agent is asking permission (SPEC-004 §5.1, SPEC-003 A9/A10).
//
// Buttons come from the agent's own `options[]` and `optionId` round-trips verbatim
// — the agent decides what the choices are, and inventing or reordering them would
// answer a different question than the one asked.
//
// Not a modal overlay: the user usually needs to read the tool call (and the diff
// above it) to decide. A blocking dialog covering that context would force a guess.
// It is pinned above the composer instead, where it cannot scroll away.

import { computed } from 'vue';

import type { PermissionOptionView, ToolCallPatch } from '../../api/acp';

const props = defineProps<{
  toolCall: ToolCallPatch;
  options: PermissionOptionView[];
}>();

const emit = defineEmits<{
  (event: 'choose', optionId: string): void;
  (event: 'dismiss'): void;
}>();

const title = computed(
  () => props.toolCall.title ?? props.toolCall.kind ?? 'Agent xin quyền thực thi',
);

/**
 * Style class per option kind.
 *
 * ACP's kinds are `allow_once`, `allow_always`, `reject_once`, `reject_always`. The
 * match is on the `reject`/`allow` prefix rather than the exact string so a future
 * kind lands in a sane bucket instead of unstyled. Unknown kinds stay neutral —
 * never styled as "allow", because a mis-coloured allow button is a click the user
 * did not mean.
 */
function kindClass(kind: string): string {
  if (kind.startsWith('reject')) return 'perm__btn--reject';
  if (kind.startsWith('allow')) return 'perm__btn--allow';
  return '';
}
</script>

<template>
  <div class="perm" role="alertdialog" aria-label="Yêu cầu quyền">
    <div class="perm__text">
      <span class="perm__label">Cần quyền</span>
      <span class="perm__title">{{ title }}</span>
    </div>

    <div class="perm__actions">
      <button
        v-for="opt in options"
        :key="opt.optionId"
        class="perm__btn"
        :class="kindClass(opt.kind)"
        @click="emit('choose', opt.optionId)"
      >
        {{ opt.name }}
      </button>
      <!-- Distinct from rejecting: dismissing sends `cancelled`, which the agent
           reads as "the human is not here", not as "no". -->
      <button class="perm__btn" @click="emit('dismiss')">Bỏ qua</button>
    </div>
  </div>
</template>

<style scoped>
.perm {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-top: 1px solid #4a3c20;
  background: #2a2417;
  color: #ffd79b;
  font-size: 12px;
}
.perm__text {
  display: flex;
  flex: 1;
  min-width: 0;
  align-items: baseline;
  gap: 6px;
}
.perm__label {
  color: #d3a83c;
  font-size: 10.5px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  white-space: nowrap;
}
.perm__title {
  min-width: 0;
  overflow: hidden;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.perm__actions {
  display: flex;
  gap: 6px;
}
.perm__btn {
  padding: 4px 10px;
  border: 1px solid #4a3c20;
  border-radius: 4px;
  background: #332c1c;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.perm__btn--allow {
  border-color: #3d5c3f;
  background: #24331f;
  color: #b9e7bb;
}
.perm__btn--reject {
  border-color: #5c3b3b;
  background: #331f1f;
  color: #f0bcbc;
}
</style>
