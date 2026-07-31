<script setup lang="ts">
// The git panel (SPEC-005 §5.7) — branch bar, status, log, and the detail views.
//
// This is the only component that talks to the store; the seven below it are
// props-in/events-out. That split is what makes them testable without Pinia, and
// it keeps the decision of *what a click means* in one place: `GitStatusList`
// knows a row was clicked, this knows that means "open the staged diff".
//
// `isRepo:false` is a first-class state, not an error (C5, C47): a plain directory
// gets a sentence explaining what it is, and none of the actions render — a commit
// box over a folder with no repository can only produce failures.

import { computed, onBeforeUnmount, ref, watch } from 'vue';

import { fetchBlame, fetchBlob } from '../../api/git';
import type { GitBlame } from '../../api/git';
import { relativeTime } from '../../git/relativeTime';
import type { GroupedEntry } from '../../git/status';
import { useGitStore } from '../../stores/git';
import GitBlameView from './GitBlameView.vue';
import GitBranchMenu from './GitBranchMenu.vue';
import GitCommitBox from './GitCommitBox.vue';
import GitDiffView from './GitDiffView.vue';
import GitLogList from './GitLogList.vue';
import GitMergeEditor from './GitMergeEditor.vue';
import GitStatusList from './GitStatusList.vue';

const props = defineProps<{ projectId: string }>();

const store = useGitStore();

/** Which half of the panel is showing: the working tree, or history. */
const tab = ref<'status' | 'log'>('status');
/** The row whose diff is open, as `group:path` — `GitStatusList`'s own key. */
const selectedKey = ref<string | null>(null);
const blame = ref<GitBlame | null>(null);
const blameContent = ref('');

/** Whether checkout would carry or overwrite uncommitted work (C26, C48). */
const dirty = computed(() => {
  const counts = store.status.counts;
  // Untracked paths count too: checkout usually carries them, but it refuses when
  // the target branch owns the same path. Omitting them would leave that guarded
  // force-retry unreachable exactly when the collision happens.
  return counts.staged + counts.changed + counts.untracked + counts.conflicted > 0;
});

/** Human sentence for the branch bar's freshness indicator. */
const watchLabel = computed(() => {
  switch (store.watchMode) {
    case 'live':
      return 'realtime';
    // Said out loud on purpose (§5.7): a user who knows realtime is off will hit
    // refresh when it matters. One who doesn't will trust a stale panel.
    case 'polling':
      return 'đang poll';
    default:
      return '';
  }
});

const upstreamLabel = computed(() => {
  const up = store.status.upstream;
  if (!up) return '';
  const parts: string[] = [];
  if (up.ahead > 0) parts.push(`↑${up.ahead}`);
  if (up.behind > 0) parts.push(`↓${up.behind}`);
  return parts.length > 0 ? `${up.name} ${parts.join(' ')}` : up.name;
});

/** A repository mid-merge/rebase — shown so an abort is reachable (C29). */
const inProgress = computed(() =>
  store.status.state === 'clean' ? null : store.status.state,
);

// ---- lifecycle -------------------------------------------------------------

/**
 * Load and watch one project.
 *
 * `reset()` first: paths, oids and branches all belong to a specific repository,
 * so carrying any of it across a project switch would render the previous repo's
 * state under the new project's name.
 */
async function load(projectId: string): Promise<void> {
  store.reset();
  selectedKey.value = null;
  blame.value = null;
  await store.refresh(projectId);
  if (store.isRepo) await store.loadLog(projectId);
  // Watch plain directories too: an external `git init` must turn the notice into
  // a repository panel without requiring a remount or manual refresh (C35).
  store.startWatch(projectId);
}

watch(() => props.projectId, (id) => void load(id), { immediate: true });

// A `git init` while the panel is open turns a plain directory into a repository;
// the watcher reports it (C35), but the log and branches were never loaded.
watch(
  () => store.isRepo,
  async (isRepo) => {
    if (isRepo && store.commits.length === 0) {
      await store.loadLog(props.projectId);
      store.startWatch(props.projectId);
    }
  },
);

onBeforeUnmount(() => {
  // Leaving the last subscriber attached would keep the server polling `git
  // status` for a panel nobody is looking at (C38).
  store.stopWatch();
});

// ---- actions ---------------------------------------------------------------

/**
 * Open a row's diff.
 *
 * The group decides the side: a Staged row shows index-vs-HEAD, a Changed row
 * worktree-vs-index. For an `MM` file those are different diffs of the same path,
 * which is the whole reason the row appears in both groups (C9, C42).
 */
async function selectRow(row: GroupedEntry): Promise<void> {
  selectedKey.value = row.key;
  blame.value = null;
  if (row.group === 'conflicted') {
    await store.openConflict(props.projectId, row.path);
    return;
  }
  await store.openDiff(props.projectId, row.path, row.group === 'staged');
}

function closeDiff(): void {
  selectedKey.value = null;
  store.closeDiff();
}

