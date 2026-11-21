// The agent's settings beside the composer: its model and effort as its
// own choices, in its order and groups, fast mode as a switch; a change
// waiting until the agent takes it; before the session opens, what CAF
// knows the agent offers, or learning it; and the model each turn used.

import type { Setting, SettingChoice } from '@clackatoo/caf'
import { CafError } from '@clackatoo/caf'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import { openThenChoose } from '../../src/chat/actions.ts'
import ChatView from '../../src/chat/ChatView.svelte'
import { runs } from '../../src/chat/settings.ts'
import { choiceText, failureText, noteText, refusedText } from '../../src/chat/words.ts'
import { actions, chat, chatId, draw, item, life, status, turn } from './build.ts'

afterEach(cleanup)

function choice(id: string, name: string, more: { description?: string; group?: string } = {}) {
  return { id, name, description: more.description ?? null, group: more.group ?? null }
}

/** What Claude offers: models, the first its `default`, then effort and fast mode. */
function offered(model = 'default'): Setting[] {
  return [
    {
      kind: { type: 'Model' },
      name: 'Model',
      form: {
        type: 'Choices',
        value: [
          choice('default', 'Default', { description: 'Opus 4.7, for most work' }),
          choice('sonnet', 'Sonnet', { description: 'Quick, for everyday tasks' }),
          choice('opus[1m]', 'Opus (1M context)', { group: 'Opus' }),
          choice('haiku', 'Haiku'),
          choice('opus', 'Opus', { group: 'Opus' }),
        ],
      },
      current: { type: 'Choice', value: model },
    },
    {
      kind: { type: 'Effort' },
      name: 'Effort',
      form: { type: 'Choices', value: [choice('low', 'Low'), choice('high', 'High')] },
      current: { type: 'Choice', value: 'high' },
    },
    { kind: { type: 'Fast' }, name: 'Fast mode', form: { type: 'Switch' }, current: { type: 'Switch', value: false } },
  ]
}

const sonnet: SettingChoice = { kind: { type: 'Model' }, value: { type: 'Choice', value: 'sonnet' } }

function select(name: string): HTMLSelectElement {
  return screen.getByRole('combobox', { name }) as HTMLSelectElement
}

function bar(): HTMLElement {
  return screen.getByRole('group', { name: "The agent's settings" })
}

