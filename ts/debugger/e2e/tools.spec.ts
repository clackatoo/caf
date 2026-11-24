// CAF's own tools, for real, through caf-debugger: a fake agent of the
// test's own script is an MCP client, as Claude is, and calls CAF's
// Read, Edit, Write, Bash and TaskStop, which read and write a folder of
// the test's own and run real commands. The page shows each: a read, an
// edit's diff before it's allowed, a command's output live, a progress
// bar redrawn in place, Stop, a job in the background, CAF's notice of
// its end, and TaskStop.

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { expect, test, type Page } from './server.ts'

/** The fake's session. */
const SESSION = 's1'

/** A step of the fake's script as a line. */
const line = (step: unknown) => JSON.stringify(step)

/** A pattern matching a prompt of exactly `text`. */
const prompt = (text: string) => ({ params: { prompt: [{ type: 'text', text }] } })

/**
 * A script that opens its session, starts CAF's tools and checks they're
 * all there, then for each round, waits for its message, does its steps,
 * and ends the turn. Any other message, such as CAF's notice of a job's
 * end, ends its turn at once. Stop ends the round's turn, cancelled.
 */
function script(rounds: { message: string; steps: unknown[] }[]): string {
  const lines: unknown[] = [
    { on: { method: 'session/cancel', do: [{ respond: { to: 'prompt', result: { stopReason: 'cancelled' } } }] } },
    { on: { method: 'session/prompt', fallback: true, do: [{ respond: { result: { stopReason: 'end_turn' } } }] } },
    { on: { fallback: true, do: [{ respond: { error: { code: -32601, message: 'Method not found' } } }] } },
    { wait: { method: 'initialize', as: 'prompt' } },
    { respond: { result: { protocolVersion: 1, agentCapabilities: {}, agentInfo: { name: 'caf-fake-agent', version: '0.0.0' } } } },
    { wait: { method: 'session/new' } },
    { respond: { result: { sessionId: SESSION } } },
    { mcp_start: { timeout: 20_000 } },
    { mcp_list: { expect: ['Read', 'Edit', 'Write', 'Bash', 'TaskStop'], timeout: 20_000 } },
  ]
  for (const round of rounds) {
    lines.push({ wait: { method: 'session/prompt', match: prompt(round.message), as: 'prompt' } })
    lines.push(...round.steps)
    lines.push({ respond: { to: 'prompt', result: { stopReason: 'end_turn' } } })
  }
  return lines.map(line).join('\n') + '\n'
}

let calls = 0
/** A call to CAF's `tool`, reported in the session first, as Claude's adapter does. */
const call = (tool: string, args: Record<string, unknown>) => ({
  mcp_call: { tool, arguments: args, tool_use_id: `toolu_${++calls}`, report: SESSION, as: `call${calls}` },
})
/** Waits for the last call's answer, checking it matches `expect`. */
const answered = (expect: unknown = {}) => ({ mcp_await: { expect, timeout: 60_000 } })

/** The debugger offering this script's fake, as `tools`, with a folder to work in. */
function setup(rounds: { message: string; steps: unknown[] }[], files: Record<string, string> = {}) {
  return {
    agents: `
[[agent]]
name = "tools"
program = "{fake}"
args = ["{state}/tools.jsonl"]
profile = "fake"
`,
    files: { 'tools.jsonl': script(rounds), 'work/.keep': '', ...Object.fromEntries(Object.entries(files).map(([path, text]) => [`work/${path}`, text])) },
  }
}

async function openChat(page: Page, cwd: string) {
  await page.getByRole('button', { name: 'New chat' }).click()
  await page.getByRole('combobox', { name: /^Agent/ }).selectOption('tools')
  await page.getByRole('textbox', { name: /Folder/ }).fill(cwd)
  await expect(page.getByRole('checkbox', { name: "CAF's tools" })).toBeChecked()
  await page.getByRole('button', { name: 'Open' }).click()
  await expect(page.locator('.caf-chat .agent')).toHaveText('tools')
}

async function send(page: Page, text: string) {
  const box = page.getByRole('textbox', { name: 'Message' })
  await box.fill(text)
  await box.press('Enter')
}

async function allow(page: Page) {
  await page.locator('.questions .question').getByRole('button', { name: 'Allow' }).click()
}

