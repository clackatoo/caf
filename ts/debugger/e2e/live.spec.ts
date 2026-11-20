// Live, with caf-debugger's fake agent: open a chat, send, watch it
// stream, answer a permission question, Stop, and a failed turn's Send
// again. Then look back at it on the timeline.

import { expect, test, type Page } from './server.ts'

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

test('sends a message, and the reply streams in', async ({ page, server }) => {
  await page.goto(server.url)
  await expect(page.locator('.mode')).toHaveText('Live')
  await expect(page.locator('.link')).toHaveText('Connected')
  await openChat(page, process.cwd())
  await send(page, 'hello')
  const turn = page.locator('.caf-chat .turn').last()
  await expect(turn.locator('.message')).toHaveText('hello')
  await expect(turn.locator('.markdown')).toContainText("CAF's fake agent")
  await expect(turn.locator('.life')).toHaveText('Finished')
  await expect(turn.locator('details.thinking')).toBeVisible()
})

test('answers a permission question, with none chosen beforehand', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, process.cwd())
  await send(page, 'ask')
  const question = page.locator('.questions .question')
  await expect(question).toContainText('Run `echo hello`', { timeout: 15_000 })
  await expect(question.locator('button')).toHaveText(['Allow', 'Reject'])
  await expect(question.locator('button:focus')).toHaveCount(0)
  await question.getByRole('button', { name: 'Allow' }).click()
  await expect(question).toHaveCount(0)
  const turn = page.locator('.caf-chat .turn').last()
  await expect(turn.locator('.markdown').last()).toContainText('Thanks for answering.')
  await expect(turn.locator('.life')).toHaveText('Finished')
})

test('stops a running turn', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, process.cwd())
  await send(page, 'ask')
  const turn = page.locator('.caf-chat .turn').last()
  await expect(turn.locator('.life')).toHaveText('Running')
  await page.getByRole('button', { name: 'Stop' }).click()
  await expect(turn.locator('.life')).toHaveText('Stopped')
})

test('a message sent while a turn runs waits for it', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, process.cwd())
  await send(page, 'ask')
  await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Running')
  await send(page, 'and then this')
  await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Waiting')
  await page.getByRole('button', { name: 'Stop' }).click()
  await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Finished', { timeout: 15_000 })
})

// The fake agent has no way to fail a turn yet: this waits on a message
// that makes it, from caf-debugger's fake script.
test.fixme('a failed turn offers Send again, which sends it again', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, process.cwd())
  await send(page, 'fail')
  const failed = page.locator('.caf-chat .turn').last()
  await expect(failed.locator('.life')).toHaveText('Failed')
  await expect(failed.locator('.failure .why')).toBeVisible()
  await failed.getByRole('button', { name: 'Send again' }).click()
  await expect(page.locator('.caf-chat .turn')).toHaveCount(2)
  await expect(page.locator('.caf-chat .turn').last().locator('.message')).toHaveText('fail')
})

test('looks back at the chat as it was, read-only, then follows again', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page, process.cwd())
  await send(page, 'hello')
  await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Finished')

  const position = page.getByRole('slider', { name: 'Position' })
  await page.getByRole('button', { name: 'Step back' }).click()
  await expect(page.locator('.timeline .where')).toHaveText(/^Step \d+ of \d+$/)
  await expect(page.getByText('As it was then')).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeDisabled()
  await position.fill('0')
  // The chat chosen stays chosen, before it existed too.
  await expect(page.getByText(/Chat \d+ didn't exist at this step\./)).toBeVisible()

  await page.getByRole('button', { name: 'Back to live' }).click()
  await expect(page.locator('.timeline .live')).toBeVisible()
  await expect(page.locator('.caf-chat .turn').last().locator('.markdown')).toContainText("CAF's fake agent")
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeEnabled()
})

test('warns before saving a bug report', async ({ page, server }) => {
  await page.goto(server.url)
  await page.getByRole('button', { name: 'Save bug report' }).click()
  const dialog = page.getByRole('dialog')
  await expect(dialog).toContainText('everything said')
  const download = page.waitForEvent('download')
  await dialog.getByRole('button', { name: 'Download it' }).click()
  expect((await download).suggestedFilename()).toMatch(/\.jsonl$/)
})