describe("the agent's settings", () => {
  it('offers its models in its order, its groups as it gives them, saying what the one chosen is', () => {
    draw(chat({ settings: { offered: offered(), chosen: [] } }))
    const model = select('Model')
    // Opus's two choices are apart, as the agent put them, each in a group of its own.
    expect([...model.children].map((child) => (child instanceof HTMLOptGroupElement ? `[${child.label}]` : child.textContent))).toEqual([
      'Default',
      'Sonnet',
      '[Opus]',
      'Haiku',
      '[Opus]',
    ])
    expect([...model.options].map((option) => option.value)).toEqual(['default', 'sonnet', 'opus[1m]', 'haiku', 'opus'])
    // The agent's `default` is passed on as the agent names it.
    expect(model.value).toBe('default')
    expect(bar().textContent).toContain('Opus 4.7, for most work')
    expect(select('Effort').value).toBe('high')
  })

  it('chooses a model, effort and fast mode by the agent\'s own ids', async () => {
    const { actions } = draw(chat({ settings: { offered: offered(), chosen: [] } }))
    await fireEvent.change(select('Model'), { target: { value: 'sonnet' } })
    await waitFor(() => expect(actions.setSetting).toHaveBeenCalledWith(sonnet))
    await fireEvent.change(select('Effort'), { target: { value: 'low' } })
    await waitFor(() =>
      expect(actions.setSetting).toHaveBeenLastCalledWith({ kind: { type: 'Effort' }, value: { type: 'Choice', value: 'low' } }),
    )
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Fast mode' }))
    await waitFor(() => expect(actions.setSetting).toHaveBeenLastCalledWith({ kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }))
  })

  it('says fast mode may cost more', () => {
    draw(chat({ settings: { offered: offered(), chosen: [] } }))
    const fast = screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement
    expect(fast.checked).toBe(false)
    expect(fast.closest('.setting')?.textContent).toContain('May cost more.')
  })

  it('shows a change waiting for the turn running as from the next message', () => {
    const { container } = draw(chat({ turns: [turn({ life: life('Running') })], settings: { offered: offered(), chosen: [sonnet] } }))
    expect(select('Model').value).toBe('sonnet')
    expect(container.querySelector('.waiting')?.textContent).toBe('Sonnet, from your next message')
  })

  it('shows a change waiting for the session to open as from the next message', () => {
    const { container } = draw(chat({ state: { type: 'Idle' }, settings: { offered: offered(), chosen: [sonnet] } }))
    expect(container.querySelector('.waiting')?.textContent).toBe('Sonnet, from your next message')
  })

  it('shows a change sent at once as changing, and nothing once the agent has taken it', async () => {
    const { container, rerender } = draw(chat({ settings: { offered: offered(), chosen: [sonnet] } }))
    expect(container.querySelector('.waiting')?.textContent).toBe('Changing to Sonnet…')
    await rerender({ chat: chat({ settings: { offered: offered('sonnet'), chosen: [sonnet] } }) })
    // The same live region, emptied: one put in with its text isn't read out.
    expect(container.querySelector('[data-kind=Model] .waiting')?.textContent).toBe('')
    expect(select('Model').value).toBe('sonnet')
  })

  it('names a switch waiting by the setting', () => {
    const fast: SettingChoice = { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }
    const { container } = draw(chat({ settings: { offered: offered(), chosen: [fast] } }))
    expect((screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement).checked).toBe(true)
    expect(container.querySelector('[data-kind=Fast] .waiting')?.textContent).toBe('Turning fast mode on…')
  })

  it('still shows a choice the agent no longer lists', () => {
    const gone: SettingChoice = { kind: { type: 'Model' }, value: { type: 'Choice', value: 'claude-2' } }
    draw(chat({ state: { type: 'Idle' }, settings: { offered: offered(), chosen: [gone] } }))
    expect(select('Model').value).toBe('claude-2')
  })

  it("says why a choice wasn't taken, and goes back to what it was", async () => {
    const given = actions()
    given.setSetting.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'NoSuchSetting' } }))
    draw(chat({ settings: { offered: offered(), chosen: [] } }), { actions: given })
    await fireEvent.change(select('Model'), { target: { value: 'haiku' } })
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe("The agent doesn't offer that setting, or that choice of it."))
    expect(select('Model').value).toBe('default')
  })

  it('keeps the control and its focus while a change is sent, saying the bar is busy, and puts back what is chosen meanwhile', async () => {
    const given = actions()
    let done = () => {}
    given.setSetting.mockImplementation(() => new Promise<void>((resolve) => (done = resolve)))
    draw(chat({ settings: { offered: offered(), chosen: [] } }), { actions: given })
    const model = select('Model')
    model.focus()
    await fireEvent.change(model, { target: { value: 'sonnet' } })
    expect(bar().getAttribute('aria-busy')).toBe('true')
    expect(model.disabled).toBe(false)
    expect(document.activeElement).toBe(model)
    // Another choice while it's sent isn't sent, and the control shows the one that is.
    await fireEvent.change(model, { target: { value: 'haiku' } })
    await fireEvent.change(select('Effort'), { target: { value: 'low' } })
    expect(given.setSetting).toHaveBeenCalledOnce()
    expect(model.value).toBe('sonnet')
    expect(select('Effort').value).toBe('high')
    done()
    await waitFor(() => expect(bar().getAttribute('aria-busy')).toBe('false'))
  })

  it('puts a switch back as it was if the change is refused', async () => {
    const given = actions()
    let refuse = (_: unknown) => {}
    given.setSetting.mockImplementation(() => new Promise<void>((_, reject) => (refuse = reject)))
    const fast: SettingChoice = { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }
    draw(chat({ settings: { offered: offered(), chosen: [fast] } }), { actions: given })
    const box = screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement
    expect(box.checked).toBe(true)
    await fireEvent.click(box)
    // A second click while it's sent is put back, not flipped twice.
    await fireEvent.click(box)
    expect(box.checked).toBe(false)
    refuse(new CafError({ type: 'Refused', value: { type: 'NoSuchSetting' } }))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain("doesn't offer that setting"))
    expect(box.checked).toBe(true)
  })

  it('links each control to what it says of itself and its change waiting', () => {
    draw(chat({ settings: { offered: offered(), chosen: [sonnet] } }))
    const said = (control: HTMLElement) =>
      control
        .getAttribute('aria-describedby')!
        .split(' ')
        .map((id) => document.getElementById(id)?.textContent)
    expect(said(select('Model'))).toEqual(['Quick, for everyday tasks', 'Changing to Sonnet…'])
    expect(said(select('Effort'))).toEqual([''])
    expect(said(screen.getByRole('checkbox', { name: 'Fast mode' }))).toEqual(['May cost more.', ''])
  })

  it.each([
    ['looking back', { readOnly: true }],
    ['the host lets it only watch', { may: { act: false, agents: true } }],
  ])('chooses nothing while %s', (_, options) => {
    draw(chat({ settings: { offered: offered(), chosen: [] } }), options)
    expect(select('Model').disabled).toBe(true)
    expect((screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement).disabled).toBe(true)
  })

  it('chooses nothing in a closed chat', () => {
    draw(chat({ state: { type: 'Closed' }, settings: { offered: offered(), chosen: [] } }))
    expect(select('Model').disabled).toBe(true)
  })

  it("draws a setting of a kind it doesn't know by its form, and names a form it doesn't know", () => {
    const later = [
      { kind: { type: 'Mode' }, name: 'Mode', form: { type: 'Choices', value: [choice('plan', 'Plan')] }, current: null },
      { kind: { type: 'Voice' }, name: 'Voice', form: { type: 'Slider', value: { max: 9 } }, current: null },
    ] as never as Setting[]
    draw(chat({ settings: { offered: later, chosen: [] } }))
    expect(select('Mode').value).toBe('')
    expect(bar().textContent).toContain("Voice A kind of setting this viewer can't show (Slider).")
  })

  it('offers nothing for an agent that offers nothing', () => {
    const { container } = draw(chat())
    expect(container.querySelector('.caf-settings')).toBeNull()
  })
})