/** A step of the transcript, by its title. */
const step = (page: Page, title: string | RegExp) => page.locator('details.tool').filter({ has: page.locator('.title', { hasText: title }) })

test.describe('reading and editing', () => {
  test.use({
    setup: setup(
      [
        {
          message: 'go',
          steps: [
            call('Read', { file_path: 'notes.txt' }),
            answered({ result: { isError: false } }),
            call('Edit', { file_path: 'notes.txt', old_string: 'red', new_string: 'blue' }),
            answered({ result: { isError: false } }),
          ],
        },
      ],
      { 'notes.txt': 'The colour is red.\nAnd stays so.\n' },
    ),
  })
  test.setTimeout(90_000)

  test("reads a file, shows an edit's diff before it's allowed, and the file changes on disk", async ({ page, server }) => {
    const work = join(server.state, 'work')
    await page.goto(server.url)
    await openChat(page, work)
    await send(page, 'go')

    // Read asks first, as everything does in the debugger.
    const question = page.locator('.questions .question')
    await expect(question.locator('.ask')).toContainText('The agent wants to read a file: notes.txt')
    await allow(page)
    const read = step(page, 'Read notes.txt')
    await expect(read.locator('.status')).toHaveText('Done')
    await read.locator('summary').first().click()
    await expect(read.locator('.result')).toContainText('The colour is red.')
    await expect(read.locator('.locations')).toHaveText(`${work}/notes.txt`)

    // The edit's diff is in the question, before anything is written.
    await expect(question.locator('.ask')).toContainText('The agent wants to change a file: notes.txt')
    await expect(question.locator('.about')).toContainText('The colour is blue.')
    await expect(question.locator('.about')).toContainText('The colour is red.')
    expect(readFileSync(join(work, 'notes.txt'), 'utf8')).toBe('The colour is red.\nAnd stays so.\n')
    await allow(page)
    const edit = step(page, 'Edit notes.txt')
    await expect(edit.locator('.status')).toHaveText('Done')
    await expect(edit.locator('.decision').first()).toHaveText('Allowed by you')
    await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Finished')
    expect(readFileSync(join(work, 'notes.txt'), 'utf8')).toBe('The colour is blue.\nAnd stays so.\n')
  })
})

test.describe('writing', () => {
  test.use({
    setup: setup([
      {
        message: 'go',
        steps: [
          call('Write', { file_path: 'docs/new.md', content: '# New\n' }),
          answered({ result: { isError: false } }),
          call('Edit', { file_path: 'missing-read.txt', old_string: 'a', new_string: 'b' }),
          answered({ result: { isError: true } }),
        ],
      },
    ]),
  })
  test.setTimeout(90_000)

  test("writes a new file, its folder made, and fails an edit at once, asking nobody, when it can't be made", async ({ page, server }) => {
    const work = join(server.state, 'work')
    await page.goto(server.url)
    await openChat(page, work)
    await send(page, 'go')
    const question = page.locator('.questions .question')
    await expect(question.locator('.ask')).toContainText('The agent wants to write a file: docs/new.md')
    await expect(question.locator('.about')).toContainText('New file')
    await allow(page)
    await expect(step(page, 'Write docs/new.md').locator('.status')).toHaveText('Done')
    expect(readFileSync(join(work, 'docs/new.md'), 'utf8')).toBe('# New\n')

    const failed = step(page, 'Edit missing-read.txt')
    await expect(failed.locator('.status')).toHaveText('Failed')
    await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Finished')
    await expect(page.locator('.questions .question')).toHaveCount(0)
    await failed.locator('summary').first().click()
    await expect(failed.locator('.result')).toContainText('File does not exist.')
  })
})

/** A progress bar drawn in place, three times, then the line after it. */
const BAR = `for i in 1 2 3; do printf '\\r[%-3s] %d%%' "$(printf '%*s' $i '' | tr ' ' '#')" $((i * 33)); sleep 0.4; done; printf '\\nbuilt\\n'`

