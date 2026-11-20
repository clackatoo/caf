import { svelte } from '@sveltejs/vite-plugin-svelte'
import { svelteTesting } from '@testing-library/svelte/vite'
import { defineConfig } from 'vitest/config'
import { route } from './dev-proxy.ts'

// `npm run dev` serves the app with Vite, passing `/viewer`, `/inspect`
// and `/recording` on to a caf-debugger on 127.0.0.1, port
// `CAF_DEBUGGER_PORT` or 7320 (see dev-proxy.ts). Open the dev server's
// address with the `#token=…` caf-debugger printed.
const server = `http://127.0.0.1:${process.env.CAF_DEBUGGER_PORT ?? '7320'}`

export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  server: {
    proxy: { '/viewer': route(server), '/inspect': route(server), '/recording': route(server) },
  },
  test: {
    environment: 'jsdom',
    include: ['test/**/*.test.ts'],
  },
})
