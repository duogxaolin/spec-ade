// @vitest-environment jsdom
//
// ChatPlan: the agent's plan as a checklist (SPEC-004 §5.1).
//
// A plan is a FULL snapshot, so the interesting cases are (a) that a replacement
// really replaces — a step the agent dropped must vanish, not linger — and (b) that an
// unknown ACP v2 `Other` status degrades to a neutral marker instead of throwing.

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { PlanEntryPayload, PlanPayload } from '../../api/acp';
import ChatPlan from './ChatPlan.vue';

function plan(...entries: PlanEntryPayload[]): PlanPayload {
  return { entries };
}

describe('ChatPlan — empty', () => {
  it('renders nothing at all for an empty plan', () => {
    // Not "renders an empty list": an empty box is a claim that a plan exists.
    const w = mount(ChatPlan, { props: { plan: plan() } });
    expect(w.find('.plan').exists()).toBe(false);
  });
});

describe('ChatPlan — counts', () => {
  it('counts completed over total', () => {
    const w = mount(ChatPlan, {
      props: {
        plan: plan(
          { content: 'a', status: 'completed' },
          { content: 'b', status: 'in_progress' },
          { content: 'c', status: 'pending' },
        ),
      },
    });
    expect(w.find('.plan__count').text()).toBe('1/3');
  });

  it('reads 0/N when nothing is done', () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'a' }, { content: 'b' }) },
    });
    expect(w.find('.plan__count').text()).toBe('0/2');
  });

  it('reads N/N when everything is done', () => {
    const w = mount(ChatPlan, {
      props: {
        plan: plan({ content: 'a', status: 'completed' }, { content: 'b', status: 'completed' }),
      },
    });
    expect(w.find('.plan__count').text()).toBe('2/2');
  });

  it('does not count an unknown status as completed', () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'a', status: 'something_new' }) },
    });
    expect(w.find('.plan__count').text()).toBe('0/1');
  });
});

describe('ChatPlan — glyphs', () => {
  const cases: Array<[string | undefined, string]> = [
    ['completed', '✓'],
    ['in_progress', '▸'],
    ['pending', '○'],
    [undefined, '·'],
    ['other_thing', '·'],
  ];

  for (const [status, expected] of cases) {
    it(`maps status ${String(status)} to ${expected}`, () => {
      const w = mount(ChatPlan, { props: { plan: plan({ content: 'x', status }) } });
      expect(w.find('.plan__glyph').text()).toBe(expected);
    });
  }

  it('hides glyphs from screen readers — the text is the content', () => {
    const w = mount(ChatPlan, { props: { plan: plan({ content: 'x', status: 'pending' }) } });
    expect(w.find('.plan__glyph').attributes('aria-hidden')).toBe('true');
  });
});

describe('ChatPlan — step text', () => {
  it('preserves order', () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'một' }, { content: 'hai' }, { content: 'ba' }) },
    });
    expect(w.findAll('.plan__text').map((n) => n.text())).toEqual(['một', 'hai', 'ba']);
  });

  it('renders content as plain text, so an underscore stays an underscore', () => {
    const w = mount(ChatPlan, { props: { plan: plan({ content: 'sửa my_file_name.rs' }) } });
    const text = w.find('.plan__text');
    expect(text.text()).toBe('sửa my_file_name.rs');
    expect(text.find('em').exists()).toBe(false);
  });

  it('does not interpret markdown emphasis in a step', () => {
    const w = mount(ChatPlan, { props: { plan: plan({ content: '**đậm**' }) } });
    expect(w.find('.plan__text').text()).toBe('**đậm**');
    expect(w.find('.plan__text').find('strong').exists()).toBe(false);
  });

  it('escapes HTML in a step rather than parsing it', () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: '<img src=x onerror=alert(1)>' }) },
    });
    const text = w.find('.plan__text');
    expect(text.text()).toBe('<img src=x onerror=alert(1)>');
    expect(text.find('img').exists()).toBe(false);
  });
});

describe('ChatPlan — status and priority classes', () => {
  it('marks each item with its status class', () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'a', status: 'in_progress' }) },
    });
    expect(w.find('.plan__item').classes()).toContain('plan__item--in_progress');
  });

  it('falls back to an unknown status class when status is absent', () => {
    const w = mount(ChatPlan, { props: { plan: plan({ content: 'a' }) } });
    expect(w.find('.plan__item').classes()).toContain('plan__item--unknown');
  });

  it('adds a priority class when priority is present', () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'a', status: 'pending', priority: 'high' }) },
    });
    expect(w.find('.plan__item').classes()).toContain('plan__item--p-high');
  });

  it('adds no priority class when priority is absent', () => {
    const w = mount(ChatPlan, { props: { plan: plan({ content: 'a' }) } });
    const withPriority = w.find('.plan__item').classes().filter((c) => c.startsWith('plan__item--p-'));
    expect(withPriority).toEqual([]);
  });
});

describe('ChatPlan — replacement semantics', () => {
  it('drops a step the agent removed', async () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'giữ' }, { content: 'bỏ' }) },
    });
    expect(w.findAll('.plan__item')).toHaveLength(2);

    await w.setProps({ plan: plan({ content: 'giữ' }) });
    expect(w.findAll('.plan__item')).toHaveLength(1);
    expect(w.find('.plan__text').text()).toBe('giữ');
  });

  it('reflects a status advance in both glyph and count', async () => {
    const w = mount(ChatPlan, {
      props: { plan: plan({ content: 'a', status: 'in_progress' }) },
    });
    expect(w.find('.plan__count').text()).toBe('0/1');

    await w.setProps({ plan: plan({ content: 'a', status: 'completed' }) });
    expect(w.find('.plan__count').text()).toBe('1/1');
    expect(w.find('.plan__glyph').text()).toBe('✓');
  });

  it('disappears when the plan is replaced by an empty one', async () => {
    const w = mount(ChatPlan, { props: { plan: plan({ content: 'a' }) } });
    await w.setProps({ plan: plan() });
    expect(w.find('.plan').exists()).toBe(false);
  });
});
