// @vitest-environment jsdom
//
// PermissionDialog: the consent gate (SPEC-004 §5.1).
//
// This is the one component where a rendering bug has security consequences, so the
// assertions are deliberately strict about two things the source itself calls out:
//
//   1. `optionId` round-trips VERBATIM. The agent chose that string; re-deriving it
//      from the label, trimming it, or lower-casing it would grant a different
//      permission than the one the user clicked.
//   2. An unknown `kind` is NEVER styled as allow. A mis-coloured allow button is a
//      click the user did not mean to make.

import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';

import type { PermissionOptionView, ToolCallPatch } from '../../api/acp';
import PermissionDialog from './PermissionDialog.vue';

function call(patch: Partial<ToolCallPatch> = {}): ToolCallPatch {
  return { toolCallId: 'tc-1', ...patch } as ToolCallPatch;
}

function opt(
  optionId: string,
  name: string,
  kind: string,
): PermissionOptionView {
  return { optionId, name, kind };
}

const ALLOW = opt('allow-once-1', 'Cho phép một lần', 'allow_once');
const REJECT = opt('reject-once-1', 'Từ chối', 'reject_once');

function mountDialog(options: PermissionOptionView[], toolCall: ToolCallPatch = call()) {
  return mount(PermissionDialog, { props: { toolCall, options } });
}

describe('PermissionDialog — option identity', () => {
  it('emits the exact optionId string it was given', async () => {
    const weird = opt('  Allow-ONCE::v2/#7  ', 'Cho phép', 'allow_once');
    const w = mountDialog([weird]);
    await w.findAll('.perm__btn')[0]!.trigger('click');

    // Verbatim: no trim, no case change, no slug.
    expect(w.emitted('choose')).toEqual([['  Allow-ONCE::v2/#7  ']]);
  });

  it('emits the id of the button actually clicked, not the first one', async () => {
    const w = mountDialog([REJECT, ALLOW]);
    const buttons = w.findAll('.perm__btn');
    await buttons[1]!.trigger('click');
    expect(w.emitted('choose')).toEqual([['allow-once-1']]);
  });

  it('keeps ids distinct when two options share a label', async () => {
    const a = opt('id-a', 'Cho phép', 'allow_once');
    const b = opt('id-b', 'Cho phép', 'allow_always');
    const w = mountDialog([a, b]);
    await w.findAll('.perm__btn')[1]!.trigger('click');
    expect(w.emitted('choose')).toEqual([['id-b']]);
  });

  it('emits once per click, so a single consent is not counted twice', async () => {
    const w = mountDialog([ALLOW]);
    await w.findAll('.perm__btn')[0]!.trigger('click');
    expect(w.emitted('choose')).toHaveLength(1);
  });
});

describe('PermissionDialog — kind styling', () => {
  it('styles an allow_once option as allow', () => {
    const w = mountDialog([ALLOW]);
    expect(w.findAll('.perm__btn')[0]!.classes()).toContain('perm__btn--allow');
  });

  it('styles allow_always as allow too — the prefix is what matters', () => {
    const w = mountDialog([opt('x', 'Luôn cho phép', 'allow_always')]);
    expect(w.findAll('.perm__btn')[0]!.classes()).toContain('perm__btn--allow');
  });

  it('styles reject_once as reject', () => {
    const w = mountDialog([REJECT]);
    expect(w.findAll('.perm__btn')[0]!.classes()).toContain('perm__btn--reject');
  });

  it('styles reject_always as reject', () => {
    const w = mountDialog([opt('x', 'Luôn từ chối', 'reject_always')]);
    expect(w.findAll('.perm__btn')[0]!.classes()).toContain('perm__btn--reject');
  });

  it('leaves an unknown kind neutral — never allow, never reject', () => {
    const w = mountDialog([opt('x', 'Điều gì đó mới', 'escalate_to_admin')]);
    const classes = w.findAll('.perm__btn')[0]!.classes();
    expect(classes).not.toContain('perm__btn--allow');
    expect(classes).not.toContain('perm__btn--reject');
  });

  it('does not treat a kind that merely CONTAINS "allow" as allow', () => {
    // Prefix, not substring: "disallow_all" is not permission to proceed.
    const w = mountDialog([opt('x', 'Không cho', 'disallow_all')]);
    expect(w.findAll('.perm__btn')[0]!.classes()).not.toContain('perm__btn--allow');
  });

  it('leaves an empty kind neutral', () => {
    const w = mountDialog([opt('x', 'Trống', '')]);
    const classes = w.findAll('.perm__btn')[0]!.classes();
    expect(classes).not.toContain('perm__btn--allow');
    expect(classes).not.toContain('perm__btn--reject');
  });

  it('still renders an unknown kind as a usable button', () => {
    // Neutral styling must not mean "hidden": the user has to be able to pick it.
    const w = mountDialog([opt('novel-id', 'Tùy chọn mới', 'brand_new')]);
    expect(w.findAll('.perm__btn')[0]!.text()).toBe('Tùy chọn mới');
  });
});

