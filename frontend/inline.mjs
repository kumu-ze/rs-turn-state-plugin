import fs from 'node:fs'

const root = new URL('./dist/', import.meta.url)
let html = fs.readFileSync(new URL('index.html', root), 'utf8')
html = html.replace(/<script[^>]*src="([^"]+)"[^>]*><\/script>/g, (_, src) => `<script type="module">${fs.readFileSync(new URL(src.replace(/^\//, ''), root), 'utf8').replace(/<\/script/gi, '<\\/script')}</script>`)
html = html.replace(/<link[^>]*href="([^"]+\.css)"[^>]*>/g, (_, src) => `<style>${fs.readFileSync(new URL(src.replace(/^\//, ''), root), 'utf8')}</style>`)
fs.writeFileSync(new URL('../src/page.html', import.meta.url), html)
