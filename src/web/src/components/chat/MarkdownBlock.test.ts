// @vitest-environment jsdom

import { mount } from '@vue/test-utils';
import { afterEach, describe, expect, it, vi } from 'vitest';

import MarkdownBlock from './MarkdownBlock.vue';

// SPEC-004 B7-B11, B28: the streaming render path, asserted on a mounted component.
//
// Real timers here, not fake ones: the component's render pipeline crosses a
// setTimeout AND a microtask (`Promise.resolve().then(enhance)`), and fake timers
// only control the first. A short real wait is the honest way to observe the
// settled DOM.

/** Wait for the debounce window plus the enhance microtask to settle. */
async function settle(ms = 80): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

afterEach(() => vi.restoreAllMocks());

describe('MarkdownBlock', () => {
  it('renders the first chunk immediately, without waiting for the debounce', () => {
    const wrapper = mount(MarkdownBlock, { props: { source: 'xin chào' } });
    // No await: the first render is synchronous by design (§5.3).
    expect(wrapper.find('.md__body').html()).toContain('xin chào');
  });

  it('renders markdown structure', () => {
    const wrapper = mount(MarkdownBlock, { props: { source: '## Đầu đề\n\n- một\n- hai' } });
    expect(wrapper.find('h2').exists()).toBe(true);
    expect(wrapper.findAll('li')).toHaveLength(2);
  });

  it('shows a cursor while streaming and hides it after', async () => {
    const wrapper = mount(MarkdownBlock, { props: { source: 'đang gõ', streaming: true } });
    expect(wrapper.find('.md__cursor').exists()).toBe(true);

    await wrapper.setProps({ streaming: false });
    expect(wrapper.find('.md__cursor').exists()).toBe(false);
  });

  // The core of §5.3: an open fence is shown as an escaped <pre>, never handed to
  // markdown-it, so the block does not flip shape when the fence closes.
  it('shows an unterminated fence as a plain escaped pre', async () => {
    const wrapper = mount(MarkdownBlock, {
      props: { source: 'Đây:\n\n```rust\nfn main() {', streaming: true },
    });
    await settle();

    const streamingFence = wrapper.find('.md__streaming-fence');
    expect(streamingFence.exists()).toBe(true);
    expect(streamingFence.text()).toContain('fn main() {');
    // The prose half rendered as markdown; the fence body is not inside it.
    expect(wrapper.find('.md__body').text()).toContain('Đây:');
    expect(wrapper.find('.md__body').text()).not.toContain('fn main');
  });

  it('moves the fence into the highlighted body once it closes', async () => {
    const wrapper = mount(MarkdownBlock, {
      props: { source: '```rust\nfn main() {}', streaming: true },
    });
    await settle();
    expect(wrapper.find('.md__streaming-fence').exists()).toBe(true);

    await wrapper.setProps({ source: '```rust\nfn main() {}\n```', streaming: false });
    await settle();

    expect(wrapper.find('.md__streaming-fence').exists()).toBe(false);
    expect(wrapper.find('.md__body code.language-rust').exists()).toBe(true);
  });

  it('escapes markup inside a streaming fence', async () => {
    const wrapper = mount(MarkdownBlock, {
      props: { source: '```html\n<script>alert(1)</script>', streaming: true },
    });
    await settle();
    // `{{ }}` cannot produce an element at all.
    expect(wrapper.find('.md__streaming-fence script').exists()).toBe(false);
    expect(wrapper.find('.md__streaming-fence').text()).toContain('<script>');
  });

  it('coalesces a burst of chunks instead of rendering each one', async () => {
    const wrapper = mount(MarkdownBlock, { props: { source: 'a', streaming: true } });
    for (const text of ['ab', 'abc', 'abcd']) {
      await wrapper.setProps({ source: text });
    }
    // Mid-burst the DOM may still show an earlier state; after the window it must
    // show the newest text.
    await settle();
    expect(wrapper.find('.md__body').text()).toContain('abcd');
  });

  it('flushes the final chunk when the turn ends', async () => {
    const wrapper = mount(MarkdownBlock, { props: { source: 'một', streaming: true } });
    await wrapper.setProps({ source: 'một hai ba' });
    // No wait for the timer: ending the turn must render synchronously.
    await wrapper.setProps({ streaming: false });
    expect(wrapper.find('.md__body').text()).toContain('một hai ba');
  });

  it('never emits a script element for an XSS payload', async () => {
    const wrapper = mount(MarkdownBlock, {
      props: { source: '<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>' },
    });
    await settle();
    expect(wrapper.find('script').exists()).toBe(false);
    expect(wrapper.find('img').exists()).toBe(false);
  });

  it('renders math in prose', async () => {
    const wrapper = mount(MarkdownBlock, { props: { source: 'cho $x^2$ nhé' } });
    await settle();
    expect(wrapper.find('.katex').exists()).toBe(true);
  });

  it('does not render math inside a code fence', async () => {
    const wrapper = mount(MarkdownBlock, { props: { source: '```bash\necho $PATH\n```' } });
    await settle();
    expect(wrapper.find('.katex').exists()).toBe(false);
    expect(wrapper.find('code').text()).toContain('$PATH');
  });

  // Under jsdom mermaid cannot draw (no getBBox), so the fence must survive as
  // readable source. That is the same fallback a real browser takes on a diagram
  // that fails to parse.
  it('leaves a mermaid fence readable when the diagram cannot be drawn', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const wrapper = mount(MarkdownBlock, {
      props: { source: '```mermaid\ngraph TD; A-->B;\n```' },
    });
    await settle(400);
    const text = wrapper.text();
    expect(text).toContain('graph TD');
  });

  it('does not retry a failed mermaid render on the next chunk', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const wrapper = mount(MarkdownBlock, {
      props: { source: '```mermaid\ngraph TD; A-->B;\n```', streaming: true },
    });
    await settle(400);
    const pre = wrapper.find('pre');
    // The marker is set before the result is checked, precisely so a broken
    // diagram is not re-parsed on every subsequent chunk.
    expect(pre.attributes('data-mermaid-done')).toBe('1');
  });

  it('renders an empty source without throwing', () => {
    const wrapper = mount(MarkdownBlock, { props: { source: '' } });
    expect(wrapper.find('.md__body').text()).toBe('');
  });

  it('cleans up its timer on unmount', async () => {
    // A pending render firing after unmount would touch a torn-down component, and
    // Vue reports that on console.error / console.warn. Silence there is the
    // assertion.
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

    const wrapper = mount(MarkdownBlock, { props: { source: 'a', streaming: true } });
    await wrapper.setProps({ source: 'ab' });
    wrapper.unmount();
    await settle();

    expect(error).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
  });
});
