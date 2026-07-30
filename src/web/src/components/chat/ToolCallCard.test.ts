// @vitest-environment jsdom

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { ToolCallPayload } from '../../api/acp';
import ToolCallCard from './ToolCallCard.vue';

// SPEC-004 B19-B23: the tool-call card.

function call(overrides: Partial<ToolCallPayload> = {}): ToolCallPayload {
  return { toolCallId: 'tc-1', title: 'Đọc src/main.rs', kind: 'read', ...overrides };
}

describe('ToolCallCard header', () => {
  it('shows title, kind icon and status', () => {
    const wrapper = mount(ToolCallCard, { props: { call: call({ status: 'completed' }) } });
    expect(wrapper.find('.tc__title').text()).toBe('Đọc src/main.rs');
    expect(wrapper.find('.tc__status').text()).toBe('xong');
    expect(wrapper.find('.tc__glyph').text()).toBe('✓');
  });

  it('falls back to kind, then to the id, when there is no title', () => {
    expect(
      mount(ToolCallCard, { props: { call: { toolCallId: 'tc-9', kind: 'edit' } } })
        .find('.tc__title')
        .text(),
    ).toBe('edit');
    expect(
      mount(ToolCallCard, { props: { call: { toolCallId: 'tc-9' } } }).find('.tc__title').text(),
    ).toBe('tc-9');
  });

  // Absent status is pending, per the serde default — not "unknown".
  it('reads an absent status as pending', () => {
    const wrapper = mount(ToolCallCard, { props: { call: call() } });
    expect(wrapper.find('.tc__status').text()).toBe('đang chờ');
  });

  it('shows an unknown status verbatim and marks the card as other', () => {
    const wrapper = mount(ToolCallCard, { props: { call: call({ status: 'quantum' }) } });
    expect(wrapper.find('.tc__status').text()).toBe('quantum');
    expect(wrapper.classes()).toContain('tc--other');
  });

  it('disables the disclosure when there is nothing to show', () => {
    const wrapper = mount(ToolCallCard, { props: { call: call() } });
    expect(wrapper.find('.tc__head').attributes('disabled')).toBeDefined();
    expect(wrapper.find('.tc__chevron').exists()).toBe(false);
  });
});

describe('ToolCallCard disclosure', () => {
  const withBody = call({
    status: 'completed',
    content: [{ type: 'content', content: { type: 'text', text: 'nội dung' } }],
  });

  it('starts closed by default', () => {
    const wrapper = mount(ToolCallCard, { props: { call: withBody } });
    expect(wrapper.find('.tc__body').exists()).toBe(false);
    expect(wrapper.find('.tc__head').attributes('aria-expanded')).toBe('false');
  });

  it('starts open when asked', () => {
    const wrapper = mount(ToolCallCard, { props: { call: withBody, defaultOpen: true } });
    expect(wrapper.find('.tc__body').exists()).toBe(true);
  });

  it('toggles on click', async () => {
    const wrapper = mount(ToolCallCard, { props: { call: withBody } });
    await wrapper.find('.tc__head').trigger('click');
    expect(wrapper.find('.tc__body').exists()).toBe(true);
    await wrapper.find('.tc__head').trigger('click');
    expect(wrapper.find('.tc__body').exists()).toBe(false);
  });

  // An error the user has to hunt for is a hidden error.
  it('force-opens a failed call', () => {
    const wrapper = mount(ToolCallCard, {
      props: { call: { ...withBody, status: 'failed' } },
    });
    expect(wrapper.find('.tc__body').exists()).toBe(true);
    expect(wrapper.classes()).toContain('tc--failed');
  });
});