async function showBlame(path: string): Promise<void> {
  store.closeDiff();
  try {
    // Blame needs the file's text beside it, and the worktree copy is what the
    // line numbers refer to.
    const [attribution, file] = await Promise.all([
      fetchBlame(props.projectId, path),
      fetchBlob(props.projectId, path, 'worktree'),
    ]);
    blame.value = attribution;
    blameContent.value = file.binary ? '' : file.content;
  } catch (err) {
    blame.value = null;
    store.error = err instanceof Error ? err.message : String(err);
  }
}

/** The path whose diff is open, for the blame button. */
const openPath = computed(() => store.diff?.path ?? null);
</script>

<template>
  <section class="git" aria-label="Git">
    <!-- Branch bar. Rendered even when this is not a repository, so the panel has
         a stable header instead of collapsing to a bare sentence. -->
    <header class="git__bar">
      <template v-if="store.isRepo">
        <GitBranchMenu
          :branches="store.branches"
          :dirty="dirty"
          :busy="store.busy"
          @checkout="(target, force) => store.switchTo(projectId, target, force)"
          @create="(name, checkout) => store.newBranch(projectId, name, { checkout })"
          @merge="(from, noFf) => store.mergeFrom(projectId, from, noFf)"
        />
        <span v-if="upstreamLabel" class="git__upstream">{{ upstreamLabel }}</span>
        <span
          v-if="inProgress"
          class="git__state"
          :title="`Repository đang ở giữa một ${inProgress}`"
        >{{ inProgress }}</span>
        <button
          v-if="inProgress === 'merge'"
          type="button"
          class="git__abort"
          :disabled="store.busy"
          @click="store.abortMergeNow(projectId)"
        >
          Huỷ merge
        </button>
      </template>
      <span v-else class="git__branch-empty">Git</span>

      <span class="git__spacer" />
      <span v-if="watchLabel" class="git__watch" :class="`git__watch--${store.watchMode}`">
        {{ watchLabel }}
      </span>
      <button
        type="button"
        class="git__refresh"
        :disabled="store.loading"
        title="Tải lại trạng thái"
        @click="store.refresh(projectId)"
      >
        ⟳
      </button>
    </header>

    <p v-if="store.error" class="git__error" role="alert">
      {{ store.error }}
      <button type="button" class="git__error-close" @click="store.dismissError()">×</button>
    </p>

    <!-- C47: not a repository → a sentence, and none of the actions. -->
    <p v-if="!store.isRepo" class="git__notice">
      Thư mục này không phải git repository.
      <span class="git__notice-hint">Chạy <code>git init</code> để bắt đầu theo dõi.</span>
    </p>

    <template v-else>
      <nav class="git__tabs">
        <button
          type="button"
          class="git__tab"
          :class="{ 'git__tab--on': tab === 'status' }"
          @click="tab = 'status'"
        >
          Thay đổi
        </button>
        <button
          type="button"
          class="git__tab"
          :class="{ 'git__tab--on': tab === 'log' }"
          @click="tab = 'log'"
        >
          Lịch sử
        </button>
      </nav>

      <div v-if="tab === 'status'" class="git__body">
        <GitStatusList
          :entries="store.status.entries"
          :selected-key="selectedKey"
          :busy="store.busy"
          @select="selectRow"
          @stage="(paths) => store.stagePaths(projectId, paths)"
          @unstage="(paths) => store.unstagePaths(projectId, paths)"
          @discard="(paths) => store.discardPaths(projectId, paths)"
          @resolve="(path) => store.openConflict(projectId, path)"
        />
        <GitCommitBox
          :can-commit="store.canCommit"
          :busy="store.busy"
          :has-conflicts="store.hasConflicts"
          @commit="(message, amend) => store.commitStaged(projectId, message, amend)"
        />
      </div>

      <div v-else class="git__body">
        <GitLogList
          :commits="store.commits"
          :next-before="store.nextBefore"
          :busy="store.busy"
          :selected-oid="store.commitDetail?.commit.oid ?? null"
          @select="(oid) => store.openCommit(projectId, oid)"
          @load-more="store.loadMore(projectId)"
        />
        <section v-if="store.commitDetail" class="detail">
          <header class="detail__head">
            <code class="detail__oid">{{ store.commitDetail.commit.short }}</code>
            <span class="detail__summary">{{ store.commitDetail.commit.summary }}</span>
            <button type="button" class="detail__close" @click="store.closeCommit()">×</button>
          </header>
          <p class="detail__meta">
            {{ store.commitDetail.commit.author.name }} ·
            {{ relativeTime(store.commitDetail.commit.author.time) }}
          </p>
          <pre v-if="store.commitDetail.commit.body" class="detail__body">{{
            store.commitDetail.commit.body
          }}</pre>
          <ul class="detail__files">
            <li v-for="file in store.commitDetail.files" :key="file.path" class="detail__file">
              <span class="detail__change">{{ file.change.charAt(0).toUpperCase() }}</span>
              <span class="detail__path">{{ file.path }}</span>
              <span class="detail__counts">
                <span class="detail__added">+{{ file.added }}</span>
                <span class="detail__removed">−{{ file.removed }}</span>
              </span>
            </li>
          </ul>
        </section>
      </div>
    </template>

    <!-- Detail views. Only one is open at a time: they all describe one file, and
         two of them side by side would be answering different questions. -->
    <GitMergeEditor
      v-if="store.conflict"
      :conflict="store.conflict"
      :busy="store.busy"
      @resolve="(path, content) => store.resolvePath(projectId, path, content)"
      @close="store.closeConflict()"
    />
    <GitBlameView
      v-else-if="blame"
      :blame="blame"
      :content="blameContent"
      @commit="(oid) => { tab = 'log'; store.openCommit(projectId, oid); }"
      @close="blame = null"
    />
    <template v-else-if="store.diff">
      <GitDiffView
        :diff="store.diff"
        :busy="store.busy"
        @stage="(path) => store.stagePaths(projectId, [path])"
        @unstage="(path) => store.unstagePaths(projectId, [path])"
        @discard="(path) => store.discardPaths(projectId, [path])"
        @stage-hunk="(path, content) => store.stageHunk(projectId, path, content)"
        @unstage-hunk="(path, content, exists) => store.unstageHunk(projectId, path, content, exists)"
        @discard-hunk="(path, content, expectedOid) => store.discardHunk(projectId, path, content, expectedOid)"
        @close="closeDiff"
      />
      <button
        v-if="openPath"
        type="button"
        class="git__blame-btn"
        @click="showBlame(openPath)"
      >
        Xem blame
      </button>
    </template>
  </section>
