import { createApp } from 'vue';
import { createPinia } from 'pinia';
import { VueQueryPlugin, QueryClient } from '@tanstack/vue-query';
import App from './App.vue';
import { router } from './router';
import { retryQuery } from './api/client';
import { i18n, initializeLocale } from './i18n';
import './style.css';

await initializeLocale();

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: retryQuery,
      refetchOnWindowFocus: false,
      staleTime: 10_000,
    },
    mutations: { retry: false },
  },
});

createApp(App)
  .use(createPinia())
  .use(router)
  .use(i18n)
  .use(VueQueryPlugin, { queryClient })
  .mount('#app');
