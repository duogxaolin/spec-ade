// @vitest-environment jsdom
//
// Checkout is the destructive edge of the branch menu (C48). A dirty tree must
// never put `force: true` on the first click; only the explicit second decision
// may emit it.

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { GitBranches } from '../../api/git';
import GitBranchMenu from './GitBranchMenu.vue';

const BRANCHES: GitBranches = {
  current: 'main',
  local: [
    { name: 'main', oid: '111', upstream: null, ahead: 0, behind: 0, current: true },
    { name: 'feature', oid: '222', upstream: null, ahead: 0, behind: 0, current: false },
  ],
  remote: [],
};

describe('GitBranchMenu — guarded checkout (C48)', () => {
  it('checks out a clean tree without force', async () => {
    const w = mount(GitBranchMenu, { props: { branches: BRANCHES, dirty: false } });
    await w.findAll('.branches__item button')[0]!.trigger('click');

    expect(w.emitted('checkout')).toEqual([['feature', false]]);
    expect(w.find('.branches__confirm').exists()).toBe(false);
  });

  it('does not emit checkout on the first click when dirty', async () => {
    const w = mount(GitBranchMenu, { props: { branches: BRANCHES, dirty: true } });
    await w.findAll('.branches__item button')[0]!.trigger('click');

    expect(w.emitted('checkout')).toBeUndefined();
    expect(w.find('.branches__confirm').attributes('role')).toBe('alertdialog');
    expect(w.find('.branches__warn').text()).toContain('feature');
  });

  it('emits force only after explicit confirmation', async () => {
    const w = mount(GitBranchMenu, { props: { branches: BRANCHES, dirty: true } });
    await w.findAll('.branches__item button')[0]!.trigger('click');
    await w.find('.branches__danger').trigger('click');

    expect(w.emitted('checkout')).toEqual([['feature', true]]);
    expect(w.find('.branches__confirm').exists()).toBe(false);
  });

  it('cancels without emitting or losing the menu', async () => {
    const w = mount(GitBranchMenu, { props: { branches: BRANCHES, dirty: true } });
    await w.findAll('.branches__item button')[0]!.trigger('click');
    await w.findAll('.branches__actions button')[1]!.trigger('click');

    expect(w.emitted('checkout')).toBeUndefined();
    expect(w.find('.branches__confirm').exists()).toBe(false);
    expect(w.text()).toContain('feature');
  });
});
