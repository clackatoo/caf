// Live, the agents the debugger offers: Claude, installed by CAF, and the
// fake agent, which the debugger starts itself. Each test's installs go
// in a folder of its own (see server.ts), so Claude is never installed
// there, and it's never installed here: no test clicks its Install.

import { expect, test, type Page } from './server.ts'

test.use({ serve: [] })

/** An agent's row in the Agents panel. */
const row = (page: Page, name: string) => page.locator(`.agents [data-agent="${name}"]`)

async function openChat(page: Page, agent: string) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox').selectOption(agent)
  await page.getByRole('textbox', { name: /Folder/ }).fill(process.cwd())
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText(agent)
}

test('the Agents panel shows each agent, installed or not', async ({ page, server }) => {
  await page.goto(server.url)
  await expect(page.locator('.link')).toHaveText('Connected')
  // Not installed, with its size, or can't be, for want of Node or npm.
  await expect(row(page, 'claude')).toHaveAttribute('data-install', /^(Missing|Cannot)$/)
  if ((await row(page, 'claude').getAttribute('data-install')) === 'Missing') {
    await expect(row(page, 'claude')).toContainText(/About \d+ MB to download\./)
    await expect(row(page, 'claude').getByRole('button', { name: 'Install' })).toBeEnabled()
  }
  await expect(row(page, 'claude').getByRole('button', { name: 'Sign in' })).toHaveCount(0)
  await expect(row(page, 'fake')).toContainText('Started by the app')
})

test("signing in to an agent whose profile can't is refused, in CAF's words", async ({ page, server }) => {
  await page.goto(server.url)
  await row(page, 'fake').getByRole('button', { name: 'Sign in' }).click()
  await expect(row(page, 'fake').getByRole('alert')).toHaveText("CAF can't sign in to this agent now.")
})

test("a chat with an agent that isn't installed waits for it, and Not now stops its message", async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, 'claude')
  await expect(page.locator('.caf-chat [data-state]')).toHaveText('Waiting for its agent')
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill('hello')
  await box.press('Enter')
  const banner = page.locator('.caf-chat [data-agent-state]')
  await expect(banner).toHaveAttribute('data-agent-state', /^(Missing|Cannot)$/)
  if ((await banner.getAttribute('data-agent-state')) === 'Missing') {
    await expect(banner).toContainText(/claude isn't installed\. Installing it downloads about \d+ MB/)
  }
  await banner.getByRole('button', { name: 'Not now' }).click()
  await expect(page.locator('.caf-chat .note').last()).toContainText(
    "The agent isn't installed, so the message that waited for it was stopped.",
  )
  // Every step replayed as it happened, from the steps and the recording.
  await expect(page.locator('.diverged')).toHaveCount(0)
})
