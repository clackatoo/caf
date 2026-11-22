// Saving, live: a chat opened in one run of caf-debugger is saved, and the
// next run, started on the same folder, lists it, restores it with its
// record, and carries it on.

import { readdirSync, rmSync, statSync } from 'node:fs'
import { join } from 'node:path'

import { expect, start, stop, test, type Page } from './server.ts'

test.use({ serve: [] })

async function openChat(page: Page, cwd: string) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox', { name: /^Agent/ }).selectOption('fake')
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

    // Closed, it's archived and put away, and shown archived; restored again
    // at once, it comes back as soon as CAF has written it, still archived.
    await page.locator('.caf-chat header').getByRole('button', { name: 'Close' }).click()
    await expect(page.locator('.caf-chat > header .state')).toHaveText('Archived')
    await expect(row).toHaveAttribute('data-state', 'Saved')
    await expect(row).toContainText('archived')
    await row.getByRole('button', { name: 'Restore hello' }).click()
    await expect(row).toHaveAttribute('data-state', 'Loaded')
    await expect(turns).toHaveCount(2)
    await expect(saved.getByRole('alert')).toHaveCount(0)
    await expect(page.locator('.caf-chat > header .state')).toHaveText('Archived')
    await expect(page.getByText('This chat is archived. Sending a message brings it back.')).toBeVisible()
    await expect(page.getByRole('button', { name: 'Unarchive' })).toBeVisible()

    // Sent to, it's unarchived, and carries on.
    await send(page, 'once more')
    await expect(turns).toHaveCount(3)
    await expect(turns.last().locator('.life')).toHaveText('Finished')
    await expect(page.locator('.caf-chat > header .state')).not.toHaveText('Archived')
    await expect(page.getByRole('button', { name: 'Unarchive' })).toHaveCount(0)
  } finally {
    await stop(again)
  }
})

// Closed, a saved chat is put away too: Unarchive restores it, archived
// still, then unarchives it.
test('an archived chat, unarchived, is an ordinary chat again, and carries on', async ({ page, server }) => {
  await page.goto(server.url)
  await expect(page.locator('.link')).toHaveText('Connected')
  await openChat(page, process.cwd())
  await send(page, 'hello')
  const turns = page.locator('.caf-chat .turn')
  await expect(turns.last().locator('.life')).toHaveText('Finished')
  await page.locator('.caf-chat > header').getByRole('button', { name: 'Close' }).click()
  await expect(page.locator('.caf-chat > header .state')).toHaveText('Archived')
  await page.getByRole('button', { name: 'Unarchive' }).click()
  await expect(page.locator('.caf-chat > header .state')).not.toHaveText('Archived')
  await expect(page.getByRole('button', { name: 'Unarchive' })).toHaveCount(0)
  await send(page, 'again')
  await expect(turns).toHaveCount(2)
  await expect(turns.last().locator('.life')).toHaveText('Finished')
})

/** Every file under `dir`, with its size and when it last changed. */
function files(dir: string): string[] {
  return readdirSync(dir, { recursive: true, encoding: 'utf8' })
    .sort()
    .flatMap((name) => {
      const at = statSync(join(dir, name))
      return at.isFile() ? [`${name} ${at.size} ${at.mtimeMs}`] : []
    })
}

/**
 * Waits until a chat's whole save is under `dir`, a snapshot and its
 * header, and nothing there has changed for a while: CAF's write has ended.
 */
async function written(dir: string) {
  let last = ''
  await expect
    .poll(
      () => {
        const now = files(dir)
        const settled = now.some((each) => each.includes('.caf ')) && now.some((each) => each.includes('.head ')) && now.join('\n') === last
        last = now.join('\n')
        return settled
      },
      { intervals: [500], timeout: 15_000 },
    )
    .toBe(true)
}

/** Archives the chat shown, and waits until CAF has put it away. */
async function archive(page: Page) {
  await page.locator('.caf-chat > header').getByRole('button', { name: 'Close' }).click()
  await expect(page.locator('.caf-chat > header .state')).toHaveText('Archived')
  await expect(page.getByRole('region', { name: 'Saved chats' }).locator('li', { hasText: 'hello' })).toHaveAttribute('data-state', 'Saved')
}

// Sending to it brings it back: put away as it was archived, it's restored
// first, archived still, without the view going.
test('an archived chat, sent to at once, is brought back and carries on', async ({ page, server }) => {
  await page.goto(server.url)
  await expect(page.locator('.link')).toHaveText('Connected')
  await openChat(page, process.cwd())
  await send(page, 'hello')
  const turns = page.locator('.caf-chat .turn')
  await expect(turns.last().locator('.life')).toHaveText('Finished')
  await archive(page)
  const box = await page.getByRole('textbox', { name: 'Message' }).elementHandle()
  await send(page, 'and again')
  await expect(turns).toHaveCount(2)
  await expect(turns.last().locator('.message')).toHaveText('and again')
  // The view never went meanwhile: the box written in is the same one.
  expect(await box!.evaluate((element) => element.isConnected)).toBe(true)
  await expect(turns.last().locator('.life')).toHaveText('Finished')
  await expect(page.locator('.caf-chat > header .state')).not.toHaveText('Archived')
  await expect(page.getByText('This chat is archived.')).toHaveCount(0)
})

test("a message to an archived chat that can't be brought back stays, and says why", async ({ page, server }) => {
  await page.goto(server.url)
  await expect(page.locator('.link')).toHaveText('Connected')
  await openChat(page, process.cwd())
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn .life').last()).toHaveText('Finished')
  await archive(page)
  // Its save gone from under it, it can't be restored. CAF lists it as
  // saved before it has written it, so this waits for the write to end
  // first, or the write could fail, or put it back.
  const chats = join(server.state, 'caf-debugger', 'chats')
  await written(chats)
  for (const shelf of readdirSync(chats)) rmSync(join(chats, shelf), { recursive: true })
  await send(page, 'and again')
  await expect(page.locator('.composer [role=alert]')).toContainText("isn't on its shelf any more")
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveValue('and again')
  await expect(page.locator('.caf-chat > header .state')).toHaveText('Archived')
})
