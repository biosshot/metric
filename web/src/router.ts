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
import SettingsView from './views/SettingsView.vue';
import NotFoundView from './views/NotFoundView.vue';
import FirstProjectOnboarding from './components/FirstProjectOnboarding.vue';
import OrganizationCreateView from './views/OrganizationCreateView.vue';

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/', redirect: '/dashboard' },
    { path: '/dashboard', name: 'dashboard', component: DashboardsView },
    { path: '/dashboards', redirect: '/dashboard' },
    {
      path: '/metrics',
      name: 'metrics',
      component: ExploreView,
      props: { initialDataset: 'metrics', datasetLocked: true, metricsView: 'overview' },
    },
    {
      path: '/metrics/query',
      name: 'metrics-query',
      component: ExploreView,
      props: { initialDataset: 'metrics', datasetLocked: true, metricsView: 'query' },
    },
    { path: '/issues', name: 'issues', component: IssuesView },
    { path: '/issues/:issueId', name: 'issue', component: IssueDetailView },
    { path: '/events/:eventId', name: 'event', component: EventDetailView },
    { path: '/logs', name: 'logs', component: LogsView },
    { path: '/logs/:logId', name: 'log', component: LogDetailView },
    { path: '/traces', name: 'traces', component: TransactionsView },
    { path: '/traces/:traceId', name: 'trace', component: TraceView },
    { path: '/performance', name: 'performance', component: PerformanceView },
    { path: '/explore', name: 'explore', component: ExploreView },
    { path: '/alerts', redirect: '/settings/notifications' },
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
    { path: '/organization', redirect: '/settings/organization' },
    { path: '/account/tokens', redirect: '/settings/organization' },
    { path: '/projects/new', name: 'project-new', component: FirstProjectOnboarding },
    { path: '/organizations/new', name: 'organization-new', component: OrganizationCreateView },
    { path: '/project/setup', name: 'setup', component: ProjectSetupView },
    { path: '/project/settings', redirect: '/settings/project' },
    {
      path: '/settings',
      component: SettingsView,
      children: [
        { path: '', redirect: '/settings/project' },
        { path: 'project', name: 'settings-project', component: ProjectSettingsView },
        { path: 'notifications', name: 'settings-notifications', component: AlertsView },
        { path: 'organization', name: 'settings-organization', component: OrganizationView },
        { path: 'system', name: 'settings-system', component: SystemStatusView },
      ],
    },
    { path: '/system', redirect: '/settings/system' },
    { path: '/:pathMatch(.*)*', name: 'not-found', component: NotFoundView },
  ],
  scrollBehavior: (to) => (to.hash ? { el: to.hash, top: 24 } : { top: 0 }),
});
