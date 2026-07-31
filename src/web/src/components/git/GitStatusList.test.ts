// @vitest-environment jsdom
//
// GitStatusList's rendering contract (C42, C46): four semantic groups rather
// than one flat porcelain list, and no empty headers pretending there is work.

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { StatusEntry } from '../../api/git';
import GitStatusList from './GitStatusList.vue';

function entry(path: string, patch: Partial<StatusEntry>): StatusEntry {
  return {
    path,
    origPath: null,
    index: 'none',
    worktree: 'none',
    conflicted: false,
    staged: false,
    ...patch,
  };
}

const ALL_GROUPS: StatusEntry[] = [
  entry('conflict.txt', { index: 'modified', worktree: 'modified', conflicted: true, staged: true }),
  // One MM path deliberately appears twice: index and worktree are different
  // versions with different available actions (C42).
  entry('both.txt', { index: 'modified', worktree: 'modified', staged: true }),
  entry('new.txt', { worktree: 'new' }),
];

describe('GitStatusList — groups (C46)', () => {
  it('renders all four groups in semantic order with their counts', () => {
    const w = mount(GitStatusList, { props: { entries: ALL_GROUPS } });

    expect(w.findAll('.status__title').map((node) => node.text())).toEqual([
      'Xung đột',
      'Đã stage',
      'Thay đổi',
      'Chưa theo dõi',
    ]);
    expect(w.findAll('.status__count').map((node) => node.text())).toEqual(['1', '1', '1', '1']);
  });

  it('renders an MM path in both Staged and Changed, with unique row keys', () => {
    const w = mount(GitStatusList, { props: { entries: ALL_GROUPS } });
    const both = w.findAll('.status__name').filter((node) => node.text() === 'both.txt');

    expect(both).toHaveLength(2);
    expect(w.findAll('.status__row')).toHaveLength(4);
  });

  it('omits empty groups instead of rendering zero-count headers', async () => {
    const w = mount(GitStatusList, {
      props: { entries: [entry('only.txt', { worktree: 'modified' })] },
    });

    expect(w.findAll('.status__title').map((node) => node.text())).toEqual(['Thay đổi']);
    expect(w.text()).not.toContain('Đã stage');
    expect(w.text()).not.toContain('Chưa theo dõi');
    expect(w.text()).not.toContain('Xung đột');

    await w.setProps({ entries: [] });
    expect(w.findAll('.status__group')).toHaveLength(0);
    expect(w.find('.status__clean').text()).toBe('Không có thay đổi nào.');
  });
});
