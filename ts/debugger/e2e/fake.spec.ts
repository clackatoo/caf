// Live, installing and signing in, with CAF's fake agent under the `fake`
// profile (see caf-core's `Builtin::Fake`): one started by the debugger,
// which signs in through `caf-fake-agent auth`, and one CAF would
// install, whose install always fails at once, as offline. Nothing is
// ever installed, nor any real sign-in run.

import { existsSync } from 'node:fs'
import { join } from 'node:path'
import { expect, test, type Page } from './server.ts'

const agents = `
[[agent]]
name = "signs-in"
program = "{fake}"
args = ["{script}"]
profile = "fake"

[[agent]]
name = "offline"
installed = true
profile = "fake"
`

/** The fake's sign-in: it opens its link with BROWSER, shows a code, and takes only this paste. */
const auth = {
  url: 'https://caf.example/sign-in?state=e2e',
  browser: true,
  code: 'WXYZ-9876',
  paste: true,
  expect: 'code#state',
  state: '{state}/signed-in',
}

test.use({ setup: { agents, auth } })

const row = (page: Page, name: string) => page.locator(`.agents [data-agent="${name}"]`)

/**
 * Waits for CAF's look at its agents, as it starts, to be done: the
 * installed fake is then known to be missing. A status command may run
 * when that look ends, so a test that waits first never has one start
 * partway through it.
 */
async function surveyed(page: Page) {
  await expect(row(page, 'offline')).toHaveAttribute('data-install', 'Missing')
}

async function openChat(page: Page, agent: string) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox').selectOption(agent)
  await page.getByRole('textbox', { name: /Folder/ }).fill(process.cwd())
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText(agent)
}

async function send(page: Page, text: string) {
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
}

/** Every step replayed as it happened, from the steps and the recording, so far. */
async function noDivergence(page: Page) {
  await expect(page.locator('.diverged')).toHaveCount(0)
}

test('an install that fails says why, offers Try again, and stops the waiting message', async ({ page, server }) => {
  await page.goto(server.url)
  await expect(row(page, 'offline')).toHaveAttribute('data-install', 'Missing')
  await expect(row(page, 'offline')).toContainText('About 4 MB to download.')
  await openChat(page, 'offline')
  await send(page, 'hello')
  const banner = page.locator('.caf-chat [data-agent-state]')
  await expect(banner).toContainText("offline isn't installed. Installing it downloads about 4 MB")
  await banner.getByRole('button', { name: 'Install' }).click()
  await expect(banner).toHaveAttribute('data-agent-state', 'Cannot')
  await expect(banner).toContainText("offline can't be installed: the last try failed because the network was out of reach.")
  await expect(banner.getByRole('button', { name: 'Try again' })).toBeEnabled()
  await expect(page.locator('.caf-chat .note').last()).toContainText(
    "The agent's install failed, so the message that waited for it was stopped.",
  )
  await expect(row(page, 'offline')).toContainText('the network was out of reach')
  await expect(row(page, 'offline').getByRole('button', { name: 'Try again to install offline' })).toBeVisible()
  await noDivergence(page)
})

test('installs from the panel, and fails as offline', async ({ page, server }) => {
  await page.goto(server.url)
  await row(page, 'offline').getByRole('button', { name: 'Install' }).click()
  await expect(row(page, 'offline')).toHaveAttribute('data-install', 'Cannot')
  await expect(row(page, 'offline')).toContainText("Can't install")
  await noDivergence(page)
})