</template>

<style scoped>
.git {
  display: flex;
  flex-direction: column;
  min-height: 0;
  height: 100%;
  background: #171717;
  color: #c8c8c8;
  font-size: 12px;
}
.git__bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 6px;
  border-bottom: 1px solid #2c2c2c;
}
.git__spacer {
  flex: 1;
}
.git__branch-empty {
  color: #7a7a7a;
  font-weight: 600;
}
.git__upstream,
.git__state {
  color: #8a8a8a;
  font-size: 11px;
}
.git__state {
  padding: 0 4px;
  border-radius: 3px;
  background: #4a3a1a;
  color: #ffd79b;
  text-transform: uppercase;
}
.git__abort,
.git__refresh,
.git__blame-btn {
  border: 1px solid #3a3a3a;
  border-radius: 3px;
  background: #232323;
  color: #c8c8c8;
  font-size: 11px;
  cursor: pointer;
}
.git__abort,
.git__blame-btn {
  padding: 1px 6px;
}
.git__refresh {
  padding: 0 5px;
}
.git__refresh:disabled {
  opacity: 0.5;
  cursor: default;
}
.git__watch {
  font-size: 10px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.git__watch--live {
  color: #6f8f72;
}
/* Polling is a degraded mode, so it reads as a warning rather than as status. */
.git__watch--polling {
  color: #d3a83c;
}
.git__error {
  display: flex;
  gap: 6px;
  margin: 0;
  padding: 4px 6px;
  background: #3a1f1f;
  color: #ffb4b4;
}
.git__error-close {
  margin-left: auto;
  border: 0;
  background: none;
  color: inherit;
  cursor: pointer;
}
.git__notice {
  margin: 0;
  padding: 10px;
  color: #8a8a8a;
  line-height: 1.5;
}
.git__notice-hint {
  display: block;
  color: #6e6e6e;
  font-size: 11px;
}
.git__notice code {
  padding: 0 3px;
  border-radius: 2px;
  background: #232323;
}
.git__tabs {
  display: flex;
  gap: 2px;
  padding: 4px 6px 0;
}
.git__tab {
  padding: 2px 8px;
  border: 1px solid transparent;
  border-radius: 3px 3px 0 0;
  background: none;
  color: #8a8a8a;
  font-size: 11px;
  cursor: pointer;
}
.git__tab--on {
  border-color: #2c2c2c;
  border-bottom-color: #171717;
  background: #1e1e1e;
  color: #dcdcdc;
}
.git__body {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
  border-top: 1px solid #2c2c2c;
}
.detail {
  border-top: 1px solid #2c2c2c;
  padding: 6px;
  max-height: 40%;
  overflow: auto;
}
.detail__head {
  display: flex;
  gap: 6px;
  align-items: baseline;
}
.detail__oid {
  color: #d3a83c;
}
.detail__summary {
  color: #dcdcdc;
  font-weight: 600;
}
.detail__close {
  margin-left: auto;
  border: 0;
  background: none;
  color: #8a8a8a;
  cursor: pointer;
}
.detail__meta {
  margin: 2px 0 6px;
  color: #8a8a8a;
  font-size: 11px;
}
.detail__body {
  margin: 0 0 6px;
  color: #b4b4b4;
  font-family: inherit;
  white-space: pre-wrap;
}
.detail__files {
  margin: 0;
  padding: 0;
  list-style: none;
}
.detail__file {
  display: flex;
  gap: 6px;
  padding: 1px 0;
}
.detail__change {
  width: 12px;
  color: #8a8a8a;
  text-align: center;
}
.detail__path {
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.detail__counts {
  display: flex;
  gap: 4px;
  font-size: 11px;
}
.detail__added {
  color: #6f8f72;
}
.detail__removed {
  color: #b06a6a;
}
</style>
