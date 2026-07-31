<script setup lang="ts">
// The search pane: query box, toggles, glob filter, results (SPEC-006 §5.9).
//
// The store owns the debounce (D42) and the stream lifecycle (D41), so every
// input here calls straight through — there is no local timer to keep in sync.
//
// Enter bypasses the debounce: having pressed Enter, waiting another 200 ms for a
// timer to expire is a delay with no purpose.

import { computed, onBeforeUnmount, ref, watch } from 'vue';

import { parseGlobs } from '../../api/search';
import { useSearchStore } from '../../stores/search';
import SearchResults from './SearchResults.vue';

const props = defineProps<{ projectId: string | null }>();
const emit = defineEmits<{ open: [path: string, line: number] }>();

const store = useSearchStore();

/** The raw glob text; parsed into the store's array only on change. */
const globText = ref('');

const summary = computed(() => {
  if (store.running) return `Đang tìm… ${store.filesScanned} tệp`;
  if (store.elapsedMs === null) return '';
  return `${store.matchCount} kết quả · ${store.fileCount} tệp · ${store.elapsedMs} ms`;
});

function onQuery(event: Event): void {
  const value = (event.target as HTMLInputElement).value;
  if (props.projectId) store.search(props.projectId, value);
  else store.query = value;
}

function onEnter(): void {
  if (props.projectId) store.run(props.projectId);
}

function toggle(key: 'regex' | 'case' | 'word'): void {
  if (!props.projectId) return;
  store.setOptions(props.projectId, { [key]: !store.options[key] });
}

function onGlobs(): void {
  if (!props.projectId) return;
  store.setOptions(props.projectId, { globs: parseGlobs(globText.value) });
}

// Switching projects must not leave the previous project's results on screen
// under the new project's name.
watch(
  () => props.projectId,
  () => {
    store.reset();
    globText.value = '';
  },
);

// A pane that unmounts with a walk still running would leave the server scanning
// for a result list nobody will read.
onBeforeUnmount(() => store.stop());
</script>

<template>
  <div class="search">
    <div class="search__bar">
      <input
        class="search__input"
        type="search"
        placeholder="Tìm trong dự án…"
        :value="store.query"
        :disabled="!projectId"
        @input="onQuery"
        @keydown.enter.prevent="onEnter"
      />
      <div class="search__toggles">
        <button
          type="button"
          class="search__toggle"
          :class="{ 'search__toggle--on': store.options.case }"
          title="Phân biệt hoa thường"
          :disabled="!projectId"
          @click="toggle('case')"
        >
          Aa
        </button>
        <button
          type="button"
          class="search__toggle"
          :class="{ 'search__toggle--on': store.options.word }"
          title="Khớp trọn từ"
          :disabled="!projectId"
          @click="toggle('word')"
        >
          ab
        </button>
        <button
          type="button"
          class="search__toggle"
          :class="{ 'search__toggle--on': store.options.regex }"
          title="Biểu thức chính quy"
          :disabled="!projectId"
          @click="toggle('regex')"
        >
          .*
        </button>
      </div>
    </div>

    <input
      v-model="globText"
      class="search__glob"
      type="text"
      placeholder="glob: *.rs, !target/**"
      :disabled="!projectId"
      @change="onGlobs"
      @keydown.enter.prevent="onGlobs"
    />

    <p v-if="store.error" class="search__error">{{ store.error }}</p>
    <p v-else-if="summary" class="search__summary">{{ summary }}</p>

    <SearchResults
      class="search__results"
      :groups="store.groups"
      :errors="store.errors"
      :running="store.running"
      :truncated="store.truncated"
      :match-count="store.matchCount"
      :file-count="store.fileCount"
      :empty="store.empty"
      @open="(path, line) => emit('open', path, line)"
    />
  </div>
</template>

<style scoped>
.search {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
  gap: 4px;
  padding: 6px;
}
.search__bar {
  display: flex;
  gap: 4px;
}
.search__input {
  flex: 1;
  min-width: 0;
  padding: 3px 6px;
  border: 1px solid #333;
  border-radius: 3px;
  background: #1c1c1c;
  color: #e4e4e4;
  font-size: 12px;
}
.search__input:focus {
  border-color: #4a6d8c;
  outline: none;
}
.search__toggles {
  display: flex;
  flex: none;
  gap: 2px;
}
.search__toggle {
  min-width: 22px;
  border: 1px solid transparent;
  border-radius: 3px;
  background: none;
  color: #7a7a7a;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 11px;
  cursor: pointer;
}
.search__toggle:hover {
  color: #e4e4e4;
}
.search__toggle--on {
  border-color: #4a6d8c;
  background: #24313c;
  color: #9ec4e4;
}
.search__toggle:disabled {
  color: #4a4a4a;
  cursor: default;
}
.search__glob {
  padding: 2px 6px;
  border: 1px solid #2c2c2c;
  border-radius: 3px;
  background: #1c1c1c;
  color: #c4c4c4;
  font-size: 11px;
}
.search__summary,
.search__error {
  margin: 0;
  padding: 0 2px;
  color: #7a7a7a;
  font-size: 11px;
}
.search__error {
  color: #e06c6c;
}
.search__results {
  flex: 1;
  min-height: 0;
}
</style>
