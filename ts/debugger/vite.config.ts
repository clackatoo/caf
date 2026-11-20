import { svelte } from '@sveltejs/vite-plugin-svelte'
import { svelteTesting } from '@testing-library/svelte/vite'
import { defineConfig } from 'vitest/config'

// `npm run dev` serves the app with Vite, passing `/viewer`, `/inspect`
// and `/recording` on to a caf-debugger on 127.0.0.1, port
// `CAF_DEBUGGER_PORT` or 7320. Open the dev server's address with the
// `#token=…` caf-debugger printed. caf-debugger only takes requests from
// its own origin, so they're passed on as if from there.
const port = process.env.CAF_DEBUGGER_PORT ?? '7320'
const server = `http://127.0.0.1:${port}`
const route = {
  target: server,
  ws: true,
  configure: (proxy: { on(event: string, listener: (request: { setHeader(name: string, value: string): void }) => void): void }) => {
    const own = (request: { setHeader(name: string, value: string): void }) => request.setHeader('origin', server)
    proxy.on('proxyReq', own)
    proxy.on('proxyReqWs', own)
  },
}

export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  server: {
    proxy: { '/viewer': route, '/inspect': route, '/recording': route },
  },
  test: {
    environment: 'jsdom',
    include: ['test/**/*.test.ts'],
  },
})
