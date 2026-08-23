// The split handle (SPEC-008 §5.4) — a two-phase drag so resizing never
// re-renders the tree mid-gesture ([INVENTED-7]).
//
// pointerdown caches the container rect ONCE and captures the pointer;
// pointermove writes `flex` DIRECTLY to the two child elements (no store, no
// Vue reactivity — the browser lays out, nothing re-renders); pointerup
// commits the final ratio to the layout store exactly once and releases the
// capture. Unmounting mid-drag cleans up WITHOUT committing: a half-finished
// gesture was never confirmed by the user.
//
// The ResizeObserver on the container keeps the cached rect honest when a
// window resize or font change lands mid-drag.

<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue';
import { clampRatio } from '../../panes/tree';
import type { Direction, PanePath } from '../../panes/tree';
import { useLayoutStore } from '../../stores/layout';

const props = defineProps<{
  /** Which split node this handle sits between. */
  path: PanePath;
  direction: Direction;
  /** Element of the `first` half — flex is written here during the drag. */
  firstEl: HTMLElement;
  /** Element of the `second` half. */
  secondEl: HTMLElement;
  /** The split's bounding box; pointer math is relative to it. */
  containerEl: HTMLElement;
}>();

const layout = useLayoutStore();

const dragging = ref(false);

const label = computed(() => (props.direction === 'horizontal' ? 'Kéo để đổi tỉ lệ ngang' : 'Kéo để đổi tỉ lệ dọc'));

let rectStart: DOMRect | null = null;
let observer: ResizeObserver | null = null;

function onPointerDown(ev: PointerEvent): void {
  if (ev.button !== 0 || dragging.value) return;
  ev.preventDefault();

  // Snapshot BEFORE capturing: mid-drag reflows must not shift the origin.
  rectStart = props.containerEl.getBoundingClientRect();
  observer = new ResizeObserver(() => {
    rectStart = props.containerEl.getBoundingClientRect();
  });
  observer.observe(props.containerEl);

  dragging.value = true;
  (ev.currentTarget as HTMLElement).setPointerCapture(ev.pointerId);
}

function applyFlex(ratio: number): void {
  // Direct style writes bypass reactivity on purpose — see header comment.
  props.firstEl.style.flex = `${ratio} 1 0%`;
  props.secondEl.style.flex = `${1 - ratio} 1 0%`;
}

function onPointerMove(ev: PointerEvent): void {
  if (!dragging.value || !rectStart) return;
  const ratio =
    props.direction === 'horizontal'
      ? (ev.clientX - rectStart.left) / Math.max(rectStart.width, 1)
      : (ev.clientY - rectStart.top) / Math.max(rectStart.height, 1);
  applyFlex(clampRatio(ratio));
}

function onPointerUp(ev: PointerEvent): void {
  if (!dragging.value) return;

  // Read back what the DOM shows rather than tracking a parallel variable —
  // the flex write is the single source of truth during the gesture.
  const firstBasis = Number.parseFloat(props.firstEl.style.flex) || 0.5;
  const committed = clampRatio(firstBasis);
  endDrag(ev);
  layout.setRatio(props.path, committed);
}

/** Lost capture (pointercancel, Escape via browser) — restore, no commit. */
function onLostPointerCapture(): void {
  if (!dragging.value) return;
  cancelDrag();
}

function cancelDrag(): void {
  dragging.value = false;
  rectStart = null;
  observer?.disconnect();
  observer = null;
  // Drop the inline overrides; reactive ratio re-applies the stored value.
  props.firstEl.style.removeProperty('flex');
  props.secondEl.style.removeProperty('flex');
}

function endDrag(ev: PointerEvent): void {
  dragging.value = false;
  rectStart = null;
  observer?.disconnect();
  observer = null;
  try {
    (ev.currentTarget as HTMLElement).releasePointerCapture(ev.pointerId);
  } catch {
    // Capture already gone (e.g. pointercancel raced the up) — fine.
  }
  props.firstEl.style.removeProperty('flex');
  props.secondEl.style.removeProperty('flex');
}

onBeforeUnmount(() => {
  // Mid-drag unmount: clean up WITHOUT committing an unconfirmed gesture.
  if (!dragging.value) return;
  dragging.value = false;
  rectStart = null;
  observer?.disconnect();
  observer = null;
});
</script>

<template>
  <div
    class="psplit"
    :class="[direction === 'horizontal' ? 'psplit--horizontal' : 'psplit--vertical', { 'psplit--active': dragging }]"
    role="separator"
    :aria-orientation="direction === 'horizontal' ? 'vertical' : 'horizontal'"
    :aria-label="label"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @lostpointercapture="onLostPointerCapture"
  />
</template>

<style scoped>
.psplit {
  flex: none;
  background: var(--border, #33353b);
  transition: background 120ms ease;
}
.psplit--horizontal {
  width: var(--split-size, 5px);
  height: 100%;
  cursor: col-resize;
}
.psplit--vertical {
  height: var(--split-size, 5px);
  width: 100%;
  cursor: row-resize;
}
.psplit:hover,
.psplit--active {
  background: var(--accent, #4f8cff);
}
.psplit:focus-visible {
  outline: 2px solid var(--accent, #4f8cff);
  outline-offset: -2px;
}
</style>
