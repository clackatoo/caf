// Live, what the fake agent's turns spend: it says it as Claude's adapter
// does (see caf-debugger's fake.rs), the same session cost every time, so
// a chat's first turn costs $0.0125 and its tokens are 24,928. The
// context ring's popover shows the chat's spend; the usage page, read from
// the usage ledger under the test's own $XDG_DATA_HOME, shows it once
// asked again; and a viewer the host doesn't let read usage has no usage
// page, though the chat's own spend still shows.

import { expect, test, type Page } from './server.ts'

test.use({ serve: [] })

async function openChat(page: Page) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox', { name: /^Agent/ }).selectOption('fake')
  await page.getByRole('textbox', { name: /Folder/ }).fill(process.cwd())
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText('fake')
}

/** Sends a message, and waits for the fake's answer to end. */
async function turn(page: Page, text = 'hello') {
  const turns = page.locator('.caf-chat .turn')
  const before = await turns.count()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
  await expect(turns).toHaveCount(before + 1)
  await expect(turns.last().locator('.life')).toHaveText('Finished')
}

const ring = (page: Page) => page.getByRole('button', { name: /This chat's usage/ })
const popover = (page: Page) => page.getByRole('dialog', { name: "This chat's usage" })
const usage = (page: Page) => page.getByRole('region', { name: 'Usage' })

test("a turn's spend shows in the ring's popover", async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  await ring(page).click()
  await expect(popover(page).locator('.cost')).toHaveText('Nothing yet')
  await ring(page).click()
  await turn(page)
  await expect(ring(page)).toHaveAccessibleName("This chat's usage: context 12% full")
  await ring(page).click()
  const shown = popover(page)
  await expect(shown.locator('.cost')).toHaveText('$0.01')
  await expect(shown).toContainText('As the agent reported it.')
  await expect(shown.locator('.processed')).toHaveText('24.9K')
  await expect(shown).toContainText('24.9K / 200K')
  // A second turn costs nothing more: the fake says the same session cost.
  await page.keyboard.press('Escape')
  await turn(page)
  await ring(page).click()
  await expect(shown.locator('.processed')).toHaveText('49.9K')
  await expect(shown.locator('.cost')).toHaveText('$0.01')
})

test('the usage page shows a turn once asked again', async ({ page, context, server }) => {
  await page.goto(server.url)
  await openChat(page)
  await page.getByRole('button', { name: 'Usage', exact: true }).click()
  await expect(usage(page)).toContainText('No chats in this period')
  await expect(usage(page).locator('.big')).toHaveText('$0.00')

  // Another page, as another viewer would, sends a turn meanwhile.
  const other = await context.newPage()
  await other.goto(server.url)
  await expect(other.locator('.caf-chat .agent')).toHaveText('fake')
  await turn(other)
  await other.close()

  // Asked again, until the ledger has the line.
  await expect(async () => {
    await usage(page).getByRole('button', { name: 'Refresh' }).click()
    await expect(usage(page).locator('.big')).toHaveText('$0.01', { timeout: 1_000 })
  }).toPass({ timeout: 15_000 })
  await expect(usage(page).locator('.sub')).toHaveText('1 chat · API estimate')
  const models = usage(page).locator('table tbody tr')
  await expect(models).toHaveCount(1)
  await expect(models.first().locator('td').nth(1)).toHaveText('small')
  await expect(models.first().locator('td').nth(4)).toHaveText('24.9K')

  await usage(page).getByRole('button', { name: 'Tokens', exact: true }).click()
  await expect(usage(page).locator('.big')).toHaveText('24.9K')

  // A model's price, and one of the user's own: the fake's model has none.
  await usage(page).getByRole('button', { name: 'small' }).click()
  await expect(usage(page).locator('.price')).toContainText('CAF knows no price for small')

  await usage(page).getByRole('button', { name: 'Close usage' }).click()
  await expect(usage(page)).toHaveCount(0)
})

test('the usage page splits by agent or by app', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  await turn(page)
  await ring(page).click()
  await popover(page).getByRole('button', { name: /Open usage/ }).click()
  const shares = usage(page).getByRole('list', { name: /^By / })
  await expect(async () => {
    await usage(page).getByRole('button', { name: 'Refresh' }).click()
    await expect(usage(page).locator('.big')).toHaveText('$0.01', { timeout: 1_000 })
  }).toPass({ timeout: 15_000 })
  await expect(shares).toHaveAccessibleName('By agent')
  await expect(shares.locator('li')).toHaveCount(1)
  await expect(shares.locator('li .name')).toHaveText('caf-fake-agent 1 chat')
  await usage(page).getByRole('button', { name: 'By app' }).click()
  await expect(shares).toHaveAccessibleName('By app')
  await expect(shares.locator('li .name')).toHaveText('caf-debugger 1 chat')
  await expect(usage(page).getByRole('img', { name: 'Cost by day' })).toBeVisible()
  // The day table, split by app too.
  await usage(page).getByRole('button', { name: 'Day', exact: true }).click()
  await usage(page).getByRole('button', { name: 'Split by app' }).click()
  await expect(usage(page).locator('table thead th').nth(1)).toHaveText('caf-debugger')
  await expect(usage(page).locator('table tbody tr')).toHaveCount(7)
})

test("a viewer the host doesn't let read usage has no usage page, but sees the chat's spend", async ({ page, server }) => {
  // The host's Welcome, as one that doesn't allow usage would say it.
  await page.routeWebSocket(/\/viewer\b/, (socket) => {
    const caf = socket.connectToServer()
    caf.onMessage((message) => {
      const parsed = typeof message === 'string' ? JSON.parse(message) : null
      if (parsed?.type === 'Welcome') parsed.value.may.usage = false
      socket.send(parsed === null ? message : JSON.stringify(parsed))
    })
  })
  await page.goto(server.url)
  await openChat(page)
  await turn(page)
  await expect(page.getByRole('button', { name: 'Usage', exact: true })).toHaveCount(0)
  await ring(page).click()
  await expect(popover(page).locator('.cost')).toHaveText('$0.01')
  await expect(popover(page).getByRole('button', { name: /Open usage/ })).toHaveCount(0)
})