describe('before the session opens', () => {
  const opening = chat({ state: { type: 'Opening' } })

  it("offers what CAF knows the agent offers, choosing it for the session's opening", async () => {
    const { actions } = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Known', value: offered() }) })
    await fireEvent.change(select('Model'), { target: { value: 'sonnet' } })
    await waitFor(() => expect(actions.setSetting).toHaveBeenCalledWith(sonnet))
  })

  it('shows a choice made before the session opens as from the next message', () => {
    const { container } = draw(chat({ state: { type: 'Opening' }, settings: { offered: [], chosen: [sonnet] } }), {
      agent: status({ type: 'Host' }, undefined, false, { type: 'Known', value: offered() }),
    })
    expect(select('Model').value).toBe('sonnet')
    expect(container.querySelector('[data-kind=Model] .waiting')?.textContent).toBe('Sonnet, from your next message')
  })

  it("learns what the agent offers, if it isn't known", async () => {
    const { actions, container } = draw(opening, { agent: status({ type: 'Host' }) })
    expect(container.querySelector('.caf-settings')?.textContent).toContain('shows once its session opens')
    await fireEvent.click(screen.getByRole('button', { name: 'Learn it now' }))
    expect(actions.learnSettings).toHaveBeenCalledOnce()
  })

  it.each([
    ['agents', { act: true, agents: false }],
    ['chats', { act: false, agents: true }],
  ])('offers no learning where the host lets it only watch the %s', (_, may) => {
    const { container } = draw(opening, { agent: status({ type: 'Host' }), may })
    expect(container.querySelector('.caf-settings')).toBeNull()
  })

  it('says while it learns, and why it couldn\'t, offering to try again', async () => {
    const learning = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Learning' }) })
    expect(learning.container.querySelector('.caf-settings [role=status]')?.textContent).toContain('Learning what the agent offers')
    cleanup()
    const failed = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Failed', value: 'It wouldn’t start.' }) })
    expect(failed.container.querySelector('.caf-settings')?.textContent).toContain("couldn't be learned: It wouldn’t start.")
    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }))
    expect(failed.actions.learnSettings).toHaveBeenCalledOnce()
  })

  it('says why learning failed to start', async () => {
    const given = actions()
    given.learnSettings.mockRejectedValue(new CafError({ type: 'NotAllowed' }))
    draw(opening, { agent: status({ type: 'Host' }), actions: given })
    await fireEvent.click(screen.getByRole('button', { name: 'Learn it now' }))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe("The host doesn't let this viewer do that."))
  })
})