describe('ToolCallCard content', () => {
  it('renders a diff through ToolDiff', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({
          content: [{ type: 'diff', path: 'a.rs', oldText: 'a\n', newText: 'b\n' }],
        }),
      },
    });
    expect(wrapper.find('.diff').exists()).toBe(true);
    expect(wrapper.text()).toContain('a.rs');
  });

  it('names a terminal instead of rendering nothing', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({ content: [{ type: 'terminal', terminalId: 'term-7' }] }),
      },
    });
    expect(wrapper.text()).toContain('term-7');
  });

  it('renders a text block as markdown', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({ content: [{ type: 'content', content: { type: 'text', text: '**đậm**' } }] }),
      },
    });
    expect(wrapper.find('strong').exists()).toBe(true);
  });

  it('renders an allow-listed image as an attribute-bound data URL', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({
          content: [
            { type: 'content', content: { type: 'image', data: 'AAAA', mimeType: 'image/png' } },
          ],
        }),
      },
    });
    expect(wrapper.find('img.tc__image').attributes('src')).toBe('data:image/png;base64,AAAA');
  });

  // The allow-list is what keeps `data:text/html` out of an <img src>.
  it('refuses an image whose mime type is not an image', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({
          content: [
            {
              type: 'content',
              content: { type: 'image', data: 'PHN2Zz4=', mimeType: 'text/html' },
            },
          ],
        }),
      },
    });
    expect(wrapper.find('img').exists()).toBe(false);
    expect(wrapper.text()).toContain('chưa hỗ trợ');
  });

  it('refuses image/svg+xml, which can carry script', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({
          content: [
            {
              type: 'content',
              content: { type: 'image', data: 'PHN2Zz4=', mimeType: 'image/svg+xml' },
            },
          ],
        }),
      },
    });
    expect(wrapper.find('img').exists()).toBe(false);
  });

  it('hardens a resource_link and prefers its title as the label', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({
          content: [
            {
              type: 'content',
              content: {
                type: 'resource_link',
                uri: 'https://example.com/a',
                name: 'a',
                title: 'Tài liệu A',
              },
            },
          ],
        }),
      },
    });
    const link = wrapper.find('a.tc__link');
    expect(link.text()).toBe('Tài liệu A');
    expect(link.attributes('rel')).toBe('noopener noreferrer nofollow');
    expect(link.attributes('target')).toBe('_blank');
  });

  it('labels a content type it does not render rather than dropping it', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({ content: [{ type: 'content', content: { type: 'audio', data: 'x' } }] }),
      },
    });
    expect(wrapper.text()).toContain('audio');
  });

  it('labels an unknown top-level content tag', () => {
    const wrapper = mount(ToolCallCard, {
      props: { defaultOpen: true, call: call({ content: [{ type: 'hologram' }] }) },
    });
    expect(wrapper.text()).toContain('hologram');
  });
});

describe('ToolCallCard locations', () => {
  it('renders a chip per location, showing the basename and line', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({ locations: [{ path: 'src/server/main.rs', line: 42 }, { path: 'a.rs' }] }),
      },
    });
    const chips = wrapper.findAll('.tc__loc');
    expect(chips).toHaveLength(2);
    expect(chips[0]!.text()).toBe('main.rs:42');
    expect(chips[0]!.attributes('title')).toBe('src/server/main.rs');
    expect(chips[1]!.text()).toBe('a.rs');
  });

  it('emits the full path and line when a chip is clicked', async () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({ locations: [{ path: 'src/server/main.rs', line: 42 }] }),
      },
    });
    await wrapper.find('.tc__loc').trigger('click');
    expect(wrapper.emitted('open-location')).toEqual([[{ path: 'src/server/main.rs', line: 42 }]]);
  });

  it('emits a null line when the location has none', async () => {
    const wrapper = mount(ToolCallCard, {
      props: { defaultOpen: true, call: call({ locations: [{ path: 'a.rs' }] }) },
    });
    await wrapper.find('.tc__loc').trigger('click');
    expect(wrapper.emitted('open-location')![0]).toEqual([{ path: 'a.rs', line: null }]);
  });
});

describe('ToolCallCard rawOutput', () => {
  it('shows rawOutput only when there is no structured content', () => {
    const wrapper = mount(ToolCallCard, {
      props: { defaultOpen: true, call: call({ rawOutput: { ok: true } }) },
    });
    expect(wrapper.find('.tc__raw').text()).toContain('"ok": true');
  });

  it('prefers structured content over rawOutput', () => {
    const wrapper = mount(ToolCallCard, {
      props: {
        defaultOpen: true,
        call: call({
          rawOutput: { ok: true },
          content: [{ type: 'content', content: { type: 'text', text: 'kết quả' } }],
        }),
      },
    });
    expect(wrapper.find('.tc__raw').exists()).toBe(false);
  });

  it('escapes rawOutput rather than interpreting it', () => {
    const wrapper = mount(ToolCallCard, {
      props: { defaultOpen: true, call: call({ rawOutput: { html: '<script>alert(1)</script>' } }) },
    });
    expect(wrapper.find('.tc__raw script').exists()).toBe(false);
    expect(wrapper.find('.tc__raw').text()).toContain('<script>');
  });

  it('survives an unserializable rawOutput', () => {
    const cyclic: Record<string, unknown> = {};
    cyclic['self'] = cyclic;
    const wrapper = mount(ToolCallCard, {
      props: { defaultOpen: true, call: call({ rawOutput: cyclic }) },
    });
    expect(wrapper.find('.tc__raw').exists()).toBe(false);
  });
});
