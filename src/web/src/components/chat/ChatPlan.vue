<script setup lang="ts">
// The agent's plan as a checklist (SPEC-004 §5.1).
//
// A plan is a FULL snapshot — SPEC-003's fold replaces it rather than merging, so
// this component never has to reconcile: whatever it is handed is the current truth.
// Rendering it as a list of `plan.entries` in order is therefore correct by
// construction, and a step disappearing between updates means the agent dropped it.

import { computed } from 'vue';

import type { PlanPayload } from '../../api/acp';

const props = defineProps<{ plan: PlanPayload }>();

const done = computed(
  () => props.plan.entries.filter((e) => e.status === 'completed').length,
);
const total = computed(() => props.plan.entries.length);

/** Glyph per status; an unknown status keeps a neutral marker (v2 `Other`). */
function glyph(status: string | undefined): string {
  switch (status) {
    case 'completed':
      return '✓';
    case 'in_progress':
      return '▸';
    case 'pending':
      return '○';
    default:
      return '·';
  }
}
</script>

<template>
  <section v-if="total" class="plan">
    <header class="plan__head">
      Kế hoạch
      <span class="plan__count">{{ done }}/{{ total }}</span>
    </header>
    <ol class="plan__list">
      <li
        v-for="(step, i) in plan.entries"
        :key="i"
        class="plan__item"
        :class="[`plan__item--${step.status ?? 'unknown'}`, step.priority ? `plan__item--p-${step.priority}` : '']"
      >
        <span class="plan__glyph" aria-hidden="true">{{ glyph(step.status) }}</span>
        <!-- Plain text, not markdown: plan steps are short imperatives, and an
             underscore in a filename should not become italics. -->
        <span class="plan__text">{{ step.content }}</span>
      </li>
    </ol>
  </section>
</template>

<style scoped>
.plan {
  margin-bottom: 8px;
  padding: 6px 8px;
  border: 1px solid #2c2c2c;
  border-radius: 4px;
  background: #191919;
}
.plan__head {
  display: flex;
  gap: 6px;
  margin-bottom: 4px;
  color: #9e9e9e;
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.plan__count {
  color: #7a7a7a;
}
.plan__list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.plan__item {
  display: flex;
  gap: 6px;
  padding: 1px 0;
  color: #c4c4c4;
  font-size: 12px;
}
.plan__glyph {
  width: 12px;
  color: #7a7a7a;
  text-align: center;
}
.plan__item--completed {
  color: #6f8f72;
}
.plan__item--completed .plan__glyph {
  color: #6fcf74;
}
.plan__item--completed .plan__text {
  text-decoration: line-through;
}
.plan__item--in_progress {
  color: #ffd79b;
}
.plan__item--in_progress .plan__glyph {
  color: #d3a83c;
}
/* Priority is a hint, not a hierarchy: a left tick is enough to spot the high
   ones without turning the list into a colour chart. */
.plan__item--p-high .plan__text {
  font-weight: 600;
}
</style>
