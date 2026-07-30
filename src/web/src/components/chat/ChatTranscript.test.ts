// @vitest-environment jsdom
//
// ChatTranscript: row dispatch, plan placement, and follow-the-bottom scrolling
// (SPEC-004 §5.1, §5.6).
//
// jsdom does no layout, so `scrollHeight`/`clientHeight` are 0 unless we define them.
// Every scroll test here therefore stubs those three numbers on the scroller element
// explicitly — which is honest about what is being tested: the DECISION logic
// ("am I at the bottom?"), not the browser's scrolling.

import { mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import { nextTick } from 'vue';

import type { ToolCallPayload } from '../../api/acp';
import { createSessionView, type SessionView, type TranscriptEntry } from '../../stores/acpSession';
import ChatTranscript from './ChatTranscript.vue';

function view(patch: Partial<SessionView> = {}): SessionView {
  return { ...createSessionView(), ...patch };
}

function toolCall(id: string, patch: Partial<ToolCallPayload> = {}): ToolCallPayload {
  return { toolCallId: id, title: id, ...patch } as ToolCallPayload;
}

function mountTranscript(v: SessionView) {
  return mount(ChatTranscript, { props: { view: v } });
}

/**
 * Give the scroller fake geometry.
 *
 * `scrollTop` needs a real backing value because the component both reads and writes
 * it; the other two are read-only in the component, so plain getters suffice.
 */
function fakeGeometry(
  el: HTMLElement,
  geo: { scrollHeight: number; clientHeight: number; scrollTop: number },
): { get scrollTop(): number } {
  let top = geo.scrollTop;
  Object.defineProperty(el, 'scrollHeight', { configurable: true, get: () => geo.scrollHeight });
  Object.defineProperty(el, 'clientHeight', { configurable: true, get: () => geo.clientHeight });
  Object.defineProperty(el, 'scrollTop', {
    configurable: true,
    get: () => top,
    set: (v: number) => {
      top = v;
    },
  });
  return {
    get scrollTop() {
      return top;
    },
  };
}

/**
 * Wait for a pending `scrollToBottomIfFollowing` to complete.
 *
 * Two ticks: one for the watcher/DOM update that triggers it, one for the
 * `await nextTick()` inside it before it touches `scrollTop`.
 */
async function scrollSettled(): Promise<void> {
  await nextTick();
  await nextTick();
}

describe('ChatTranscript — row dispatch', () => {
  it('renders a message entry as a MarkdownBlock', () => {
    const w = mountTranscript(
      view({ entries: [{ kind: 'message', seq: 1, text: 'xin chào' }] }),
    );
    expect(w.findComponent({ name: 'MarkdownBlock' }).exists()).toBe(true);
  });

  it('renders a thought entry as a ChatThought', () => {
    const w = mountTranscript(view({ entries: [{ kind: 'thought', seq: 1, text: 'hmm' }] }));
    expect(w.findComponent({ name: 'ChatThought' }).exists()).toBe(true);
  });

  it('renders a tool entry as a ToolCallGroup', () => {
    const w = mountTranscript(
      view({
        entries: [{ kind: 'tool', seq: 1, toolCallId: 'tc-1' }],
        toolCalls: { 'tc-1': toolCall('tc-1') },
      }),
    );
    expect(w.findComponent({ name: 'ToolCallGroup' }).exists()).toBe(true);
  });

  it('collapses consecutive tool entries into one group', () => {
    const w = mountTranscript(
      view({
        entries: [
          { kind: 'tool', seq: 1, toolCallId: 'tc-1' },
          { kind: 'tool', seq: 2, toolCallId: 'tc-2' },
        ],
        toolCalls: { 'tc-1': toolCall('tc-1'), 'tc-2': toolCall('tc-2') },
      }),
    );
    expect(w.findAllComponents({ name: 'ToolCallGroup' })).toHaveLength(1);
  });

  it('renders a turn_end label', () => {
    const w = mountTranscript(
      view({
        entries: [{ kind: 'turn_end', seq: 1, stopReason: 'end_turn', label: '— xong lượt —' }],
      }),
    );
    expect(w.find('.ct__meta').text()).toBe('— xong lượt —');
  });

  it('falls back to a generic turn_end label when the label is empty', () => {
    const w = mountTranscript(
      view({ entries: [{ kind: 'turn_end', seq: 1, stopReason: 'end_turn', label: '' }] }),
    );
    expect(w.find('.ct__meta').text()).toBe('— hết lượt —');
  });

  it('names the missing range for a gap entry', () => {
    const w = mountTranscript(view({ entries: [{ kind: 'gap', seq: 5, fromSeq: 2 }] }));
    expect(w.find('.ct__meta').text()).toBe('— thiếu lịch sử trước seq 2 —');
  });

  it('renders a notice entry', () => {
    const w = mountTranscript(
      view({ entries: [{ kind: 'notice', seq: 1, text: 'agent đã ngắt kết nối' }] }),
    );
    expect(w.find('.ct__meta').text()).toBe('agent đã ngắt kết nối');
  });

  it('renders rows in arrival order', () => {
    const entries: TranscriptEntry[] = [
      { kind: 'notice', seq: 1, text: 'một' },
      { kind: 'notice', seq: 2, text: 'hai' },
      { kind: 'notice', seq: 3, text: 'ba' },
    ];
    const w = mountTranscript(view({ entries }));
    expect(w.findAll('.ct__meta').map((n) => n.text())).toEqual(['một', 'hai', 'ba']);
  });

  it('renders nothing in the rows track for an empty transcript', () => {
    const w = mountTranscript(view());
    expect(w.find('.ct__rows').element.children).toHaveLength(0);
  });
});

describe('ChatTranscript — gap notice', () => {
  it('warns when history was pruned', () => {
    const w = mountTranscript(view({ hasGap: true }));
    expect(w.find('.ct__gap').text()).toBe('Một phần lịch sử đã bị server xoá.');
  });

  it('stays silent when the log is complete', () => {
    const w = mountTranscript(view({ hasGap: false }));
    expect(w.find('.ct__gap').exists()).toBe(false);
  });

  it('lives inside the scroller, so it scrolls with the history it describes', () => {
    const w = mountTranscript(view({ hasGap: true }));
    expect(w.find('.ct__scroll').find('.ct__gap').exists()).toBe(true);
  });
});

describe('ChatTranscript — plan', () => {
  it('renders the plan when present', () => {
    const w = mountTranscript(view({ plan: { entries: [{ content: 'bước 1' }] } }));
    expect(w.findComponent({ name: 'ChatPlan' }).exists()).toBe(true);
  });

  it('omits the plan when null', () => {
    const w = mountTranscript(view({ plan: null }));
    expect(w.findComponent({ name: 'ChatPlan' }).exists()).toBe(false);
  });

  it('places the plan inside the scroller above the rows', () => {
    const w = mountTranscript(
      view({
        plan: { entries: [{ content: 'bước 1' }] },
        entries: [{ kind: 'notice', seq: 1, text: 'x' }],
      }),
    );
    const scroll = w.find('.ct__scroll').element;
    const planEl = w.find('.plan').element;
    const rowsEl = w.find('.ct__rows').element;
    expect(scroll.contains(planEl)).toBe(true);
    // DOCUMENT_POSITION_FOLLOWING: rows come after the plan.
    expect(planEl.compareDocumentPosition(rowsEl) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });
});

describe('ChatTranscript — streaming cursor', () => {
  it('marks the last row as streaming while the turn is active', () => {
    const w = mountTranscript(
      view({
        turnActive: true,
        entries: [
          { kind: 'message', seq: 1, text: 'cũ' },
          { kind: 'message', seq: 2, text: 'đang chạy' },
        ],
      }),
    );
    const blocks = w.findAllComponents({ name: 'MarkdownBlock' });
    expect(blocks[0]!.props('streaming')).toBe(false);
    expect(blocks[1]!.props('streaming')).toBe(true);
  });

  it('marks nothing as streaming once the turn completes', () => {
    const w = mountTranscript(
      view({ turnActive: false, entries: [{ kind: 'message', seq: 1, text: 'xong' }] }),
    );
    expect(w.findComponent({ name: 'MarkdownBlock' }).props('streaming')).toBe(false);
  });

  it('streams a thought row when it is last', () => {
    const w = mountTranscript(
      view({ turnActive: true, entries: [{ kind: 'thought', seq: 1, text: 'nghĩ' }] }),
    );
    expect(w.findComponent({ name: 'ChatThought' }).props('streaming')).toBe(true);
  });

  it('never puts the cursor in two places at once', () => {
    const w = mountTranscript(
      view({
        turnActive: true,
        entries: [
          { kind: 'message', seq: 1, text: 'a' },
          { kind: 'thought', seq: 2, text: 'b' },
          { kind: 'message', seq: 3, text: 'c' },
        ],
      }),
    );
    const streaming = [
      ...w.findAllComponents({ name: 'MarkdownBlock' }).map((c) => c.props('streaming')),
      ...w.findAllComponents({ name: 'ChatThought' }).map((c) => c.props('streaming')),
    ].filter(Boolean);
    expect(streaming).toHaveLength(1);
  });
});

describe('ChatTranscript — scroll following', () => {
  it('hides the jump button while following', () => {
    const w = mountTranscript(view());
    expect(w.find('.ct__jump').exists()).toBe(false);
  });

  it('shows the jump button once the user scrolls up', async () => {
    const w = mountTranscript(view({ entries: [{ kind: 'notice', seq: 1, text: 'x' }] }));
    const scroll = w.find('.ct__scroll');
    fakeGeometry(scroll.element as HTMLElement, {
      scrollHeight: 1000,
      clientHeight: 200,
      scrollTop: 0,
    });
    await scroll.trigger('scroll');
    expect(w.find('.ct__jump').exists()).toBe(true);
  });

  it('stays following within the 32 px slack', async () => {
    const w = mountTranscript(view());
    const scroll = w.find('.ct__scroll');
    // 1000 - 768 - 200 = 32, exactly at the tolerance.
    fakeGeometry(scroll.element as HTMLElement, {
      scrollHeight: 1000,
      clientHeight: 200,
      scrollTop: 768,
    });
    await scroll.trigger('scroll');
    expect(w.find('.ct__jump').exists()).toBe(false);
  });

  it('detaches one pixel past the slack', async () => {
    const w = mountTranscript(view());
    const scroll = w.find('.ct__scroll');
    // 1000 - 767 - 200 = 33.
    fakeGeometry(scroll.element as HTMLElement, {
      scrollHeight: 1000,
      clientHeight: 200,
      scrollTop: 767,
    });
    await scroll.trigger('scroll');
    expect(w.find('.ct__jump').exists()).toBe(true);
  });

  it('follows a new entry to the bottom while attached', async () => {
    const v = view({ entries: [{ kind: 'notice', seq: 1, text: 'một' }] });
    const w = mountTranscript(v);
    const probe = fakeGeometry(w.find('.ct__scroll').element as HTMLElement, {
      scrollHeight: 1000,
      clientHeight: 200,
      scrollTop: 0,
    });

    v.entries.push({ kind: 'notice', seq: 2, text: 'hai' });
    await scrollSettled();

    expect(probe.scrollTop).toBe(1000);
  });

  it('does NOT yank the view when the user has scrolled up', async () => {
    // The whole point of the feature: someone re-reading an earlier answer keeps
    // their place while the agent is still talking.
    const v = view({ entries: [{ kind: 'notice', seq: 1, text: 'một' }] });
    const w = mountTranscript(v);
    const el = w.find('.ct__scroll').element as HTMLElement;
    const probe = fakeGeometry(el, { scrollHeight: 1000, clientHeight: 200, scrollTop: 0 });
    // Let the mount-time scroll land first. It awaits `nextTick`, so without this
    // it would still be in flight and would overwrite the scroll position we set
    // below — in a browser it has always finished before a user can scroll.
    await scrollSettled();

    el.scrollTop = 100;
    await w.find('.ct__scroll').trigger('scroll');

    v.entries.push({ kind: 'notice', seq: 2, text: 'hai' });
    await scrollSettled();

    expect(probe.scrollTop).toBe(100);
  });

  it('reattaches and scrolls to the bottom when the jump button is clicked', async () => {
    const w = mountTranscript(view({ entries: [{ kind: 'notice', seq: 1, text: 'x' }] }));
    const el = w.find('.ct__scroll').element as HTMLElement;
    const probe = fakeGeometry(el, { scrollHeight: 1000, clientHeight: 200, scrollTop: 0 });
    await scrollSettled();

    el.scrollTop = 100;
    await w.find('.ct__scroll').trigger('scroll');
    expect(w.find('.ct__jump').exists()).toBe(true);

    await w.find('.ct__jump').trigger('click');
    await scrollSettled();

    // 1000, not the 100 we parked at: the click really re-followed.
    expect(probe.scrollTop).toBe(1000);
    expect(w.find('.ct__jump').exists()).toBe(false);
  });

  it('follows an in-place mutation of the trailing block, not just a push', async () => {
    // SPEC-003's fold appends chunks into the existing entry, so identity never
    // changes — the deep watcher is what makes this work.
    const v = view({ entries: [{ kind: 'message', seq: 1, text: 'bắt' }] });
    const w = mountTranscript(v);
    const probe = fakeGeometry(w.find('.ct__scroll').element as HTMLElement, {
      scrollHeight: 500,
      clientHeight: 200,
      scrollTop: 0,
    });

    (v.entries[0] as { text: string }).text = 'bắt đầu trả lời';
    await scrollSettled();

    expect(probe.scrollTop).toBe(500);
  });

  it('lands at the bottom on mount, for a replayed session', async () => {
    const w = mountTranscript(
      view({ entries: [{ kind: 'notice', seq: 1, text: 'lịch sử cũ' }] }),
    );
    const probe = fakeGeometry(w.find('.ct__scroll').element as HTMLElement, {
      scrollHeight: 800,
      clientHeight: 200,
      scrollTop: 0,
    });
    // `onMounted` awaits `nextTick` before scrolling, so the geometry stub above
    // is in place in time.
    await scrollSettled();
    expect(probe.scrollTop).toBe(800);
  });

  it('survives a scroll event with no geometry at all', async () => {
    // jsdom's default: every number is 0. 0 - 0 - 0 <= 32, so this is "at the
    // bottom" and must not throw or flip the button on.
    const w = mountTranscript(view());
    await w.find('.ct__scroll').trigger('scroll');
    expect(w.find('.ct__jump').exists()).toBe(false);
  });
});

describe('ChatTranscript — open-location', () => {
  it('re-emits open-location from a tool group unchanged', async () => {
    const w = mountTranscript(
      view({
        entries: [{ kind: 'tool', seq: 1, toolCallId: 'tc-1' }],
        toolCalls: { 'tc-1': toolCall('tc-1') },
      }),
    );
    const payload = { path: 'src/main.rs', line: 42 };
    w.findComponent({ name: 'ToolCallGroup' }).vm.$emit('open-location', payload);
    await nextTick();
    expect(w.emitted('open-location')).toEqual([[payload]]);
  });

  it('passes a null line through rather than substituting a number', async () => {
    const w = mountTranscript(
      view({
        entries: [{ kind: 'tool', seq: 1, toolCallId: 'tc-1' }],
        toolCalls: { 'tc-1': toolCall('tc-1') },
      }),
    );
    w.findComponent({ name: 'ToolCallGroup' }).vm.$emit('open-location', {
      path: 'a.txt',
      line: null,
    });
    await nextTick();
    expect(w.emitted('open-location')).toEqual([[{ path: 'a.txt', line: null }]]);
  });
});

describe('ChatTranscript — quiet operation', () => {
  it('mounts and unmounts without warnings', () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});

    const w = mountTranscript(
      view({
        hasGap: true,
        plan: { entries: [{ content: 'b1', status: 'completed' }] },
        turnActive: true,
        entries: [
          { kind: 'message', seq: 1, text: 'chào' },
          { kind: 'thought', seq: 2, text: 'nghĩ' },
          { kind: 'tool', seq: 3, toolCallId: 'tc-1' },
        ],
        toolCalls: { 'tc-1': toolCall('tc-1') },
      }),
    );
    w.unmount();

    expect(err).not.toHaveBeenCalled();
    expect(warn).not.toHaveBeenCalled();
    err.mockRestore();
    warn.mockRestore();
  });
});
