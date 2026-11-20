import { svelte } from '@sveltejs/vite-plugin-svelte'
import { svelteTesting } from '@testing-library/svelte/vite'
import { defineConfig } from 'vitest/config'

// `npm run dev` serves the app on its own; it talks to a caf-debugger
// given in `?server=`, or on 127.0.0.1:7320 (see src/link.ts). The server
// only takes its own origin, so for anything but the chat view's look, use
// the build that caf-debugger serves.
export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  test: {
    environment: 'jsdom',
    include: ['test/**/*.test.ts'],
  },
})
