<script setup lang="ts">
// Tab strip for the open files (SPEC-002 §5.7).
//
// Presentational: every mutation goes through the editor store, so the
// auto-save-on-switch rule (`07:42`) lives in exactly one place.

import { useEditorStore } from '../stores/editor';

const props = defineProps<{ projectId: string }>();
const store = useEditorStore();

async function select(path: string): Promise<void> {
  await store.activate(props.projectId, path);
}

async function close(path: string): Promise<void> {
  await store.close(props.projectId, path);
}
</script>

<template>
  <nav v-if="store.tabs.length" class="tabs" role="tablist">
    <button
      v-for="tab in store.tabs"
      :key="tab.path"
      class="tabs__tab"
      :class="{ 'tabs__tab--active': tab.path === store.activePath }"
      role="tab"
      :aria-selected="tab.path === store.activePath"
      :title="tab.path"
      @click="select(tab.path)"
    >
      <!-- The dot is the only dirty indicator, so it carries a text label for
           screen readers rather than relying on colour alone. -->
      <span v-if="tab.dirty" class="tabs__dot" aria-hidden="true">●</span>
      <span class="tabs__name">{{ tab.name }}</span>
      <span v-if="tab.dirty" class="tabs__sr">(chưa lưu)</span>
      <span
        class="tabs__close"
        role="button"
        :aria-label="`Đóng ${tab.name}`"
        tabindex="0"
        @click.stop="close(tab.path)"
        @keydown.enter.stop.prevent="close(tab.path)"
        @keydown.space.stop.prevent="close(tab.path)"
        >×</span
      >
    </button>
  </nav>
</template>

<style scoped>
.tabs {
  display: flex;
  gap: 2px;
  padding: 4px 8px 0;
  overflow-x: auto;
  border-bottom: 1px solid #2c2c2c;
}
.tabs__tab {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 10px;
  border: 1px solid #2c2c2c;
  border-bottom: none;
  border-radius: 4px 4px 0 0;
  background: #1c1c1c;
  color: #b8b8b8;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
  white-space: nowrap;
}
.tabs__tab--active {
  background: #1e1e1e;
  color: #fff;
}
.tabs__dot {
  color: #e2b341;
  font-size: 10px;
}
.tabs__close {
  opacity: 0.6;
}
.tabs__close:hover,
.tabs__close:focus-visible {
  opacity: 1;
}
/* Visually hidden but reachable by assistive tech. */
.tabs__sr {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip-path: inset(50%);
  white-space: nowrap;
}
</style>
