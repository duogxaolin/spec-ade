// @vitest-environment jsdom

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { DiffContent } from '../../chat/toolContent';
import ToolDiff from './ToolDiff.vue';

// SPEC-004 B16-B18: the diff card. The diff maths is covered in chat/diff.test.ts;
// this is about what the card shows.

function diff(overrides: Partial<DiffContent> = {}): DiffContent {
  return { type: 'diff', path: 'src/server/main.rs', oldText: 'a\n', newText: 'b\n', ...overrides };
}

describe('ToolDiff', () => {
  it('shows the basename with the full path as a tooltip', () => {
    const wrapper = mount(ToolDiff, { props: { diff: diff() } });
    const name = wrapper.find('.diff__name');
    expect(name.text()).toBe('main.rs');
    expect(name.attributes('title')).toBe('src/server/main.rs');
  });

  it('shows added and removed counts', () => {
    const wrapper = mount(ToolDiff, {
      props: { diff: diff({ oldText: 'a\nb\n', newText: 'a\nB\nc\n' }) },
    });
    expect(wrapper.find('.diff__stat--add').text()).toBe('+2');
    expect(wrapper.find('.diff__stat--del').text()).toBe('−1');
  });

  it('badges a new file', () => {
    const wrapper = mount(ToolDiff, { props: { diff: diff({ oldText: null }) } });
    expect(wrapper.find('.diff__badge').text()).toBe('mới');
  });

  it('does not badge an edit as new', () => {
    const wrapper = mount(ToolDiff, { props: { diff: diff() } });
    expect(wrapper.text()).not.toContain('mới');
  });

  it('renders one row per line, with both gutters', () => {
    const wrapper = mount(ToolDiff, {
      props: { diff: diff({ oldText: 'a\nold\nc\n', newText: 'a\nnew\nc\n' }) },
    });
    const rows = wrapper.findAll('tr');
    expect(rows).toHaveLength(4);
    // Removal above addition, as every diff tool shows a changed line.
    expect(rows[1]!.classes()).toContain('diff__row--remove');
    expect(rows[2]!.classes()).toContain('diff__row--add');
  });

  it('leaves the gutter blank on the side a line does not exist', () => {
    const wrapper = mount(ToolDiff, {
      props: { diff: diff({ oldText: 'a\n', newText: 'a\nb\n' }) },
    });
    const addRow = wrapper.findAll('tr')[1]!;
    const gutters = addRow.findAll('.diff__gutter');
    expect(gutters[0]!.text()).toBe('');
    expect(gutters[1]!.text()).toBe('2');
  });

  // File contents are the least trustworthy text in the app, and `{{ }}` cannot
  // produce markup at all.
  it('escapes file contents rather than rendering them', () => {
    const wrapper = mount(ToolDiff, {
      props: { diff: diff({ oldText: '', newText: '<script>alert(1)</script>\n' }) },
    });
    expect(wrapper.find('script').exists()).toBe(false);
    expect(wrapper.find('.diff__code').text()).toContain('<script>');
  });

  it('preserves indentation in the rendered line', () => {
    const wrapper = mount(ToolDiff, {
      props: { diff: diff({ oldText: '', newText: '    indented\n' }) },
    });
    // `.text()` trims, so read textContent: the leading spaces must reach the DOM
    // for `white-space: pre` to show them.
    expect(wrapper.find('.diff__code').element.textContent).toBe('    indented');
  });
});

describe('ToolDiff collapsing', () => {
  /** A diff with `count` added lines. */
  function bigDiff(count: number): DiffContent {
    return diff({
      oldText: '',
      newText: `${Array.from({ length: count }, (_, i) => `line ${i}`).join('\n')}\n`,
    });
  }

  it('shows a short diff in full, with no expander', () => {
    const wrapper = mount(ToolDiff, { props: { diff: bigDiff(10) } });
    expect(wrapper.findAll('tr')).toHaveLength(10);
    expect(wrapper.find('.diff__more').exists()).toBe(false);
  });

  it('caps a long diff at 40 rows and offers the rest', () => {
    const wrapper = mount(ToolDiff, { props: { diff: bigDiff(100) } });
    expect(wrapper.findAll('tr')).toHaveLength(40);
    expect(wrapper.find('.diff__more').text()).toContain('60');
  });

  it('shows everything once expanded, and offers to collapse again', async () => {
    const wrapper = mount(ToolDiff, { props: { diff: bigDiff(100) } });
    await wrapper.find('.diff__more').trigger('click');
    expect(wrapper.findAll('tr')).toHaveLength(100);
    expect(wrapper.find('.diff__more').text()).toBe('Thu lại');
  });

  it('collapses back to 40 rows', async () => {
    const wrapper = mount(ToolDiff, { props: { diff: bigDiff(100) } });
    await wrapper.find('.diff__more').trigger('click');
    await wrapper.find('.diff__more').trigger('click');
    expect(wrapper.findAll('tr')).toHaveLength(40);
    expect(wrapper.find('.diff__more').text()).toContain('60');
  });

  it('shows exactly 40 rows with no expander at the boundary', () => {
    const wrapper = mount(ToolDiff, { props: { diff: bigDiff(40) } });
    expect(wrapper.findAll('tr')).toHaveLength(40);
    expect(wrapper.find('.diff__more').exists()).toBe(false);
  });

  // Saying "rút gọn" beats showing an unaligned diff as if it were exact.
  it('badges a truncated diff', () => {
    const wrapper = mount(ToolDiff, { props: { diff: bigDiff(2001) } });
    expect(wrapper.text()).toContain('rút gọn');
  });
});
