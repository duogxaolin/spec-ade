// C43 — the boundaries, and the clock-skew case.
//
// Every assertion pins an exact `now` rather than mocking global time: the whole
// reason `relativeTime` takes a clock is that "59s vs 60s" cannot be tested
// against a moving one.

import { describe, expect, it } from 'vitest';

import { absoluteTime, relativeTime } from './relativeTime';

/** Fixed reference point. Value is arbitrary; being fixed is the point. */
const NOW_SECONDS = 1_700_000_000;
const NOW_MS = NOW_SECONDS * 1000;

/** What `relativeTime` says for a commit `ago` seconds old. */
function ago(seconds: number): string {
  return relativeTime(NOW_SECONDS - seconds, NOW_MS);
}

describe('relativeTime boundaries', () => {
  it('is "vừa xong" up to 59s and "1 phút trước" at exactly 60s', () => {
    expect(ago(0)).toBe('vừa xong');
    expect(ago(59)).toBe('vừa xong');
    expect(ago(60)).toBe('1 phút trước');
  });

  it('switches from minutes to hours at exactly one hour', () => {
    expect(ago(59 * 60 + 59)).toBe('59 phút trước');
    expect(ago(3600)).toBe('1 giờ trước');
  });

  it('switches from hours to days at exactly 24 hours', () => {
    expect(ago(23 * 3600 + 3599)).toBe('23 giờ trước');
    expect(ago(24 * 3600)).toBe('1 ngày trước');
  });

  it('switches from days to months at 30 days', () => {
    expect(ago(29 * 86400)).toBe('29 ngày trước');
    expect(ago(30 * 86400)).toBe('1 tháng trước');
  });

  it('switches from months to years at 365 days', () => {
    expect(ago(364 * 86400)).toBe('12 tháng trước');
    expect(ago(365 * 86400)).toBe('1 năm trước');
    expect(ago(2 * 365 * 86400)).toBe('2 năm trước');
  });

  it('leaves no gap between units', () => {
    // A boundary written twice (`< 60` here, `<= 60` there) leaves a second that
    // matches no branch and renders empty. Walking every second across each
    // boundary is the cheap way to be sure that never happens.
    const boundaries = [60, 3600, 86400, 30 * 86400, 365 * 86400];
    for (const boundary of boundaries) {
      for (const offset of [-1, 0, 1]) {
        expect(ago(boundary + offset)).not.toBe('');
      }
    }
  });
});

describe('relativeTime on a clock that disagrees', () => {
  it('reads a future timestamp as "vừa xong" rather than a negative duration', () => {
    // Real cause, not hypothetical: a commit authored on a machine with a fast
    // clock, or a rebase that preserved an author date ahead of now.
    expect(relativeTime(NOW_SECONDS + 5, NOW_MS)).toBe('vừa xong');
    expect(relativeTime(NOW_SECONDS + 86400 * 365, NOW_MS)).toBe('vừa xong');
  });

  it('returns empty for a non-finite timestamp instead of "NaN phút trước"', () => {
    expect(relativeTime(Number.NaN, NOW_MS)).toBe('');
    expect(relativeTime(Number.POSITIVE_INFINITY, NOW_MS)).toBe('');
  });

  it('treats the input as seconds, not milliseconds', () => {
    // Passing `Date.now()` where seconds are expected is the likeliest misuse, and
    // it reads as ~55,000 years in the future — so it must land in the future
    // branch rather than printing a number.
    expect(relativeTime(NOW_MS, NOW_MS)).toBe('vừa xong');
  });
});

describe('absoluteTime', () => {
  it('formats a git timestamp down to the minute', () => {
    // Local-time dependent by design (it is a tooltip for the user's own clock),
    // so the shape is asserted rather than a specific instant.
    expect(absoluteTime(NOW_SECONDS)).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  it('returns empty rather than "Invalid Date" for a bad timestamp', () => {
    expect(absoluteTime(Number.NaN)).toBe('');
    expect(absoluteTime(Number.POSITIVE_INFINITY)).toBe('');
  });
});