test.describe('running commands', () => {
  test.use({
    setup: setup([
      {
        message: 'go',
        steps: [
          call('Bash', { command: BAR, description: 'Build with a progress bar' }),
          answered({ result: { isError: false } }),
          call('Bash', { command: 'echo waiting; sleep 60; echo never' }),
          // Stopped: CAF answers it, cancelled, or not at all.
          { mcp_await: { timeout: 60_000 } },
        ],
      },
    ]),
  })
  test.setTimeout(120_000)

  test('shows output live, a progress bar redrawn in place, and Stop stops the command', async ({ page, server }) => {
    await page.goto(server.url)
    await openChat(page, join(server.state, 'work'))
    await send(page, 'go')
    const question = page.locator('.questions .question')
    await expect(question.locator('.about .command')).toContainText("printf '\\r[%-3s]")
    await allow(page)
    const bar = step(page, /^for i in 1 2 3/)
    await expect(bar.locator('.status')).toHaveText('Done')
    await expect(bar).toHaveAttribute('open', '')
    // The bar's three draws are one line, as the terminal showed it.
    await expect(bar).toContainText('[###] 99%')
    await expect(bar).toContainText('built')
    await expect(bar).not.toContainText('[#  ] 33%')
    await expect(bar).toContainText('Exited with code 0')

    await expect(question.locator('.about .command')).toHaveText('echo waiting; sleep 60; echo never')
    await allow(page)
    const waiting = step(page, 'echo waiting; sleep 60; echo never')
    await expect(waiting.locator('.status')).toHaveText('Running')
    // Live: its output, as it comes, before it ends.
    await expect(waiting.locator('.output')).toContainText('waiting')
    await page.getByRole('button', { name: 'Stop' }).click()
    await expect(waiting.locator('.status')).toHaveText('Cancelled')
    await expect(page.locator('.caf-chat .turn').last().locator('.life')).toHaveText('Stopped')
    await expect(waiting.locator('.output')).toContainText('waiting')
    await expect(waiting.locator('.output')).not.toContainText('never')
  })
})

test.describe('background jobs', () => {
  test.use({
    setup: setup([
      {
        message: 'go',
        steps: [
          call('Bash', { command: 'sleep 2; echo finished', run_in_background: true }),
          answered({ result: { isError: false } }),
          call('Bash', { command: 'echo serving; sleep 120', run_in_background: true }),
          answered({ result: { isError: false } }),
        ],
      },
      { message: 'stop it', steps: [call('TaskStop', { task_id: '2' }), answered({ result: { isError: false, content: [{ type: 'text', text: 'Stopped task 2' }] } })] },
    ]),
  })
  test.setTimeout(120_000)

  test("marks jobs Background, tells the agent of one's end in CAF's voice, and TaskStop stops one", async ({ page, server }) => {
    await page.goto(server.url)
    await openChat(page, join(server.state, 'work'))
    await send(page, 'go')
    const question = page.locator('.questions .question')
    await expect(question.locator('.about .folder')).toContainText(', in the background')
    await allow(page)
    const first = step(page, 'sleep 2; echo finished')
    await expect(first.locator('.job')).toHaveText('Background')
    await allow(page)
    const dev = step(page, 'echo serving; sleep 120')
    await expect(dev.locator('.job')).toHaveText('Background')
    const go = page.locator('.caf-chat .turn').filter({ has: page.locator('.message', { hasText: /^go$/ }) })
    await expect(go.locator('.life')).toHaveText('Finished')
    // Its turn is over; the jobs run on.
    await expect(dev.locator('.status')).toHaveText('Running')
    await expect(dev).toContainText('serving')

    // The first ends by itself: CAF tells the agent, in its own voice.
    await expect(first.locator('.status')).toHaveText('Done', { timeout: 30_000 })
    await expect(first.locator('.job')).toHaveCount(0)
    const notice = page.locator('.caf-chat .turn[data-starter="Notice"]')
    await expect(notice.locator('.starter')).toHaveText('CAF')
    await expect(notice.locator('.message')).toContainText('Background command "sleep 2; echo finished" completed (exit code 0)')
    await expect(notice.locator('.life')).toHaveText('Finished')

    // TaskStop, asking nobody, stops the other.
    await send(page, 'stop it')
    const stop = step(page, 'Stop task 2')
    await expect(stop.locator('.status')).toHaveText('Done')
    await expect(stop.locator('.kind')).toHaveText('Stop a task')
    await expect(dev.locator('.status')).toHaveText('Cancelled')
    await expect(dev.locator('.job')).toHaveCount(0)
  })
})
