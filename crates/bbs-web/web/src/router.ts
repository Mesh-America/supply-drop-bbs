import { createRouter, createWebHashHistory } from 'vue-router'

// Hash history: works from any path layout without server-side routing config.
const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/login', name: 'login', component: () => import('./pages/LoginPage.vue') },
    { path: '/', name: 'dashboard', component: () => import('./pages/DashboardPage.vue') },
    { path: '/adverts', name: 'adverts', component: () => import('./pages/AdvertsPage.vue') },
    { path: '/sessions', name: 'sessions', component: () => import('./pages/SessionsPage.vue') },
    { path: '/users', name: 'users', component: () => import('./pages/UsersPage.vue') },
    { path: '/logs', name: 'logs', component: () => import('./pages/LogsPage.vue') },
    { path: '/:pathMatch(.*)*', redirect: { name: 'dashboard' } },
  ],
})

export default router
