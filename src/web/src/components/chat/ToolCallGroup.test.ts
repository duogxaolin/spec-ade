// @vitest-environment jsdom

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { ToolCallPayload } from '../../api/acp';
import ToolCallGroup from './ToolCallGroup.vue';

// SPEC-004 B12-B15: the grouped row.

/** Build a `calls` table from `[id, status]` pairs. */
function table(...pairs: [string, string | undefined][]): Record<string, ToolCallPayload> {
  const out: Record<string, ToolCallPayload> = {};
  for (const [id, status] of pairs) {
    out[id] = { toolCallId: id, title: `gọi ${id}`, kind: 'read', status };
  }
  return out;
}

describe('ToolCallGroup with one call', () => {
  const calls = table(['t1', 'completed']);

  it('shows no summary line — one call is its own row', () => {
    const wrapper = mount(ToolCallGroup, { props: { toolCallIds: ['t1'], calls } });
    expect(wrapper.find('.tg__summary').exists()).toBe(false);
    expect(wrapper.findAllComponents({ name: 'ToolCallCard' })).toHaveLength(1);
  });

  it('opens that call by default, so no click is needed to see it', () => {
    const wrapper = mount(ToolCallGroup, {
      props: {
        toolCallIds: ['t1'],
        calls: {
          t1: {
            ...calls['t1']!,
            content: [{ type: 'content', content: { type: 'text', text: 'nội dung' } }],
          },
        },
      },
    });
    expect(wrapper.find('.tc__body').exists()).toBe(true);
  });
});

describe('ToolCallGroup with several calls', () => {
  const calls = table(['t1', 'completed'], ['t2', 'completed'], ['t3', 'completed']);
  const ids = ['t1', 't2', 't3'];

  it('collapses to a summary showing the count', () => {
    const wrapper = mount(ToolCallGroup, { props: { toolCallIds: ids, calls } });
    expect(wrapper.find('.tg__text').text()).toBe('3 tool call');
    expect(wrapper.find('.tg__glyph').text()).toBe('✓');
    expect(wrapper.find('.tg__list').exists()).toBe(false);
    expect(wrapper.find('.tg__summary').attributes('aria-expanded')).toBe('false');
  });

  it('expands and collapses on click', async () => {
    const wrapper = mount(ToolCallGroup, { props: { toolCallIds: ids, calls } });
    await wrapper.find('.tg__summary').trigger('click');
    expect(wrapper.findAllComponents({ name: 'ToolCallCard' })).toHaveLength(3);
    await wrapper.find('.tg__summary').trigger('click');
    expect(wrapper.find('.tg__list').exists()).toBe(false);
  });

  // The rule that makes collapsing safe: a hidden error is the one outcome this
  // must never produce.
  it('expands itself when any call in the batch failed', () => {
    const wrapper = mount(ToolCallGroup, {
      props: { toolCallIds: ids, calls: table(['t1', 'completed'], ['t2', 'failed'], ['t3', 'completed']) },
    });
    expect(wrapper.find('.tg__list').exists()).toBe(true);
    expect(wrapper.find('.tg__summary').classes()).toContain('tg__summary--failed');
    expect(wrapper.find('.tg__glyph').text()).toBe('✕');
    expect(wrapper.find('.tg__text').text()).toContain('có lỗi');
  });

  it('reports a running batch, and running outranks completed', () => {
    const wrapper = mount(ToolCallGroup, {
      props: { toolCallIds: ids, calls: table(['t1', 'completed'], ['t2', 'in_progress'], ['t3', undefined]) },
    });
    expect(wrapper.find('.tg__glyph').text()).toBe('⋯');
    expect(wrapper.find('.tg__text').text()).toContain('đang chạy');
  });

  // Worst-first: a failure must win over a still-running sibling.
  it('reports failure even when another call is still running', () => {
    const wrapper = mount(ToolCallGroup, {
      props: { toolCallIds: ids, calls: table(['t1', 'in_progress'], ['t2', 'failed'], ['t3', 'completed']) },
    });
    expect(wrapper.find('.tg__glyph').text()).toBe('✕');
  });

  it('treats an absent status as pending, so the batch reads as running', () => {
    const wrapper = mount(ToolCallGroup, {
      props: { toolCallIds: ['t1', 't2'], calls: table(['t1', undefined], ['t2', undefined]) },
    });
    expect(wrapper.find('.tg__text').text()).toContain('đang chạy');
  });
});

describe('ToolCallGroup with missing payloads', () => {
  // A gap in the log can leave an id referenced with no payload behind it.
  it('ignores ids whose call never arrived', () => {
    const wrapper = mount(ToolCallGroup, {
      props: { toolCallIds: ['t1', 'ghost', 't2'], calls: table(['t1', 'completed'], ['t2', 'completed']) },
    });
    expect(wrapper.find('.tg__text').text()).toBe('2 tool call');
  });

  it('renders nothing but stays mounted when no payload has arrived', () => {
    const wrapper = mount(ToolCallGroup, { props: { toolCallIds: ['ghost'], calls: {} } });
    expect(wrapper.findAllComponents({ name: 'ToolCallCard' })).toHaveLength(0);
    expect(wrapper.find('.tg').exists()).toBe(true);
  });

  it('treats a single surviving call as a single call', () => {
    const wrapper = mount(ToolCallGroup, {
      props: { toolCallIds: ['t1', 'ghost'], calls: table(['t1', 'completed']) },
    });
    expect(wrapper.find('.tg__summary').exists()).toBe(false);
  });
});

describe('ToolCallGroup events', () => {
  it('forwards open-location from a card', async () => {
    const wrapper = mount(ToolCallGroup, {
      props: {
        toolCallIds: ['t1'],
        calls: {
          t1: {
            toolCallId: 't1',
            title: 'sửa file',
            status: 'completed',
            locations: [{ path: 'src/a.rs', line: 3 }],
          },
        },
      },
    });
    await wrapper.find('.tc__loc').trigger('click');
    expect(wrapper.emitted('open-location')).toEqual([[{ path: 'src/a.rs', line: 3 }]]);
  });
});