describe('opening a chat with its settings', () => {
  const open = async () => chatId(9)
  const fast: SettingChoice = { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }

  it('opens it, then chooses each', async () => {
    const chosen: [string, SettingChoice][] = []
    const choose = async (chat: string, choice: SettingChoice) => void chosen.push([chat, choice])
    expect(await openThenChoose(open, choose, [sonnet, fast])).toEqual({ chat: chatId(9), problem: null })
    expect(chosen).toEqual([
      [chatId(9), sonnet],
      [chatId(9), fast],
    ])
  })

  it('resolves with the chat, and why, if its settings couldn\'t be chosen, so it isn\'t opened again', async () => {
    const refused = new CafError({ type: 'Refused', value: { type: 'NoSuchSetting' } })
    const choose = async () => Promise.reject(refused)
    expect(await openThenChoose(open, choose, [sonnet, fast])).toEqual({ chat: chatId(9), problem: refused })
  })

  it("rejects if it didn't open", async () => {
    const notAllowed = new CafError({ type: 'NotAllowed' })
    await expect(openThenChoose(async () => Promise.reject(notAllowed), async () => {}, [sonnet])).rejects.toBe(notAllowed)
  })

  it("shows the host's notice in the chat", () => {
    const { container } = render(ChatView, {
      chat: chat(),
      actions: actions(),
      notice: "The chat is open, but its settings couldn't be chosen.",
    })
    expect(container.querySelector('[role=alert]')?.textContent).toBe("The chat is open, but its settings couldn't be chosen.")
  })
})

describe('the model each turn used', () => {
  it('shows it quietly in the turn, by name, or by id where the agent gave none', () => {
    const { container } = draw(
      chat({
        turns: [turn({ model: { id: 'sonnet', name: 'Sonnet 4.6' } }), turn({ model: { id: 'haiku', name: null } }), turn()],
      }),
    )
    expect([...container.querySelectorAll('.turn')].map((each) => each.querySelector('.model')?.textContent ?? null)).toEqual([
      'Model: Sonnet 4.6',
      'Model: haiku',
      null,
    ])
  })
})

describe('the words for settings', () => {
  it.each([
    [{ type: 'AgentChanged', value: sonnet }, 'The agent changed by itself to the model sonnet, and this chat follows it.'],
    [
      { type: 'NotOffered', value: { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } } },
      "The agent no longer offers fast mode, so that choice was dropped, and the agent's own applies.",
    ],
  ])('draws the note %j', (note, says) => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Note', value: note as never })] })] }))
    expect(container.querySelector('.note')?.textContent).toContain(says)
    expect(noteText(note as never)).toBe(says)
  })

  it('names a choice of every kind, and one it doesn\'t know by its tag', () => {
    expect(choiceText({ kind: { type: 'Effort' }, value: { type: 'Choice', value: 'high' } })).toBe('the effort high')
    expect(choiceText({ kind: { type: 'Mode' }, value: { type: 'Choice', value: 'plan' } } as never)).toBe(
      "a setting this viewer doesn't know (Mode), plan",
    )
  })

  it('says a message failed for a setting that couldn\'t be applied, and offers to send it again', async () => {
    const failed = life('Failed', { reason: { type: 'SettingNotApplied' }, agent_said: null, remedy: { type: 'SendAgain' } })
    const { container, actions } = draw(chat({ turns: [turn({ life: failed })] }))
    expect(container.querySelector('.failure .why')?.textContent).toContain(failureText({ type: 'SettingNotApplied' }))
    expect(failureText({ type: 'SettingNotApplied' })).toContain("couldn't be applied, so CAF didn't send the message")
    await fireEvent.click(screen.getByRole('button', { name: 'Send again' }))
    expect(actions.sendAgain).toHaveBeenCalledOnce()
  })

  it('says the agent offers no such setting', () => {
    expect(refusedText({ type: 'NoSuchSetting' })).toBe("The agent doesn't offer that setting, or that choice of it.")
  })

  it('keeps runs of the same group together, and a group split apart split', () => {
    const groups = runs([choice('a', 'A'), choice('b', 'B'), choice('c', 'C', { group: 'X' }), choice('d', 'D', { group: 'X' }), choice('e', 'E')])
    expect(groups.map((run) => [run.group, run.choices.map((each) => each.id)])).toEqual([
      [null, ['a', 'b']],
      ['X', ['c', 'd']],
      [null, ['e']],
    ])
  })
})
