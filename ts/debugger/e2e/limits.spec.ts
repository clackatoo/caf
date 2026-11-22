// Live, the fake agent's account limits, with `--fake-limits`: it says
// them as Claude's adapter does (see caf-debugger's fake.rs), its five
// hours climbing 60%, 67%, 74%, 81%, 88% and 95% used, turn by turn, its
// week and Opus's week more slowly. The Agents panel shows the lowest at
// a glance, fine, then low, then out, and each window opened; `limit`
// has the five hours refused, and the turn fails. Without the flag, the
// fake says none.

import { expect, test, type Page } from './server.ts'

async function openChat(page: Page) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox', { name: /^Agent/ }).selectOption('fake')
  await page.getByRole('textbox', { name: /Folder/ }).fill(process.cwd())
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText('fake')
}

/** Sends a message, and waits for the fake's answer to end as `life`. */
async function turn(page: Page, text = 'hello', life = 'Finished') {
  const turns = page.locator('.caf-chat .turn')
  const before = await turns.count()
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
  await expect(turns).toHaveCount(before + 1)
  await expect(turns.last().locator('.life')).toHaveText(life)
}

const fake = (page: Page) => page.locator('[data-agent="fake"]')
const limits = (page: Page) => fake(page).locator('details.limits')
const glance = (page: Page) => page.getByLabel("fake's limits")
const window = (page: Page, name: string) => fake(page).locator(`[data-window="${name}"]`)

test.describe('with --fake-limits', () => {
  test.use({ serve: ['--fake-limits'] })

  test("the fake's limits show in the Agents panel, climbing turn by turn", async ({ page, server }) => {
    await page.goto(server.url)
    await openChat(page)
    await expect(fake(page)).toBeVisible()
    await expect(limits(page)).toHaveCount(0)
    await turn(page)
    // The five hours are the lowest: 40% left, fine.
    await expect(glance(page)).toHaveText('5-hour: 40% left')
    await expect(limits(page)).toHaveAttribute('data-lowness', 'fine')
    await glance(page).click()
    await expect(fake(page).locator('[data-window]')).toHaveCount(3)
    await expect(window(page, '5-hour')).toContainText('40% left')
    await expect(window(page, '5-hour')).toContainText(/Allowed, resets in 2 h 5\d min, as of /)
    // Two of the five hours gone: at an even pace, 60% would be left.
    await expect(window(page, '5-hour').locator('.pace')).toHaveAttribute('data-left', '60')
    await expect(window(page, '5-hour').getByRole('meter')).toHaveAttribute('aria-valuenow', '40')
    await expect(window(page, 'Weekly')).toContainText(/70% left\s+Allowed, resets in 3 d 23 h/)
    await expect(window(page, 'Weekly · Opus')).toContainText('80% left')
    // Three more turns: past 80% used, the agent warns, and it's low.
    for (let n = 0; n < 3; n++) await turn(page)
    await expect(glance(page)).toHaveText('5-hour: 19% left')
    await expect(limits(page)).toHaveAttribute('data-lowness', 'low')
    await expect(window(page, '5-hour')).toContainText('Allowed, but near the limit')
    await expect(window(page, 'Weekly')).toContainText('64% left')
    // Two more: out.
    for (let n = 0; n < 2; n++) await turn(page)
    await expect(glance(page)).toHaveText('5-hour: 5% left')
    await expect(limits(page)).toHaveAttribute('data-lowness', 'out')
  })

  test('`limit` has the five hours refused, and the turn fails', async ({ page, server }) => {
    await page.goto(server.url)
    await openChat(page)
    await turn(page)
    await expect(limits(page)).toHaveAttribute('data-lowness', 'fine')
    await turn(page, 'limit', 'Failed')
    await expect(glance(page)).toHaveText('5-hour: 0% left')
    await expect(limits(page)).toHaveAttribute('data-lowness', 'out')
    await glance(page).click()
    await expect(window(page, '5-hour')).toContainText('Refused until it resets')
    await expect(window(page, '5-hour')).toHaveAttribute('data-state', 'Refused')
    // Sent again, answered, and allowed again.
    await turn(page, 'limit')
    await expect(window(page, '5-hour')).toHaveAttribute('data-state', 'Fine')
    await expect(glance(page)).toHaveText('5-hour: 33% left')
  })
})

test('without --fake-limits, the fake says none', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  await turn(page)
  await turn(page, 'limit')
  await expect(fake(page)).toBeVisible()
  await expect(limits(page)).toHaveCount(0)
})
