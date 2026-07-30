// Spec ADE frontend entry point.
//
// Roadmap: Pha 0 (docs/analysis/07-build-roadmap.md) — mount the Vue app with
// Quasar + Pinia. Router, stores, and feature panels are wired in later phases.

import { createApp } from 'vue';
import { createPinia } from 'pinia';
import { Quasar } from 'quasar';

import App from './App.vue';

// Quasar core styles. No Quasar component is used yet (the Pha 0/1 shell is
// plain CSS), so the icon set is chosen with the first phase that renders one.
// TODO(spec-008): pick an icon set (@quasar/extras) and import its CSS.
import 'quasar/src/css/index.sass';

const app = createApp(App);

app.use(createPinia());
app.use(Quasar, {
  // TODO(spec-008): plugins (Notify, Dialog, LocalStorage) + theme config, added
  // with the pane/tab layout that first needs them.
  plugins: {},
});

// TODO(phase-8): install vue-router once the pane/tab layout system lands.
app.mount('#app');
