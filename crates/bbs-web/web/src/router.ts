import { createRouter, createWebHashHistory } from 'vue-router'

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/login', name: 'login', component: () => import('./pages/LoginPage.vue') },
    { path: '/', name: 'dashboard', component: () => import('./pages/DashboardPage.vue') },
    { path: '/adverts', name: 'adverts', component: () => import('./pages/AdvertsPage.vue') },
    { path: '/sessions', name: 'sessions', component: () => import('./pages/SessionsPage.vue') },
    { path: '/users', name: 'users', component: () => import('./pages/UsersPage.vue') },
    { path: '/rooms', name: 'rooms', component: () => import('./pages/RoomsPage.vue') },
    { path: '/messages', name: 'messages', component: () => import('./pages/MessagesPage.vue') },
    { path: '/reports', name: 'reports', component: () => import('./pages/ReportsPage.vue') },
    { path: '/backups', name: 'backups', component: () => import('./pages/BackupsPage.vue') },
    { path: '/logs', name: 'logs', component: () => import('./pages/LogsPage.vue') },
    { path: '/audit', name: 'audit', component: () => import('./pages/AuditPage.vue') },
    { path: '/plugins', name: 'plugins', component: () => import('./pages/PluginsPage.vue') },
    { path: '/settings', name: 'settings', component: () => import('./pages/SettingsPage.vue') },
    { path: '/:pathMatch(.*)*', redirect: { name: 'dashboard' } },
  ],
})

export default router
