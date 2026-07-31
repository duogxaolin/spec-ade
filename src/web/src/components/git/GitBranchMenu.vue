<script setup lang="ts">
// Branch list, create, checkout and merge (C24–C29, C48).
//
// The rule this component exists to enforce is C48: a checkout while the tree is
// dirty must ask before retrying with `force`. `git checkout` itself refuses that
// switch, and the server turns the refusal into a 409 (`GitError::Blocked`) — so
// the only way to lose work here is for the UI to send `force: true` on the user's
// behalf. It never does: the confirm step is a separate render, and the emit that
// carries `force` only happens after the user picked it.
//
// The confirmation is in-component state rather than `window.confirm` because a
// native dialog cannot be asserted in vitest without stubbing a global, and this
// is the one interaction where the test *is* the guarantee.

import { computed, ref } from 'vue';

import type { GitBranches, LocalBranch } from '../../api/git';

const props = defineProps<{
  branches: GitBranches;
  /** Whether the working tree has changes a checkout would have to carry (C26). */
  dirty: boolean;
  busy?: boolean;
}>();

const emit = defineEmits<{
  /** `force` is only ever `true` after the user confirmed it (C48). */
  checkout: [target: string, force: boolean];
  create: [name: string, checkout: boolean];
  merge: [from: string, noFf: boolean];
}>();

/** The branch a confirm dialog is currently about, `null` when none is open. */
const pendingCheckout = ref<string | null>(null);
const newName = ref('');
const createAndSwitch = ref(true);
const noFf = ref(false);

const others = computed(() => props.branches.local.filter((b) => !b.current));

function requestCheckout(branch: LocalBranch): void {
  if (props.dirty) {
    // Ask first. Sending `force` here would discard the user's uncommitted work
    // on the strength of a single click (C48).
    pendingCheckout.value = branch.name;
    return;
  }
  emit('checkout', branch.name, false);
}

function confirmForce(): void {
  const target = pendingCheckout.value;
  if (target === null) return;
  pendingCheckout.value = null;
  emit('checkout', target, true);
}

function cancelCheckout(): void {
  pendingCheckout.value = null;
}

function submitCreate(): void {
  const name = newName.value.trim();
  if (name.length === 0) return;
  emit('create', name, createAndSwitch.value);
  newName.value = '';
}

/** Ahead/behind, shown only when there is something to say. */
function tracking(branch: LocalBranch): string {
  if (!branch.upstream) return '';
  const parts: string[] = [];
  if (branch.ahead > 0) parts.push(`↑${branch.ahead}`);
  if (branch.behind > 0) parts.push(`↓${branch.behind}`);
  return parts.join(' ');
}
</script>

<template>
  <div class="branches">
    <section v-if="pendingCheckout !== null" class="branches__confirm" role="alertdialog">
      <p class="branches__warn">
        Working tree đang có thay đổi chưa commit. Chuyển sang
        <strong>{{ pendingCheckout }}</strong> sẽ <strong>mất</strong> các thay đổi đó.
      </p>
      <div class="branches__actions">
        <button type="button" class="branches__danger" :disabled="busy" @click="confirmForce">
          Chuyển và bỏ thay đổi
        </button>
        <button type="button" @click="cancelCheckout">Huỷ</button>
      </div>
    </section>

    <template v-else>
      <form class="branches__create" @submit.prevent="submitCreate">
        <input
          v-model="newName"
          class="branches__input"
          type="text"
          placeholder="Branch mới…"
          aria-label="Tên branch mới"
        />
        <label class="branches__check">
          <input v-model="createAndSwitch" type="checkbox" />
          chuyển luôn
        </label>
        <button type="submit" :disabled="busy || newName.trim().length === 0">Tạo</button>
      </form>

      <p class="branches__current">
        Đang ở <strong>{{ branches.current ?? '(detached)' }}</strong>
      </p>

      <ul class="branches__list">
        <li v-for="branch in others" :key="branch.name" class="branches__item">
          <span class="branches__name">{{ branch.name }}</span>
          <span v-if="tracking(branch)" class="branches__track">{{ tracking(branch) }}</span>
          <button type="button" :disabled="busy" @click="requestCheckout(branch)">Chuyển</button>
          <button type="button" :disabled="busy" @click="emit('merge', branch.name, noFf)">
            Merge
          </button>
        </li>
      </ul>

      <label class="branches__check">
        <input v-model="noFf" type="checkbox" />
        merge commit dù fast-forward được
      </label>

      <details v-if="branches.remote.length" class="branches__remote">
        <summary>Remote ({{ branches.remote.length }})</summary>
        <ul class="branches__list">
          <li v-for="branch in branches.remote" :key="branch.name" class="branches__item">
            <span class="branches__name">{{ branch.name }}</span>
            <!-- Checkout of a remote branch goes through the same guard: git
                 creates the local tracking branch, so the dirty check still
                 applies. -->
            <button
              type="button"
              :disabled="busy"
              @click="requestCheckout({ ...branch, upstream: null, ahead: 0, behind: 0, current: false })"
            >
              Chuyển
            </button>
          </li>
        </ul>
      </details>
    </template>
  </div>
</template>

<style scoped>
.branches {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 6px;
  font-size: 12px;
  color: #c4c4c4;
}
.branches__confirm {
  padding: 6px;
  border: 1px solid #6d4a2f;
  border-radius: 3px;
  background: #2a1f16;
}
.branches__warn {
  margin: 0 0 6px;
  color: #ffd79b;
}
.branches__actions {
  display: flex;
  gap: 6px;
}
.branches__danger {
  border-color: #7a3b3b;
  background: #3a2020;
  color: #ffb3b3;
}
.branches__create {
  display: flex;
  gap: 6px;
  align-items: center;
}
.branches__input {
  flex: 1;
  min-width: 0;
  padding: 3px 5px;
  border: 1px solid #2c2c2c;
  border-radius: 3px;
  background: #171717;
  color: #d4d4d4;
  font: inherit;
}
.branches__current {
  margin: 0;
  color: #9e9e9e;
}
.branches__list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.branches__item {
  display: flex;
  gap: 6px;
  align-items: center;
  padding: 2px 0;
}
.branches__name {
  flex: 1;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.branches__track {
  color: #7fa8c9;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
.branches__check {
  display: flex;
  gap: 4px;
  align-items: center;
  color: #9e9e9e;
}
.branches__remote summary {
  color: #9e9e9e;
  cursor: pointer;
}
button {
  padding: 2px 6px;
  border: 1px solid #2c2c2c;
  border-radius: 3px;
  background: #1e1e1e;
  color: #c4c4c4;
  font: inherit;
  font-size: 11px;
  cursor: pointer;
}
button:disabled {
  color: #6a6a6a;
  cursor: default;
}
</style>
