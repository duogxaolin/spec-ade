<script setup lang="ts">
// One file's diff, rendered with CodeMirror's `unifiedMergeView` (§5.7).
//
// `@codemirror/merge` is loaded by dynamic `import()` and nothing else in the app
// imports it statically — that is what keeps it out of the entry chunk, which
// `scripts/verify-spec-005.mjs` asserts on the built bundle. SPEC-004 §2.2 settled
// this rule for mermaid/katex; the merge addon is the same trade: a few hundred KB
// nobody pays for until they open a diff.
//
// The view is a `shallowRef` + `markRaw`, the same rule `EditorPane` follows: a
// deep Vue proxy over CM6 internals is a documented footgun (`07:40`, `04:39`).
//
// Why `oldText`/`newText` rather than the patch: `unifiedMergeView` needs both
// whole documents to compute its own hunks, and it gives inner-word highlighting
// for free. The `patch` field is still what a user copies, so it stays available
// behind a toggle.

import { markRaw, onBeforeUnmount, ref, shallowRef, useTemplateRef, watch } from 'vue';

import type { GitDiff } from '../../api/git';

const props = defineProps<{
  diff: GitDiff | null;
  /** Disable the actions while a mutation is in flight. */
  busy?: boolean;
}>();

const emit = defineEmits<{
  stage: [path: string];
  unstage: [path: string];
  discard: [path: string];
  /** A CodeMirror hunk control produced the complete target-side document. */
  stageHunk: [path: string, content: string];
  unstageHunk: [path: string, content: string, exists: boolean];
  discardHunk: [path: string, content: string, expectedOid: string];
  close: [];
}>();

interface DiffEditorView {
  state: import('@codemirror/state').EditorState;
  destroy(): void;
}

const host = useTemplateRef<HTMLDivElement>('host');
const view = shallowRef<DiffEditorView | null>(null);
const showPatch = ref(false);
const loadError = ref<string | null>(null);

/**
 * Build the merge view for the current diff.
 *
 * Rebuilt rather than reconfigured on every change: a diff view's two documents
 * are its identity, so there is no cursor or undo history worth preserving across
 * files the way `EditorPane` preserves them across tabs.
 */
async function render(): Promise<void> {
  view.value?.destroy();
  view.value = null;
  loadError.value = null;

  const diff = props.diff;
  if (!diff || !host.value || diff.binary || diff.truncated) return;

  try {
    // The three imports that must stay dynamic. Awaited together so a slow
    // network costs one round of latency, not three.
    const [{ EditorView }, { EditorState }, merge, { oneDark }] = await Promise.all([
      import('@codemirror/view'),
      import('@codemirror/state'),
      import('@codemirror/merge'),
      import('@codemirror/theme-one-dark'),
    ]);

    // The element can go away while the imports resolve (the user closed the pane
    // or picked another file), and mounting into a detached node leaks a view.
    if (!host.value || props.diff !== diff) return;

    view.value = markRaw(
      new EditorView({
        state: EditorState.create({
          doc: diff.newText,
          extensions: [
            merge.unifiedMergeView({
              original: diff.oldText,
              // CodeMirror owns the hunk boundaries. Its control updates the local
              // document first; the queued microtask then sends that complete,
              // unambiguous target state to the mutation endpoint ([INVENTED-10]).
              mergeControls: (type, action) => {
                const button = document.createElement('button');
                button.type = 'button';
                button.className = `git-hunk git-hunk--${type}`;
                const isWorktree = !diff.staged;
                button.textContent = type === 'accept'
                  ? 'Stage hunk'
                  : (isWorktree ? 'Discard hunk' : 'Unstage hunk');

                // A staged diff has one meaningful transition: reject a chunk back
                // toward HEAD. Accept would only hide it locally without changing
                // Git. Likewise, discarding the sole hunk of an untracked file would
                // delete work that exists nowhere else, which [INVENTED-2] forbids.
                const unavailable = (diff.staged && type === 'accept') ||
                  (isWorktree && type === 'reject' && !diff.oldExists);
                button.hidden = unavailable;
                button.onmousedown = (event) => {
                  if (props.busy || unavailable) {
                    event.preventDefault();
                    return;
                  }
                  action(event);
                  queueMicrotask(() => {
                    const editor = view.value;
                    if (!editor) return;
                    if (diff.staged) {
                      emit('unstageHunk', diff.path, editor.state.doc.toString(), diff.oldExists);
                    } else if (type === 'accept') {
                      // acceptChunk updates the *original* document (the index side),
                      // not the visible worktree document.
                      const content = merge.getOriginalDoc(editor.state).toString();
                      emit('stageHunk', diff.path, content);
                    } else {
                      const expectedOid = diff.worktreeOid;
                      if (expectedOid) {
                        emit('discardHunk', diff.path, editor.state.doc.toString(), expectedOid);
                      }
                    }
                  });
                };
                return button;
              },
            }),
            // The view is changed only by CodeMirror's hunk controls, never by
            // arbitrary typing. This keeps each mutation tied to one visible chunk.
            EditorView.editable.of(false),
            EditorView.lineWrapping,
            oneDark,
          ],
        }),
        parent: host.value,
      }),
    );
  } catch (err) {
    // A failed chunk load (offline, a stale service worker) must say so rather
    // than leaving an empty box that looks like an empty diff.
    loadError.value = err instanceof Error ? err.message : String(err);
  }
}

