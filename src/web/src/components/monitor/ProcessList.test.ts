// @vitest-environment jsdom
//
// ProcessList's kill contract (D46): the first click arms, the second sends.
//
// This is the one rule in the panel where getting it wrong destroys something —
// `SIGKILL` on the wrong row loses whatever that process had unsaved, and the row
// under the cursor moves every 3s as the list re-sorts by CPU. So the test asserts
// on `emitted('kill')`, which is exactly "did an API call happen".

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { ProcessInfo } from '../../api/system';
import ProcessList from './ProcessList.vue';

function proc(patch: Partial<ProcessInfo> = {}): ProcessInfo {
  return {
    pid: 42,
    parentPid: 1,
    name: 'node',
    cmd: 'node server.js',
    cpu: 12.5,
    memory: 1048576,
    status: 'Run',
    runTimeSec: 90,
    user: 'me',
    ...patch,
  };
}

function mountList(processes: ProcessInfo[] = [proc()], props: Record<string, unknown> = {}) {
  return mount(ProcessList, {
    props: { processes, sort: 'cpu' as const, filter: '', ...props },
  });
}

/** The ⨯ / Term / Kill / Huỷ buttons of the first row, in DOM order. */
function actions(wrapper: ReturnType<typeof mountList>) {
  return wrapper.findAll('.plist__act');
}

describe('ProcessList kill confirmation (D46)', () => {
  it('does not emit kill on the first click', async () => {
    const wrapper = mountList();
    await actions(wrapper)[0].trigger('click');

    expect(wrapper.emitted('kill')).toBeUndefined();
    // The row now says what is about to die, naming it and its pid.
    expect(wrapper.text()).toContain('node');
    expect(wrapper.text()).toContain('42');
    expect(wrapper.find('.plist__confirm').exists()).toBe(true);
  });

  it('emits kill with SIGTERM only after confirming', async () => {
    const wrapper = mountList();
    await actions(wrapper)[0].trigger('click');
    const armed = actions(wrapper);
    // Term, Kill, Huỷ
    expect(armed).toHaveLength(3);

    await armed[0].trigger('click');
    expect(wrapper.emitted('kill')).toEqual([[42, 'term']]);
  });

  it('emits kill with SIGKILL from the explicit Kill button', async () => {
    const wrapper = mountList();
    await actions(wrapper)[0].trigger('click');
    await actions(wrapper)[1].trigger('click');
    expect(wrapper.emitted('kill')).toEqual([[42, 'kill']]);
  });

  it('cancelling disarms without emitting anything', async () => {
    const wrapper = mountList();
    await actions(wrapper)[0].trigger('click');
    await actions(wrapper)[2].trigger('click'); // Huỷ

    expect(wrapper.emitted('kill')).toBeUndefined();
    expect(wrapper.find('.plist__confirm').exists()).toBe(false);
    expect(actions(wrapper)).toHaveLength(1);
  });

  it('arming a second row disarms the first', async () => {
    const wrapper = mountList([proc(), proc({ pid: 7, name: 'zsh' })]);
    const rows = wrapper.findAll('.plist__row');

    await rows[0].find('.plist__act').trigger('click');
    await rows[1].find('.plist__act').trigger('click');

    // Only one confirmation is live, so a click cannot land on a stale one.
    expect(wrapper.findAll('.plist__confirm')).toHaveLength(1);
    expect(wrapper.find('.plist__confirm').text()).toContain('zsh');
    expect(wrapper.emitted('kill')).toBeUndefined();
  });

  it('the confirmation resets after a kill is sent', async () => {
    const wrapper = mountList();
    await actions(wrapper)[0].trigger('click');
    await actions(wrapper)[0].trigger('click');

    expect(wrapper.emitted('kill')).toHaveLength(1);
    expect(wrapper.find('.plist__confirm').exists()).toBe(false);
  });

  it('busy disables the confirmation buttons so a kill cannot double-send', async () => {
    const wrapper = mountList();
    await actions(wrapper)[0].trigger('click');
    // A kill is now in flight; the armed row must not fire a second signal.
    await wrapper.setProps({ busy: true });

    const armed = actions(wrapper);
    expect(armed[0].attributes('disabled')).toBeDefined();
    expect(armed[1].attributes('disabled')).toBeDefined();
    // Cancel stays live — it sends nothing.
    expect(armed[2].attributes('disabled')).toBeUndefined();
  });
});

describe('ProcessList rendering', () => {
  it('renders rows in the order given, without re-sorting', () => {
    const wrapper = mountList([
      proc({ pid: 1, name: 'a', cpu: 1 }),
      proc({ pid: 2, name: 'b', cpu: 90 }),
      proc({ pid: 3, name: 'c', cpu: 50 }),
    ]);
    // The server already applied `sort` + top-N; re-ordering here would show a
    // ranking that does not match what was selected from.
    const pids = wrapper.findAll('.plist__td--pid').map((td) => td.text());
    expect(pids).toEqual(['1', '2', '3']);
  });

  it('formats memory in binary units', () => {
    const wrapper = mountList([proc({ memory: 1073741824 })]);
    expect(wrapper.text()).toContain('1.0 GB');
  });

  it('emits sort changes rather than sorting itself', async () => {
    const wrapper = mountList();
    await wrapper.findAll('.plist__sort')[1].trigger('click');
    expect(wrapper.emitted('update:sort')).toEqual([['memory']]);
  });

  it('emits filter changes', async () => {
    const wrapper = mountList();
    const input = wrapper.find('.plist__filter');
    await input.setValue('node');
    expect(wrapper.emitted('update:filter')).toEqual([['node']]);
  });

  it('says so when the list is empty', () => {
    const wrapper = mountList([]);
    expect(wrapper.text()).toContain('Không có tiến trình nào khớp');
  });
});
