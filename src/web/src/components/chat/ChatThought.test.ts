// @vitest-environment jsdom
//
// ChatThought: collapsed-by-default reasoning ([SPEC-004 INVENTED-2]).
//
// The load-bearing behaviours are the default-closed state and the preview, because
// both exist to stop reasoning from burying the answer. A regression that opens by
// default would still pass a "does it render the text" test, so the assertions here
// are about visibility, not presence.

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import ChatThought from './ChatThought.vue';

/** MarkdownBlock crosses a timer and a microtask; the body's presence is what we assert. */
function mountThought(props: { text: string; streaming?: boolean }) {
  return mount(ChatThought, { props });
}

describe('ChatThought — collapsed state', () => {
  it('starts closed: no body is rendered', () => {
    const w = mountThought({ text: 'suy luận dài' });
    expect(w.find('.th__body').exists()).toBe(false);
  });

  it('reports the closed state to assistive tech via aria-expanded', () => {
    const w = mountThought({ text: 'suy luận dài' });
    expect(w.find('.th__toggle').attributes('aria-expanded')).toBe('false');
  });

  it('shows a closed chevron', () => {
    const w = mountThought({ text: 'x' });
    expect(w.find('.th__chevron').text()).toBe('▸');
  });

  it('hides the chevron from screen readers — it is redundant with aria-expanded', () => {
    const w = mountThought({ text: 'x' });
    expect(w.find('.th__chevron').attributes('aria-hidden')).toBe('true');
  });
});

describe('ChatThought — expanding', () => {
  it('renders the body after a click', async () => {
    const w = mountThought({ text: 'nội dung suy nghĩ' });
    await w.find('.th__toggle').trigger('click');
    expect(w.find('.th__body').exists()).toBe(true);
  });

  it('flips aria-expanded to true', async () => {
    const w = mountThought({ text: 'x' });
    await w.find('.th__toggle').trigger('click');
    expect(w.find('.th__toggle').attributes('aria-expanded')).toBe('true');
  });

  it('swaps the chevron to the open glyph', async () => {
    const w = mountThought({ text: 'x' });
    await w.find('.th__toggle').trigger('click');
    expect(w.find('.th__chevron').text()).toBe('▾');
  });

  it('drops the preview once open — the full text is already visible', async () => {
    const w = mountThought({ text: 'dòng đầu tiên' });
    expect(w.find('.th__preview').exists()).toBe(true);
    await w.find('.th__toggle').trigger('click');
    expect(w.find('.th__preview').exists()).toBe(false);
  });

  it('collapses again on a second click', async () => {
    const w = mountThought({ text: 'x' });
    await w.find('.th__toggle').trigger('click');
    await w.find('.th__toggle').trigger('click');
    expect(w.find('.th__body').exists()).toBe(false);
    expect(w.find('.th__preview').exists()).toBe(true);
  });

  it('passes the text through to a MarkdownBlock, not raw', async () => {
    const w = mountThought({ text: '# tiêu đề' });
    await w.find('.th__toggle').trigger('click');
    // The child is a real MarkdownBlock; its own suite covers rendering. Here we
    // only need that the body delegates rather than dumping the string.
    expect(w.findComponent({ name: 'MarkdownBlock' }).exists()).toBe(true);
  });
});

describe('ChatThought — preview', () => {
  it('uses the first line only', () => {
    const w = mountThought({ text: 'dòng một\ndòng hai\ndòng ba' });
    expect(w.find('.th__preview').text()).toBe('dòng một');
  });

  it('trims leading blank lines before picking the first line', () => {
    const w = mountThought({ text: '\n\n  thực sự là dòng đầu\nsau' });
    expect(w.find('.th__preview').text()).toBe('thực sự là dòng đầu');
  });

  it('keeps an 80-char line whole', () => {
    const line = 'a'.repeat(80);
    const w = mountThought({ text: line });
    expect(w.find('.th__preview').text()).toBe(line);
  });

  it('truncates at 81 chars and marks the cut with an ellipsis', () => {
    const line = 'b'.repeat(81);
    const w = mountThought({ text: line });
    expect(w.find('.th__preview').text()).toBe(`${'b'.repeat(80)}…`);
  });

  it('omits the preview entirely for empty text', () => {
    const w = mountThought({ text: '   \n  ' });
    expect(w.find('.th__preview').exists()).toBe(false);
  });

  it('shows plan text literally — the preview must not interpret markdown', () => {
    const w = mountThought({ text: '**không in đậm**' });
    expect(w.find('.th__preview').text()).toBe('**không in đậm**');
    expect(w.find('.th__preview').find('strong').exists()).toBe(false);
  });

  it('recomputes when text grows during streaming', async () => {
    const w = mountThought({ text: 'bắt', streaming: true });
    expect(w.find('.th__preview').text()).toBe('bắt');
    await w.setProps({ text: 'bắt đầu suy nghĩ' });
    expect(w.find('.th__preview').text()).toBe('bắt đầu suy nghĩ');
  });
});

describe('ChatThought — streaming label', () => {
  it('says it is still thinking while chunks arrive', () => {
    const w = mountThought({ text: 'x', streaming: true });
    expect(w.find('.th__label').text()).toBe('Đang suy nghĩ…');
  });

  it('settles to the past-tense label when streaming stops', async () => {
    const w = mountThought({ text: 'x', streaming: true });
    await w.setProps({ streaming: false });
    expect(w.find('.th__label').text()).toBe('Suy nghĩ');
  });

  it('treats an absent streaming prop as not streaming', () => {
    const w = mountThought({ text: 'x' });
    expect(w.find('.th__label').text()).toBe('Suy nghĩ');
  });

  it('keeps the open/closed state across a streaming change', async () => {
    const w = mountThought({ text: 'x', streaming: true });
    await w.find('.th__toggle').trigger('click');
    await w.setProps({ streaming: false });
    expect(w.find('.th__body').exists()).toBe(true);
  });
});