describe('PermissionDialog — dismiss', () => {
  it('renders a dismiss button after the agent options', () => {
    const w = mountDialog([ALLOW, REJECT]);
    const buttons = w.findAll('.perm__btn');
    expect(buttons).toHaveLength(3);
    expect(buttons[2]!.text()).toBe('Bỏ qua');
  });

  it('emits dismiss, not choose — cancelled is not a rejection', async () => {
    const w = mountDialog([ALLOW]);
    await w.findAll('.perm__btn')[1]!.trigger('click');
    expect(w.emitted('dismiss')).toHaveLength(1);
    expect(w.emitted('choose')).toBeUndefined();
  });

  it('offers dismiss even when the agent sent no options', () => {
    const w = mountDialog([]);
    const buttons = w.findAll('.perm__btn');
    expect(buttons).toHaveLength(1);
    expect(buttons[0]!.text()).toBe('Bỏ qua');
  });

  it('never styles dismiss as allow or reject', () => {
    const w = mountDialog([ALLOW]);
    const classes = w.findAll('.perm__btn')[1]!.classes();
    expect(classes).not.toContain('perm__btn--allow');
    expect(classes).not.toContain('perm__btn--reject');
  });
});

describe('PermissionDialog — title', () => {
  it('prefers the tool call title', () => {
    const w = mountDialog([ALLOW], call({ title: 'Ghi vào src/main.rs' }));
    expect(w.find('.perm__title').text()).toBe('Ghi vào src/main.rs');
  });

  it('falls back to the kind when there is no title', () => {
    const w = mountDialog([ALLOW], call({ kind: 'edit' }));
    expect(w.find('.perm__title').text()).toBe('edit');
  });

  it('falls back to a generic phrase when both are absent', () => {
    const w = mountDialog([ALLOW], call());
    expect(w.find('.perm__title').text()).toBe('Agent xin quyền thực thi');
  });

  it('renders the title as text, so agent-authored markup cannot inject elements', () => {
    const w = mountDialog([ALLOW], call({ title: '<img src=x onerror=alert(1)>' }));
    const title = w.find('.perm__title');
    expect(title.text()).toBe('<img src=x onerror=alert(1)>');
    expect(title.find('img').exists()).toBe(false);
  });

  it('updates when the patch gains a title', async () => {
    const w = mountDialog([ALLOW], call({ kind: 'edit' }));
    expect(w.find('.perm__title').text()).toBe('edit');
    await w.setProps({ toolCall: call({ kind: 'edit', title: 'Sửa file' }) });
    expect(w.find('.perm__title').text()).toBe('Sửa file');
  });
});

describe('PermissionDialog — accessibility', () => {
  it('announces itself as an alertdialog', () => {
    // It interrupts the user to demand a decision; that is exactly alertdialog.
    const w = mountDialog([ALLOW]);
    expect(w.find('.perm').attributes('role')).toBe('alertdialog');
  });

  it('carries an accessible name', () => {
    const w = mountDialog([ALLOW]);
    expect(w.find('.perm').attributes('aria-label')).toBe('Yêu cầu quyền');
  });

  it('labels the request so it is not just a bare title', () => {
    const w = mountDialog([ALLOW]);
    expect(w.find('.perm__label').text()).toBe('Cần quyền');
  });
});
