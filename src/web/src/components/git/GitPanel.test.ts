// @vitest-environment jsdom
//
// GitPanel's non-repository branch is an information state, not a broken version
// of the repository UI (C47): no list and, most importantly, no commit box.

import { createPinia, setActivePinia } from 'pinia';
import { flushPromises, mount } from '@vue/test-utils';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import GitPanel from './GitPanel.vue';

const { fetchStatus, fetchBranches, fetchLog, gitEventSource } = vi.hoisted(() => ({
  fetchStatus: vi.fn(),
  fetchBranches: vi.fn(),
  fetchLog: vi.fn(),
  gitEventSource: vi.fn(),
}));

vi.mock('../../api/git', async () => {
  const actual = await vi.importActual<typeof import('../../api/git')>('../../api/git');
  return { ...actual, fetchStatus, fetchBranches, fetchLog, gitEventSource };
});

class QuietEventSource {
  onopen: (() => void) | null = null;
  onerror: (() => void) | null = null;
  closed = false;

  addEventListener(): void {}
  close(): void { this.closed = true; }
}

const NON_REPO = {
  isRepo: false,
  head: null,
  upstream: null,
  state: 'clean' as const,
  entries: [],
  counts: { staged: 0, changed: 0, untracked: 0, conflicted: 0 },
};

describe('GitPanel — plain directory (C47)', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    fetchStatus.mockReset().mockResolvedValue(NON_REPO);
    fetchBranches.mockReset();
    fetchLog.mockReset();
    gitEventSource.mockReset().mockReturnValue(new QuietEventSource());
  });

  it('renders a notice and none of the repository actions', async () => {
    const w = mount(GitPanel, { props: { projectId: 'plain-folder' } });
    await flushPromises();

    expect(w.find('.git__notice').text()).toContain('không phải git repository');
    expect(w.findComponent({ name: 'GitStatusList' }).exists()).toBe(false);
    expect(w.findComponent({ name: 'GitCommitBox' }).exists()).toBe(false);
    expect(w.find('.git__tabs').exists()).toBe(false);
    expect(fetchBranches).not.toHaveBeenCalled();
    expect(fetchLog).not.toHaveBeenCalled();

    w.unmount();
  });

  it('still opens a watcher so an external git init can be observed', async () => {
    const stream = new QuietEventSource();
    gitEventSource.mockReturnValue(stream);
    const w = mount(GitPanel, { props: { projectId: 'plain-folder' } });
    await flushPromises();

    expect(gitEventSource).toHaveBeenCalledWith('plain-folder');
    w.unmount();
    expect(stream.closed).toBe(true);
  });
});
