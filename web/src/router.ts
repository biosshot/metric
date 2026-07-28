import { createRouter, createWebHistory } from 'vue-router';
import IssuesView from './views/IssuesView.vue';
import IssueDetailView from './views/IssueDetailView.vue';
import EventDetailView from './views/EventDetailView.vue';
import AuthView from './views/AuthView.vue';
import OrganizationView from './views/OrganizationView.vue';
import ProjectSetupView from './views/ProjectSetupView.vue';
import ProjectSettingsView from './views/ProjectSettingsView.vue';
import SystemStatusView from './views/SystemStatusView.vue';
import LogsView from './views/LogsView.vue';
import LogDetailView from './views/LogDetailView.vue';
import TransactionsView from './views/TransactionsView.vue';
import TraceView from './views/TraceView.vue';
import PerformanceView from './views/PerformanceView.vue';
import ReleasesView from './views/ReleasesView.vue';
import ReleaseDetailView from './views/ReleaseDetailView.vue';
import FeedbackView from './views/FeedbackView.vue';
import FeedbackDetailView from './views/FeedbackDetailView.vue';
import ExploreView from './views/ExploreView.vue';
import DashboardsView from './views/DashboardsView.vue';
import AlertsView from './views/AlertsView.vue';
import MonitorsView from './views/MonitorsView.vue';

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/dashboard' },
    { path: '/dashboard', name: 'dashboard', component: DashboardsView },
    { path: '/dashboards', redirect: '/dashboard' },
    { path: '/issues', name: 'issues', component: IssuesView },
    { path: '/issues/:issueId', name: 'issue', component: IssueDetailView },
    { path: '/events/:eventId', name: 'event', component: EventDetailView },
    { path: '/logs', name: 'logs', component: LogsView },
    { path: '/logs/:logId', name: 'log', component: LogDetailView },
    { path: '/traces', name: 'traces', component: TransactionsView },
    { path: '/traces/:traceId', name: 'trace', component: TraceView },
    { path: '/performance', name: 'performance', component: PerformanceView },
    { path: '/explore', name: 'explore', component: ExploreView },
    { path: '/alerts', name: 'alerts', component: AlertsView },
    { path: '/monitors', name: 'monitors', component: MonitorsView },
    {
      path: '/replays',
      name: 'replays',
      component: () => import('./views/ReplaysView.vue'),
    },
    {
      path: '/replays/:replayId',
      name: 'replay',
      component: () => import('./views/ReplayDetailView.vue'),
    },
    { path: '/feedback', name: 'feedback', component: FeedbackView },
    { path: '/feedback/:feedbackId', name: 'feedback-item', component: FeedbackDetailView },
    { path: '/releases', name: 'releases', component: ReleasesView },
    { path: '/releases/:releaseId', name: 'release', component: ReleaseDetailView },
    { path: '/auth/setup', name: 'password-setup', component: AuthView },
    { path: '/organization', name: 'organization', component: OrganizationView },
    { path: '/account/tokens', redirect: '/organization' },
    { path: '/project/setup', name: 'setup', component: ProjectSetupView },
    { path: '/project/settings', redirect: '/settings' },
    { path: '/settings', name: 'settings', component: ProjectSettingsView },
    { path: '/system', name: 'system', component: SystemStatusView },
    { path: '/:pathMatch(.*)*', redirect: '/dashboard' },
  ],
  scrollBehavior: () => ({ top: 0 }),
});
