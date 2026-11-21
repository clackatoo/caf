// Live, choosing the fake agent's settings: it offers two models, three
// effort levels and fast mode, as Claude offers its own (see caf-debugger's
// fake.rs). A change made while idle applies at once; one made during a
// reply waits for it to end, and applies from the next message. Each turn
// says the model it was sent on.

import { expect, test, type Page } from './server.ts'

test.use({ serve: [] })

async function openChat(page: Page) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox', { name: /^Agent/ }).selectOption('fake')
  await page.getByRole('textbox', { name: /Folder/ }).fill(process.cwd())
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText('fake')
}

async function send(page: Page, text: string) {
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
}

/** A setting in the picker, by its kind: `Model`, `Effort` or `Fast`. */
const setting = (page: Page, kind: string) => page.locator(`.caf-settings [data-kind="${kind}"]`)

test("the picker shows the agent's models, effort levels and fast mode", async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  const model = setting(page, 'Model').locator('select')
  await expect(model.locator('option')).toHaveText(['Small', 'Large'])
  await expect(model).toHaveValue('small')
  await expect(setting(page, 'Model')).toContainText("The fake's own: it says the same whichever model")
  await expect(setting(page, 'Effort').locator('option')).toHaveText(['Low', 'Medium', 'High'])
  await expect(setting(page, 'Fast').getByRole('checkbox', { name: 'Fast mode' })).not.toBeChecked()
})

test('a model chosen while idle applies at once, and the next turn is sent on it', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  const model = setting(page, 'Model')
  await expect(model.locator('select')).toHaveValue('small')
  await model.locator('select').selectOption('large')
  // Taken, then answered: nothing waits.
  await expect(page.locator('.caf-settings')).toHaveAttribute('aria-busy', 'false')
  await expect(model.locator('.waiting')).toHaveText('')
  await expect(model.locator('select')).toHaveValue('large')
  await expect(model).toContainText("The fake's own, as large: it says the same")
  await send(page, 'hello')
  const turn = page.locator('.caf-chat .turn').last()
  await expect(turn.locator('.life')).toHaveText('Finished')
  await expect(turn.locator('.model')).toContainText('Large')
  await expect(page.locator('.diverged')).toHaveCount(0)
})

test('a model chosen during a reply applies from the next message, and each turn shows its own', async ({ page, server }) => {
  await page.goto(server.url)
  await openChat(page)
  await send(page, 'ask')
  const question = page.locator('.questions .question')
  await expect(question).toContainText('Run `echo hello`', { timeout: 15_000 })
  const model = setting(page, 'Model')
  await model.locator('select').selectOption('large')
  await expect(model.locator('.waiting')).toHaveText('Large, from your next message')
  await setting(page, 'Fast').getByRole('checkbox', { name: 'Fast mode' }).check()
  await expect(setting(page, 'Fast').locator('.waiting')).toHaveText('Fast mode on, from your next message')

  await question.getByRole('button', { name: 'Allow' }).click()
  const turns = page.locator('.caf-chat .turn')
  await expect(turns.last().locator('.life')).toHaveText('Finished')
  // Sent once the reply ended: the change no longer waits.
  await expect(model.locator('.waiting')).toHaveText('')
  await expect(setting(page, 'Fast').locator('.waiting')).toHaveText('')
  await expect(model.locator('select')).toHaveValue('large')
  await send(page, 'hello')
  await expect(turns).toHaveCount(2)
  await expect(turns.last().locator('.life')).toHaveText('Finished')
  await expect(turns.first().locator('.model')).toContainText('Small')
  await expect(turns.last().locator('.model')).toContainText('Large')
  await expect(page.locator('.diverged')).toHaveCount(0)
})

test('once CAF knows the models, a new chat can be opened on one, which its first turn is sent on', async ({ page, server }) => {
  await page.goto(server.url)
  // The first chat's session tells CAF what the agent offers.
  await openChat(page)
  await expect(setting(page, 'Model').locator('select')).toHaveValue('small')
  await page.getByRole('button', { name: 'New chat' }).click()
  const form = page.locator('.caf-chat-list form.new')
  await form.getByRole('combobox', { name: /^Agent/ }).selectOption('fake')
  await form.getByRole('textbox', { name: /Folder/ }).fill(process.cwd())
  const model = form.getByRole('combobox', { name: /^Model/ })
  await expect(model).toHaveValue('small')
  await model.selectOption('large')
  await form.getByRole('button', { name: 'Open' }).click()
  await expect(form).toHaveCount(0)
  await expect(setting(page, 'Model').locator('select')).toHaveValue('large')
  await send(page, 'hello')
  const turn = page.locator('.caf-chat .turn').last()
  await expect(turn.locator('.life')).toHaveText('Finished')
  await expect(turn.locator('.model')).toContainText('Large')
  await expect(page.locator('.diverged')).toHaveCount(0)
})
