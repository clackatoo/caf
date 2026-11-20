import { defineConfig } from '@playwright/test'

// End to end, against caf-debugger itself: each test starts its own (see
// e2e/server.ts), which serves this app's build, so run `npm run build`
// first, and build caf-debugger and caf-fake-agent (`cargo build -p
// caf-debugger -p caf-fake-agent`). `CAF_DEBUGGER` names another binary.
export default defineConfig({
  testDir: 'e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? 'github' : 'list',
  timeout: 30_000,
  use: {
    browserName: 'chromium',
    headless: true,
    trace: 'retain-on-failure',
    // A Chromium of its own, if Playwright's isn't installed.
    ...(process.env.CAF_E2E_CHROMIUM ? { launchOptions: { executablePath: process.env.CAF_E2E_CHROMIUM } } : {}),
  },
})
