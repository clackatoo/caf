// Saving, live: a chat opened in one run of caf-debugger is saved, and the
// next run, started on the same folder, lists it, restores it with its
// record, and carries it on.

import { expect, start, stop, test, type Page } from './server.ts'

test.use({ serve: [] })

async function openChat(page: Page, cwd: string) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox').selectOption('fake')
  await page.getByRole('textbox', { name: /Folder/ }).fill(cwd)
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText('fake')
}

async function send(page: Page, text: string) {
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
}

test('a chat saved in one run is restored in the next, and carries on', async ({ page, server }) => {
  await page.goto(server.url)
  await expect(page.locator('.link')).toHaveText('Connected')
  await openChat(page, process.cwd())
  await send(page, 'hello')
  const turns = page.locator('.caf-chat .turn')
  await expect(turns.last().locator('.life')).toHaveText('Finished')
  const saved = page.getByRole('region', { name: 'Saved chats' })
  await expect(saved.locator('li', { hasText: 'hello' })).toHaveAttribute('data-state', 'Loaded')

  // Stopped, it saves the chat whole; started again, it lists it.
  await stop(server)
  const again = await start([], { state: server.state })
  try {
    await page.goto(again.url)
    await expect(page.locator('.link')).toHaveText('Connected')
    const row = saved.locator('li', { hasText: 'hello' })
    await expect(row).toHaveAttribute('data-state', 'Saved')
    await expect(page.locator('.caf-chat .turn')).toHaveCount(0)
    await row.getByRole('button', { name: 'Restore hello' }).click()
    await expect(row).toHaveAttribute('data-state', 'Loaded')

    // Its record, as it was.
    await expect(turns).toHaveCount(1)
    await expect(turns.first().locator('.message')).toHaveText('hello')
    await expect(turns.first().locator('.words .markdown')).toContainText("CAF's fake agent")
    await expect(turns.first().locator('.life')).toHaveText('Finished')

    // And it carries on.
    await send(page, 'again')
    await expect(turns).toHaveCount(2)
    await expect(turns.last().locator('.message')).toHaveText('again')
    await expect(turns.last().locator('.life')).toHaveText('Finished')
    await expect(turns.last().locator('.words .markdown')).toContainText("CAF's fake agent")

    // Closed, it's put away, and shown closed; restored again at once, it
    // comes back as soon as CAF has written it.
    await page.locator('.caf-chat header').getByRole('button', { name: 'Close' }).click()
    await expect(page.locator('.caf-chat [data-state]')).toHaveText('Closed')
    await expect(row).toHaveAttribute('data-state', 'Saved')
    await expect(row).toContainText('closed')
    await row.getByRole('button', { name: 'Restore hello' }).click()
    await expect(row).toHaveAttribute('data-state', 'Loaded')
    await expect(turns).toHaveCount(2)
    await expect(saved.getByRole('alert')).toHaveCount(0)
  } finally {
    await stop(again)
  }
})
