// Live, what the fake agent's turns spend: it says it as Claude's adapter
// does (see caf-debugger's fake.rs), the same session cost every time, so
// a chat's first turn costs $0.0125 and its tokens are 24,928. The
// context ring's popover shows the chat's spend; the usage page, read from
// the usage ledger under the test's own $XDG_DATA_HOME, shows it once
// asked again; a price of the user's own is set and cleared there; and a
// viewer the host doesn't let read usage has no usage page, though the
// chat's own spend still shows. What goes wrong is said plainly: the host
// is played between the page and CAF for that.

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
const popover = (page: Page) => page.getByRole('region', { name: "This chat's usage" })
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
  await expect(usage(page)).toContainText('No chats in the last 7 days.')
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
  await expect(usage(page).locator('.sub')).toHaveText('1 chat · as agents reported it')
  const models = usage(page).locator('table tbody tr')
  await expect(models).toHaveCount(1)
  await expect(models.first().locator('td').nth(1)).toHaveText('small')
  await expect(models.first().locator('td').nth(4)).toHaveText('24.9K')

  await usage(page).getByRole('button', { name: 'Tokens', exact: true }).click()
  await expect(usage(page).locator('.big')).toHaveText('24.9K')

  await usage(page).getByRole('button', { name: 'Close usage' }).click()
  await expect(usage(page)).toHaveCount(0)
})

/** Opens the usage page from the bar, and asks again until it shows `cost`. */
async function usageShowing(page: Page, cost: string) {
  await page.getByRole('button', { name: 'Usage', exact: true }).click()
  await expect(async () => {
    await usage(page).getByRole('button', { name: 'Refresh' }).click()
    await expect(usage(page).locator('.big')).toHaveText(cost, { timeout: 1_000 })
  }).toPass({ timeout: 15_000 })
}

test('a price of your own is set, and cleared, from the usage page', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  // The second turn says the same session cost again, so it reported
  // nothing, and is estimated: the fake's model has no price, so it's
  // unpriced.
  await turn(page)
  await turn(page)
  await usageShowing(page, '$0.01')
  await expect(usage(page).locator('.sub')).toContainText('of tokens had no price')
  await usage(page).getByRole('button', { name: 'small' }).click()
  const card = usage(page).getByRole('group', { name: 'The price of small' })
  await expect(card).toContainText('CAF knows no price for small')

  await card.getByRole('button', { name: 'Set your own price' }).click()
  await card.getByRole('spinbutton', { name: 'Input' }).fill('1')
  await card.getByRole('spinbutton', { name: 'Cache read' }).fill('0.1')
  await card.getByRole('spinbutton', { name: 'Cache write' }).fill('1.25')
  await card.getByRole('spinbutton', { name: 'Output' }).fill('5')
  await card.getByRole('button', { name: 'Save' }).click()
  // Asked again at once: the second turn is priced now.
  await expect(card).toContainText('Your own price')
  await expect(card.locator('dl > div')).toHaveText(['Input$1', 'Cache read$0.10', 'Cache write$1.25', 'Output$5'])
  await expect(usage(page).locator('.big')).toHaveText('$0.03')
  await expect(usage(page).locator('.sub')).toHaveText('1 chat · API estimate ⓘ')
  // The chat's own spend is priced again too.
  await usage(page).getByRole('button', { name: 'Close usage' }).click()
  await ring(page).click()
  await expect(popover(page).locator('.cost')).toHaveText('$0.03 API estimate')
  await page.keyboard.press('Escape')

  await usageShowing(page, '$0.03')
  await usage(page).getByRole('button', { name: 'small' }).click()
  await card.getByRole('button', { name: "Use the table's price" }).click()
  await expect(card).toContainText('CAF knows no price for small')
  await expect(usage(page).locator('.big')).toHaveText('$0.01')
})

/**
 * Plays the host between the page and CAF on `/viewer`: each message
 * either way goes through `edit`, which may change it, answer it itself
 * (returning a message for the page), or pass it on as it is.
 */
async function between(page: Page, edit: { toPage?: (message: any) => any; toCaf?: (message: any) => { answer: any } | null }) {
  await page.routeWebSocket(/\/viewer\b/, (socket) => {
    const caf = socket.connectToServer()
    caf.onMessage((message) => {
      const parsed = typeof message === 'string' ? JSON.parse(message) : null
      socket.send(parsed === null ? message : JSON.stringify(edit.toPage?.(parsed) ?? parsed))
    })
    socket.onMessage((message) => {
      const parsed = typeof message === 'string' ? JSON.parse(message) : null
      const answered = parsed === null ? null : (edit.toCaf?.(parsed) ?? null)
      if (answered !== null) socket.send(JSON.stringify(answered.answer))
      else caf.send(message)
    })
  })
}

for (const [outcome, says] of [
  ['NotAllowed', "Usage couldn't be read: The host doesn't let this viewer do that."],
  ['NoLedger', "Usage couldn't be read: The app keeps no record of usage, so there is none to show."],
] as const) {
  test(`a summary refused ${outcome} is said plainly`, async ({ page, server }) => {
    await between(page, {
      toCaf: (message) =>
        message.type === 'Usage'
          ? { answer: { type: 'Done', value: { request: message.value.request, outcome: { type: outcome } } } }
          : null,
    })
    await page.goto(server.url)
    await page.getByRole('button', { name: 'Usage', exact: true }).click()
    await expect(usage(page).getByRole('alert')).toHaveText(says)
    await expect(usage(page).locator('.big')).toHaveCount(0)
  })
}

test('a summary cut short by a file that could not be read says so', async ({ page, server }) => {
  await between(page, {
    toPage: (message) =>
      message.type === 'Usage'
        ? { ...message, value: { ...message.value, summary: { ...message.value.summary, unreadable: ['eyespy/2026-10.jsonl.zst'] } } }
        : message,
  })
  await page.goto(server.url)
  await page.getByRole('button', { name: 'Usage', exact: true }).click()
  await expect(usage(page).getByRole('alert')).toHaveText(
    "Some of the usage record couldn't be read, so these figures are short: eyespy/2026-10.jsonl.zst.",
  )
  await expect(usage(page).locator('.big')).toHaveText('$0.00')
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
  await between(page, {
    toPage: (message) =>
      message.type === 'Welcome' ? { ...message, value: { ...message.value, may: { ...message.value.may, usage: false } } } : message,
  })
  await page.goto(server.url)
  await openChat(page)
  await turn(page)
  await expect(page.getByRole('button', { name: 'Usage', exact: true })).toHaveCount(0)
  await ring(page).click()
  await expect(popover(page).locator('.cost')).toHaveText('$0.01')
  await expect(popover(page).getByRole('button', { name: /Open usage/ })).toHaveCount(0)
})
