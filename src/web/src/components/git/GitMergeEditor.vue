<script setup lang="ts">
// 3-way conflict editor (C31, §5.7).
//
// `MergeView` side by side: `ours` on the left, `theirs` on the right, and the
// left side is the editable one — that is the document the user commits. `base`
// is fetched too and shown as context, because a conflict is often only
// understandable from what both sides changed *away from*.
//
// `@codemirror/merge` is a dynamic `import()` here for the same reason as
// `GitDiffView`: nothing static may pull it into the entry chunk.
//
// The content sent to the server is the left document, with markers already gone —
// the server writes it verbatim and stages the path, so whatever is on screen when
// "Đã resolve" is pressed is exactly what gets committed. That is why the button is
// disabled while conflict markers remain: staging a file containing `<<<<<<<` is
// the single most common way to commit a broken merge.

import { computed, markRaw, onBeforeUnmount, ref, shallowRef, useTemplateRef, watch } from 'vue';

import type { GitConflict } from '../../api/git';

const props = defineProps<{
  conflict: GitConflict | null;
  busy?: boolean;
}>();

const emit = defineEmits<{
  resolve: [path: string, content: string];
  close: [];
}>();

const host = useTemplateRef<HTMLDivElement>('host');
/** The `MergeView`, kept raw. `null` until the chunk has loaded. */
const merge = shallowRef<{ a: { state: { doc: { toString(): string } } }; destroy(): void } | null>(
  null,
);
const loadError = ref<string | null>(null);
const showBase = ref(false);
/** Bumped on every document change, so the marker check below re-evaluates. */
const docVersion = ref(0);

/** Conflict markers git writes into the worktree copy. */
const MARKERS = ['<<<<<<<', '=======', '>>>>>>>'];

const currentText = computed(() => {
  // `docVersion` is read so this recomputes when the document changes; the CM6
  // state itself is deliberately non-reactive, so nothing else would trigger it.
  void docVersion.value;
  return merge.value?.a.state.doc.toString() ?? '';
});

const stillConflicted = computed(() =>
  MARKERS.some((marker) => currentText.value.includes(marker)),
);

async function render(): Promise<void> {
  merge.value?.destroy();
  merge.value = null;
  loadError.value = null;

  const conflict = props.conflict;
  if (!conflict || !host.value || conflict.binary) return;

  try {
    const [{ EditorView }, { EditorState }, mergeMod, { oneDark }] = await Promise.all([
      import('@codemirror/view'),
      import('@codemirror/state'),
      import('@codemirror/merge'),
      import('@codemirror/theme-one-dark'),
    ]);

    if (!host.value || props.conflict !== conflict) return;

    const view = new mergeMod.MergeView({
      a: {
        doc: conflict.ours ?? '',
        extensions: [
          EditorView.lineWrapping,
          oneDark,
          // Any edit invalidates the marker check.
          EditorView.updateListener.of((update) => {
            if (update.docChanged) docVersion.value += 1;
          }),
        ],
      },
      b: {
        doc: conflict.theirs ?? '',
        extensions: [
          EditorView.lineWrapping,
          oneDark,
          // The right side is `theirs`: read-only, because editing it would
          // suggest it goes somewhere, and it does not.
          EditorState.readOnly.of(true),
        ],
      },
      parent: host.value,
    });

    merge.value = markRaw(view) as unknown as typeof merge.value;
    docVersion.value += 1;
  } catch (err) {
    loadError.value = err instanceof Error ? err.message : String(err);
  }
}

watch(
  () => [props.conflict?.path, props.conflict?.ours, props.conflict?.theirs],
  () => {
    void render();
  },
  { immediate: true, flush: 'post' },
);

onBeforeUnmount(() => {
  merge.value?.destroy();
  merge.value = null;
});

function submit(): void {
  const conflict = props.conflict;
  if (!conflict || stillConflicted.value) return;
  emit('resolve', conflict.path, currentText.value);
}
</script>

<template>
  <section v-if="conflict" class="mergev">
    <header class="mergev__head">
      <span class="mergev__path" :title="conflict.path">{{ conflict.path }}</span>
      <span class="mergev__hint">bên trái = của bạn (ours) · bên phải = nhánh kia (theirs)</span>
      <span class="mergev__spacer" />
      <button
        v-if="conflict.base !== null"
        type="button"
        class="mergev__btn"
        :aria-pressed="showBase"
        @click="showBase = !showBase"
      >
        Base
      </button>
      <button
        type="button"
        class="mergev__btn mergev__btn--primary"
        :disabled="busy || stillConflicted || merge === null"
        :title="stillConflicted ? 'Còn conflict marker trong file' : undefined"
        @click="submit"
      >
        Đã resolve
      </button>
      <button type="button" class="mergev__btn" aria-label="Đóng" @click="emit('close')">✕</button>
    </header>

    <p v-if="conflict.binary" class="mergev__note">
      File binary — chọn một bên bằng <code>git checkout --ours/--theirs</code>.
    </p>
    <p v-else-if="loadError" class="mergev__note mergev__note--error">
      Không tải được trình merge: {{ loadError }}
    </p>
    <p v-else-if="stillConflicted" class="mergev__note mergev__note--warn">
      File còn conflict marker — xoá hết <code>&lt;&lt;&lt;&lt;&lt;&lt;&lt;</code>,
      <code>=======</code>, <code>&gt;&gt;&gt;&gt;&gt;&gt;&gt;</code> trước khi resolve.
    </p>

    <pre v-if="showBase" class="mergev__base">{{ conflict.base ?? '(không có base)' }}</pre>
    <div v-show="!conflict.binary" ref="host" class="mergev__host" />
  </section>
</template>

<style scoped>
.mergev {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-top: 1px solid #2c2c2c;
  background: #161616;
}
.mergev__head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-bottom: 1px solid #2c2c2c;
  font-size: 11px;
  color: #9e9e9e;
}
.mergev__path {
  overflow: hidden;
  color: #d4d4d4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.mergev__hint {
  color: #6f6f6f;
}
.mergev__spacer {
  flex: 1;
}
.mergev__btn {
  padding: 2px 6px;
  border: 1px solid #333;
  border-radius: 3px;
  background: #1f1f1f;
  color: #c4c4c4;
  font-size: 11px;
  cursor: pointer;
}
.mergev__btn:hover:not(:disabled) {
  background: #2a2a2a;
}
.mergev__btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.mergev__btn[aria-pressed='true'] {
  border-color: #4a6f8a;
  color: #9ecbff;
}
.mergev__btn--primary {
  border-color: #3f5f3f;
  color: #a7d7a7;
}
.mergev__note {
  margin: 0;
  padding: 8px;
  color: #9e9e9e;
  font-size: 12px;
}
.mergev__note--error {
  color: #f0a0a0;
}
.mergev__note--warn {
  color: #ffd79b;
}
.mergev__note code {
  color: #d4d4d4;
}
.mergev__base {
  margin: 0;
  max-height: 140px;
  padding: 8px;
  overflow: auto;
  border-bottom: 1px solid #2c2c2c;
  color: #9e9e9e;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}
.mergev__host {
  min-height: 0;
  overflow: auto;
}
</style>