test("signs in from the panel, with the link, the code and a paste, then out", async ({ page, server }) => {
  await page.goto(server.url)
  const agent = row(page, 'signs-in')
  // CAF runs a host's own command's sign-in only once it has answered as
  // the profile's adapter in this run, so Sign in waits for that.
  await expect(agent).toContainText('Started by the app')
  await expect(agent.getByRole('button', { name: 'Sign in' })).toHaveCount(0)
  await openChat(page, 'signs-in')
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  await expect(agent.getByRole('button', { name: 'Sign in to signs-in' })).toBeVisible()
  // Another agent's chat beside, so the panel shows this sign-in itself.
  await openChat(page, 'offline')

  await agent.getByRole('button', { name: 'Sign in to signs-in' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', 'SigningIn')
  const link = agent.getByRole('link', { name: 'the sign-in page on caf.example' })
  await expect(link).toHaveAttribute('href', 'https://caf.example/sign-in?state=e2e')
  await expect(link).toHaveAttribute('target', '_blank')
  await expect(link).toHaveAttribute('rel', 'noopener noreferrer')
  await expect(agent.locator('code.code')).toHaveText('WXYZ-9876')

  await agent.getByRole('textbox').fill('code#state')
  await agent.getByRole('button', { name: 'Send the code' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedIn')
  await expect(agent).toContainText('Signed in with an account')
  expect(existsSync(join(server.state, 'signed-in'))).toBe(true)

  await agent.getByRole('button', { name: 'Sign out' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedOut')
  await expect(agent).toContainText('Signed out')
  await noDivergence(page)
})

test('a sign-in stopped goes back as it was', async ({ page, server }) => {
  await page.goto(server.url)
  await surveyed(page)
  await openChat(page, 'signs-in')
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  const agent = row(page, 'signs-in')
  // Signed out, as its status command says once its adapter has answered
  // as the fake's: it keeps no sign-in yet.
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedOut')
  await agent.getByRole('button', { name: 'Sign in to signs-in' }).click()
  // The chat beside is this agent's, so it shows the sign-in, and the
  // panel points there: one paste box on the page.
  const banner = page.locator('.caf-chat [data-agent-state="SigningIn"]')
  await expect(banner.locator('code.code')).toHaveText('WXYZ-9876')
  await expect(agent).toContainText('Signing in: see the chat.')
  await expect(page.getByRole('textbox', { name: /paste it here/ })).toHaveCount(1)
  await banner.getByRole('button', { name: 'Stop signing in' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedOut')
  await noDivergence(page)
})

test("a turn that failed for want of signing in offers Sign in; once signed in, the chat says so", async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, 'signs-in')
  await send(page, 'sign in')
  const failed = page.locator('.caf-chat .turn[data-life="Failed"]')
  await expect(failed).toHaveCount(1)
  await expect(failed).toContainText('The agent wants you to sign in.')
  await failed.getByRole('button', { name: 'Sign in' }).click()
  const banner = page.locator('.caf-chat [data-agent-state="SigningIn"]')
  await expect(banner.getByRole('link')).toHaveAttribute('href', 'https://caf.example/sign-in?state=e2e')
  await banner.getByRole('textbox', { name: /paste it here/ }).fill('code#state')
  await banner.getByRole('button', { name: 'Send the code' }).click()
  await expect(page.locator('.caf-chat .note').last()).toContainText('You signed in to the agent')
  await expect(banner).toHaveCount(0)
  // Send again stays, and the fake answers it this time.
  await failed.getByRole('button', { name: 'Send again' }).click()
  await expect(page.locator('.caf-chat .turn').last().locator('.words .markdown')).toContainText("CAF's fake agent")
  await noDivergence(page)
})

test('a wrong code fails the sign-in, and it goes back as it was', async ({ page, server }) => {
  await page.goto(server.url)
  await surveyed(page)
  await openChat(page, 'signs-in')
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  const agent = row(page, 'signs-in')
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedOut')
  await agent.getByRole('button', { name: 'Sign in to signs-in' }).click()
  const banner = page.locator('.caf-chat [data-agent-state="SigningIn"]')
  await banner.getByRole('textbox', { name: /paste it here/ }).fill('wrong')
  await banner.getByRole('button', { name: 'Send the code' }).click()
  // Signed out still: never signed in, nor still signing in.
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedOut')
  await noDivergence(page)
})

test("with a closed chat of the agent beside, the panel shows its sign-in itself", async ({ page, server }) => {
  await page.goto(server.url)
  await surveyed(page)
  await openChat(page, 'signs-in')
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  await page.locator('.caf-chat header').getByRole('button', { name: 'Close' }).click()
  await expect(page.locator('.caf-chat [data-state]')).toHaveText('Closed')
  const agent = row(page, 'signs-in')
  await agent.getByRole('button', { name: 'Sign in to signs-in' }).click()
  // A closed chat shows no sign-in, so the panel does: once.
  await expect(agent.getByRole('link', { name: 'the sign-in page on caf.example' })).toBeVisible()
  await expect(page.getByRole('textbox', { name: /paste it here/ })).toHaveCount(1)
  await expect(agent).not.toContainText('see the chat')
  await agent.getByRole('textbox', { name: /paste it here/ }).fill('code#state')
  await agent.getByRole('button', { name: 'Send the code' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', 'SignedIn')
  await noDivergence(page)
})
