// Replaying a recording: the chat view draws it as it was at each step,
// read-only, and the timeline scrubs back and forth.

import { resolve } from 'node:path'
import { expect, test, traces } from './server.ts'

test.describe('replaying the live 0.98.0 recording', () => {
  test.use({ serve: ['--replay', resolve(traces, 'claude-0.98.0-live.jsonl')] })

  test('draws the chat, read-only, and scrubs back and forth', async ({ page, server }) => {
    await page.goto(server.url)
    // The token is out of the address at once.
    await expect(page).not.toHaveURL(/token/)
    await expect(page.locator('.mode')).toHaveText('Replay')

    const chat = page.locator('.caf-chat')
    await expect(chat.locator('.turn').first()).toBeVisible()
    await expect(chat.getByText('Reply with exactly the word pong.')).toBeVisible()
    await expect(chat.locator('.turn').first().locator('.markdown')).toContainText('pong')
    await expect(chat.locator('.turn').first().locator('.life')).toHaveText('Finished')
    await expect(chat.getByText('As it was then')).toBeVisible()
    await expect(page.getByRole('textbox', { name: 'Message' })).toBeDisabled()

    const position = page.getByRole('slider', { name: 'Position' })
    const end = Number(await position.getAttribute('max'))
    expect(end).toBeGreaterThan(10)

    // Back to before anything: no chats.
    await position.fill('0')
    // The chat chosen stays chosen, before it existed too.
    await expect(page.getByText("The chat chosen didn't exist at this step.")).toBeVisible()
    await expect(page.locator('.timeline .where')).toHaveText(`Step 0 of ${end}`)

    // Forward, by steps: the chat comes back as it was.
    await position.fill(String(Math.floor(end / 2)))
    await expect(page.locator('.timeline .where')).toHaveText(`Step ${Math.floor(end / 2)} of ${end}`)
    await position.fill(String(end))
    await expect(chat.locator('.turn').first().locator('.markdown')).toContainText('pong')
  })

  test('shows each step, the state with what it changed, and the change logs', async ({ page, server }) => {
    await page.goto(server.url)
    const position = page.getByRole('slider', { name: 'Position' })
    await expect(page.locator('.steps .row').first()).toBeVisible()
    // From the start, so the first rows are drawn.
    await position.fill('0')
    await page.locator('.steps .row').filter({ has: page.locator('.no', { hasText: /^3$/ }) }).click()
    await expect(page.locator('.timeline .where')).toHaveText(/^Step 3 of/)
    await expect(page.locator('.detail h3')).toContainText('Step 3')

    await page.getByRole('tab', { name: 'State' }).click()
    await expect(page.locator('.state .node').first()).toBeVisible()
    await page.getByRole('tab', { name: 'Changes' }).click()
    await position.fill(await position.getAttribute('max') ?? '0')
    await expect(page.locator('.changes h4').first()).toContainText('Chat')
  })
})

test.describe('a page from another build', () => {
  test.use({ serve: ['--replay', resolve(traces, 'claude-0.98.0-live.jsonl')] })

  test('says to rebuild, and goes no further, when the server speaks another protocol', async ({ page, server }) => {
    // The server's Hello, as from a later build.
    await page.routeWebSocket(/\/inspect/, (socket) => {
      const real = socket.connectToServer()
      real.onMessage((message) => {
        const parsed = JSON.parse(String(message)) as { type: string; value: { protocol?: number } }
        if (parsed.type === 'Hello') parsed.value.protocol = 999
        socket.send(JSON.stringify(parsed))
      })
    })
    await page.goto(server.url)
    const alert = page.getByRole('alert')
    await expect(alert).toContainText('This page is from a different build of caf-debugger')
    await expect(alert).toContainText('npm run build')
    await expect(page.locator('.caf-chat')).toHaveCount(0)
    await expect(page.getByRole('slider', { name: 'Position' })).toHaveCount(0)
  })
})
