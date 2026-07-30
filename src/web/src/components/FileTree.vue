<script setup lang="ts">
// Lazy project file tree (SPEC-002 §5.7 / [INVENTED-6]).
//
// Each directory fetches its own children the first time it is expanded and the
// listing is cached by path. That is the whole point of the depth-1 endpoint: a
// large repo must not pay for a full walk to show its root.

import { computed, ref, watch } from 'vue';

import { readTree, type DirListing, type TreeEntry } from '../api/files';

const props = defineProps<{
  projectId: string | null;
  /** Highlighted row — the file currently open in the editor. */
  selectedPath?: string | null;
}>();

const emit = defineEmits<{
  open: [path: string];
  error: [message: string];
}>();

/** Listing per directory path (`''` = project root). */
const listings = ref<Record<string, DirListing>>({});
const expanded = ref<Set<string>>(new Set(['']));
const loading = ref<Set<string>>(new Set());

/** A flattened render list, so the template needs no recursive component. */
interface Row {
  entry: TreeEntry;
  depth: number;
}

const rows = computed<Row[]>(() => {
  const out: Row[] = [];
  const walk = (dir: string, depth: number): void => {
    for (const entry of listings.value[dir]?.entries ?? []) {
      out.push({ entry, depth });
      if (entry.kind === 'dir' && expanded.value.has(entry.path)) {
        walk(entry.path, depth + 1);
      }
    }
  };
  walk('', 0);
  return out;
});

const rootTruncated = computed(() => listings.value['']?.truncated ?? false);

async function load(path: string): Promise<void> {
  if (!props.projectId || loading.value.has(path)) return;
  loading.value = new Set(loading.value).add(path);
  try {
    const listing = await readTree(props.projectId, path);
    listings.value = { ...listings.value, [path]: listing };
  } catch (err) {
    emit('error', err instanceof Error ? err.message : String(err));
    // Collapse again: an expanded-but-empty folder looks like an empty folder,
    // which is a different (and wrong) statement about the filesystem.
    if (path !== '') {
      const next = new Set(expanded.value);
      next.delete(path);
      expanded.value = next;
    }
  } finally {
    const next = new Set(loading.value);
    next.delete(path);
    loading.value = next;
  }
}

function toggle(entry: TreeEntry): void {
  const next = new Set(expanded.value);
  if (next.has(entry.path)) {
    next.delete(entry.path);
  } else {
    next.add(entry.path);
    if (!listings.value[entry.path]) void load(entry.path);
  }
  expanded.value = next;
}

function activate(entry: TreeEntry): void {
  // Symlinks are listed but not traversed by the walker ([INVENTED-12]); opening
  // one still goes through read, where the guard decides if the target is legal.
  if (entry.kind === 'dir') {
    toggle(entry);
  } else {
    emit('open', entry.path);
  }
}

/** Drop every cached listing and reload the root — used after a mutation. */
async function refresh(): Promise<void> {
  listings.value = {};
  const dirs = [...expanded.value];
  await load('');
  // Re-fetch the directories that were open, so an expanded tree survives the
  // refresh instead of collapsing under the user.
  await Promise.all(dirs.filter((d) => d !== '').map((d) => load(d)));
}

defineExpose({ refresh });

watch(
  () => props.projectId,
  (id) => {
    listings.value = {};
    expanded.value = new Set(['']);
    if (id) void load('');
  },
  { immediate: true },
);
</script>

<template>
  <div class="tree">
    <p v-if="!projectId" class="tree__empty">No project selected.</p>
    <template v-else>
      <button
        v-for="row in rows"
        :key="row.entry.path"
        class="tree__row"
        :class="{ 'tree__row--active': row.entry.path === selectedPath }"
        :style="{ paddingLeft: `${8 + row.depth * 14}px` }"
        :title="row.entry.path"
        @click="activate(row.entry)"
      >
        <span class="tree__twisty">
          {{
            row.entry.kind === 'dir' ? (expanded.has(row.entry.path) ? '▾' : '▸') : ''
          }}
        </span>
        <span class="tree__name" :class="{ 'tree__name--link': row.entry.kind === 'symlink' }">
          {{ row.entry.name }}
        </span>
        <span v-if="loading.has(row.entry.path)" class="tree__hint">…</span>
        <span
          v-else-if="row.entry.kind === 'dir' && listings[row.entry.path]?.truncated"
          class="tree__hint"
          title="Directory listing was cut at the entry cap"
          >cut</span
        >
      </button>
      <p v-if="rootTruncated" class="tree__hint tree__hint--block">
        Listing truncated at the entry cap.
      </p>
      <p v-if="!rows.length && !loading.has('')" class="tree__empty">Empty project.</p>
    </template>
  </div>
</template>

<style scoped>
.tree {
  display: flex;
  flex-direction: column;
  overflow: auto;
  height: 100%;
  font-size: 12px;
}
.tree__row {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px;
  border: none;
  background: none;
  color: #cfcfcf;
  cursor: pointer;
  font: inherit;
  text-align: left;
  white-space: nowrap;
}
.tree__row:hover {
  background: #232323;
}
.tree__row--active {
  background: #2d2d2d;
  color: #fff;
}
.tree__twisty {
  display: inline-block;
  width: 10px;
  color: #8a8a8a;
}
.tree__name--link {
  font-style: italic;
  color: #9ecbff;
}
.tree__hint {
  color: #8a8a8a;
  font-size: 10px;
}
.tree__hint--block {
  padding: 4px 8px;
}
.tree__empty {
  padding: 12px 8px;
  color: #8a8a8a;
}
</style>
