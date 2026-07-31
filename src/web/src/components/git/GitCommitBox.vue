<script setup lang="ts">
// Commit message box (C45).
//
// The disabled rule and the store's refusal come from the same predicate: a
// whitespace-only message never reaches the API. Duplicating the check as a
// separate condition here would let the two drift, so the button asks the same
// question the store will.
//
// `amend` is a checkbox rather than a separate button because it changes what
// Commit *means*, not what it does — and rewriting the last commit deserves to be
// something you can see is armed before you press it.

import { computed, ref } from 'vue';

const props = defineProps<{
  /** Whether anything is staged — a commit with an empty index is a 409 (C21). */
  canCommit: boolean;
  busy?: boolean;
  /** Blocks committing mid-conflict: resolving is the next step, not committing. */
  hasConflicts?: boolean;
}>();

const emit = defineEmits<{ commit: [message: string, amend: boolean] }>();

const message = ref('');
const amend = ref(false);

/** The same test the store applies (C45). */
const messageOk = computed(() => message.value.trim().length > 0);

// Amend does not need a staged change — rewording the last commit is a valid
// amend with an empty index.
const ready = computed(
  () => messageOk.value && !props.busy && !props.hasConflicts && (props.canCommit || amend.value),
);

function submit(): void {
  if (!ready.value) return;
  emit('commit', message.value, amend.value);
  message.value = '';
  amend.value = false;
}

/**
 * Cmd/Ctrl+Enter commits.
 *
 * Plain Enter inserts a newline: a commit body is multi-line, and a textarea whose
 * Enter key submits would make writing one impossible.
 */
function onKeydown(event: KeyboardEvent): void {
  if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') {
    event.preventDefault();
    submit();
  }
}
</script>

<template>
  <form class="commit" @submit.prevent="submit">
    <textarea
      v-model="message"
      class="commit__msg"
      rows="2"
      placeholder="Commit message (⌘/Ctrl+Enter để commit)"
      aria-label="Commit message"
      @keydown="onKeydown"
    />
    <div class="commit__row">
      <label class="commit__amend">
        <input v-model="amend" type="checkbox" />
        Amend
      </label>
      <span class="commit__spacer" />
      <span v-if="hasConflicts" class="commit__hint">Giải xung đột trước khi commit</span>
      <button type="submit" class="commit__btn" :disabled="!ready">Commit</button>
    </div>
  </form>
</template>

<style scoped>
.commit {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 6px;
  border-top: 1px solid #2c2c2c;
}
.commit__msg {
  width: 100%;
  padding: 4px 6px;
  border: 1px solid #2c2c2c;
  border-radius: 3px;
  background: #191919;
  color: #e4e4e4;
  font-family: inherit;
  font-size: 12px;
  resize: vertical;
}
.commit__msg:focus {
  border-color: #4a6d8c;
  outline: none;
}
.commit__row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.commit__amend {
  display: flex;
  align-items: center;
  gap: 4px;
  color: #9e9e9e;
  font-size: 11px;
  cursor: pointer;
}
.commit__spacer {
  flex: 1;
}
.commit__hint {
  color: #d3a83c;
  font-size: 11px;
}
.commit__btn {
  padding: 3px 10px;
  border: 1px solid #3a5a75;
  border-radius: 3px;
  background: #24384a;
  color: #cfe3f5;
  font-size: 12px;
  cursor: pointer;
}
.commit__btn:disabled {
  border-color: #2c2c2c;
  background: #1e1e1e;
  color: #6a6a6a;
  cursor: default;
}
</style>