watch(
  () => [props.diff?.path, props.diff?.staged, props.diff?.oldText, props.diff?.newText],
  () => {
    void render();
  },
  { immediate: true, flush: 'post' },
);

onBeforeUnmount(() => {
  view.value?.destroy();
  view.value = null;
});

/** Keep the discriminated event names literal for Vue's typed emitter. */
function toggleStaged(): void {
  const diff = props.diff;
  if (!diff) return;
  if (diff.staged) emit('unstage', diff.path);
  else emit('stage', diff.path);
}
</script>

<template>
  <section v-if="diff" class="diff">
    <header class="diff__head">
      <span class="diff__path" :title="diff.path">{{ diff.path }}</span>
      <span class="diff__side">{{ diff.staged ? 'index ↔ HEAD' : 'worktree ↔ index' }}</span>
      <span v-if="!diff.binary" class="diff__stat">
        <span class="diff__added">+{{ diff.added }}</span>
        <span class="diff__removed">−{{ diff.removed }}</span>
      </span>
      <span class="diff__spacer" />
      <button
        v-if="!diff.binary"
        type="button"
        class="diff__btn"
        :aria-pressed="showPatch"
        @click="showPatch = !showPatch"
      >
        Patch
      </button>
      <button
        type="button"
        class="diff__btn"
        :disabled="busy"
        @click="toggleStaged"
      >
        {{ diff.staged ? 'Unstage' : 'Stage' }}
      </button>
      <button
        type="button"
        class="diff__btn diff__btn--danger"
        :disabled="busy"
        @click="emit('discard', diff.path)"
      >
        Discard
      </button>
      <button type="button" class="diff__btn" aria-label="Đóng diff" @click="emit('close')">
        ✕
      </button>
    </header>

    <!-- Binary and over-size are *answers*, not failures: the server deliberately
         ships no content for either (C10, [INVENTED-3]), so say which one it is. -->
    <p v-if="diff.binary" class="diff__note">File binary — không hiển thị diff.</p>
    <p v-else-if="diff.truncated" class="diff__note">
      File quá lớn để diff. Mở trong editor để xem nội dung.
    </p>
    <p v-else-if="loadError" class="diff__note diff__note--error">
      Không tải được trình xem diff: {{ loadError }}
    </p>

    <pre v-if="showPatch && !diff.binary" class="diff__patch">{{ diff.patch }}</pre>
    <div v-show="!showPatch && !diff.binary && !diff.truncated" ref="host" class="diff__host" />
  </section>
</template>

<style scoped>
.diff {
  display: flex;
  flex-direction: column;
  min-height: 0;
  border-top: 1px solid #2c2c2c;
  background: #161616;
}
.diff__head {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 8px;
  border-bottom: 1px solid #2c2c2c;
  font-size: 11px;
  color: #9e9e9e;
}
.diff__path {
  overflow: hidden;
  color: #d4d4d4;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.diff__side {
  color: #6f6f6f;
}
.diff__stat {
  display: flex;
  gap: 4px;
  font-variant-numeric: tabular-nums;
}
.diff__added {
  color: #6fcf74;
}
.diff__removed {
  color: #d97070;
}
.diff__spacer {
  flex: 1;
}
.diff__btn {
  padding: 2px 6px;
  border: 1px solid #333;
  border-radius: 3px;
  background: #1f1f1f;
  color: #c4c4c4;
  font-size: 11px;
  cursor: pointer;
}
.diff__btn:hover:not(:disabled) {
  background: #2a2a2a;
}
.diff__btn:disabled {
  opacity: 0.5;
  cursor: default;
}
.diff__btn[aria-pressed='true'] {
  border-color: #4a6f8a;
  color: #9ecbff;
}
.diff__btn--danger:hover:not(:disabled) {
  border-color: #7a3535;
  color: #f0a0a0;
}
.diff__note {
  margin: 0;
  padding: 12px;
  color: #9e9e9e;
  font-size: 12px;
}
.diff__note--error {
  color: #f0a0a0;
}
.diff__patch {
  margin: 0;
  padding: 8px;
  overflow: auto;
  color: #c4c4c4;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
}
.diff__host {
  min-height: 0;
  overflow: auto;
}
.diff__host :deep(.git-hunk) {
  margin: 2px;
  padding: 1px 5px;
  border: 1px solid #3f5f3f;
  border-radius: 3px;
  background: #1f2b1f;
  color: #a7d7a7;
  font: inherit;
  cursor: pointer;
}
.diff__host :deep(.git-hunk--reject) {
  border-color: #6a3f3f;
  background: #2b1f1f;
  color: #e6a2a2;
}
</style>
