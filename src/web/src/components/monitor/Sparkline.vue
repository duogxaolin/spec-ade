<script setup lang="ts">
// A 60-point series as one SVG path (SPEC-006 §5.9).
//
// No chart library: the whole job is `values → d`, which `monitor/sparkline.ts`
// already does and tests. The smallest charting dependency is ~40 KB for what is
// one `<path>` element.
//
// `preserveAspectRatio="none"` lets the fixed 100×24 coordinate space stretch to
// whatever width the card has, so the path never needs recomputing on resize.

import { computed } from 'vue';

import { sparklineArea, sparklinePath } from '../../monitor/sparkline';

const props = withDefaults(
  defineProps<{
    values: number[];
    /** Upper bound of the value axis. Percentages pass 100 so the scale is stable. */
    max?: number;
    color?: string;
    /** Fill under the line. Off for dense stacks where it would just be noise. */
    area?: boolean;
    label?: string;
  }>(),
  { max: 100, color: '#7a9ec4', area: true, label: '' },
);

const WIDTH = 100;
const HEIGHT = 24;

const options = computed(() => ({ width: WIDTH, height: HEIGHT, max: props.max, min: 0 }));
const line = computed(() => sparklinePath(props.values, options.value));
const fill = computed(() => (props.area ? sparklineArea(props.values, options.value) : ''));
</script>

<template>
  <svg
    class="spark"
    :viewBox="`0 0 ${WIDTH} ${HEIGHT}`"
    preserveAspectRatio="none"
    role="img"
    :aria-label="label"
  >
    <!-- An empty history (the first 3s after mount) renders nothing rather than a
         line at zero, which would read as a measured idle. -->
    <path v-if="fill" :d="fill" :fill="color" fill-opacity="0.18" stroke="none" />
    <path
      v-if="line"
      :d="line"
      fill="none"
      :stroke="color"
      stroke-width="1"
      vector-effect="non-scaling-stroke"
      stroke-linejoin="round"
    />
  </svg>
</template>

<style scoped>
.spark {
  display: block;
  width: 100%;
  height: 24px;
}
</style>
