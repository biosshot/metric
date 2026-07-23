import { createRouter, createWebHistory } from 'vue-router';
import IssuesView from './views/IssuesView.vue';
import IssueDetailView from './views/IssueDetailView.vue';
import EventDetailView from './views/EventDetailView.vue';
import ProjectSetupView from './views/ProjectSetupView.vue';
import ProjectSettingsView from './views/ProjectSettingsView.vue';
import SystemStatusView from './views/SystemStatusView.vue';

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/issues' },
    { path: '/issues', name: 'issues', component: IssuesView },
    { path: '/issues/:issueId', name: 'issue', component: IssueDetailView },
    { path: '/events/:eventId', name: 'event', component: EventDetailView },
    { path: '/project/setup', name: 'setup', component: ProjectSetupView },
    { path: '/project/settings', name: 'settings', component: ProjectSettingsView },
    { path: '/system', name: 'system', component: SystemStatusView },
    { path: '/:pathMatch(.*)*', redirect: '/issues' },
  ],
  scrollBehavior: () => ({ top: 0 }),
});
