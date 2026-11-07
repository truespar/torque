import { createApp } from 'vue'
import { createPinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import App from './App.vue'
import { readShareLink } from './utils/share'

import '@fontsource-variable/inter'
import '@fontsource-variable/jetbrains-mono'
import './styles/variables.css'
import './styles/base.css'
import './styles/components.css'

readShareLink()

const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: '/:pathMatch(.*)*', component: () => import('./components/layout/AppShell.vue') },
  ],
})

const app = createApp(App)
app.use(createPinia())
app.use(router)
app.mount('#app')
