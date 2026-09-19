import { createApp } from 'vue'
import App from './App.vue'
import { applyResolvedTheme, DEFAULT_CUSTOM_THEME_COLOR, DEFAULT_THEME_COLOR, resolveTheme } from './theme'
import './styles/index.css'

applyResolvedTheme(document.documentElement, resolveTheme('light', DEFAULT_THEME_COLOR, DEFAULT_CUSTOM_THEME_COLOR))
createApp(App).mount('#app')
