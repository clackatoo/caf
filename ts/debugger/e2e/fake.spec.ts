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
  await expect(row(page, 'offline').getByRole('button', { name: 'Try again' })).toBeVisible()
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
  await expect(agent.getByRole('button', { name: 'Sign in' })).toBeVisible()

  await agent.getByRole('button', { name: 'Sign in' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', 'SigningIn')
  const link = agent.getByRole('link', { name: 'the sign-in page on caf.example' })
  await expect(link).toHaveAttribute('href', 'https://caf.example/sign-in?state=e2e')
  await expect(link).toHaveAttribute('target', '_blank')
  await expect(link).toHaveAttribute('rel', 'noopener noreferrer')
  await expect(agent.locator('code.code')).toHaveText('WXYZ-9876')
  // The chat shows it too: the sign-in is the agent's, not the panel's.
  await expect(page.locator('.caf-chat [data-agent-state="SigningIn"]')).toContainText('WXYZ-9876')

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
  await openChat(page, 'signs-in')
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  const agent = row(page, 'signs-in')
  const before = await agent.getAttribute('data-sign-in')
  await agent.getByRole('button', { name: 'Sign in' }).click()
  await expect(agent.locator('code.code')).toHaveText('WXYZ-9876')
  await agent.getByRole('button', { name: 'Stop signing in' }).click()
  await expect(agent).toHaveAttribute('data-sign-in', before!)
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
  await openChat(page, 'signs-in')
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  const agent = row(page, 'signs-in')
  await agent.getByRole('button', { name: 'Sign in' }).click()
  await agent.getByRole('textbox').fill('wrong')
  await agent.getByRole('button', { name: 'Send the code' }).click()
  await expect(agent).not.toHaveAttribute('data-sign-in', 'SigningIn')
  await expect(agent).not.toHaveAttribute('data-sign-in', 'SignedIn')
  await noDivergence(page)
})
