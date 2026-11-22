// The agent's settings beside the composer: its model and effort as its
// own choices, in its order and groups, fast mode as a switch; a change
// waiting until the agent takes it; before the session opens, what CAF
// knows the agent offers, or learning it; and the model each turn used.

import type { AgentStatus, Setting, SettingChoice } from '@clackatoo/caf'
import { CafError } from '@clackatoo/caf'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import { openThenChoose } from '../../src/chat/actions.ts'
import ChatView from '../../src/chat/ChatView.svelte'
import { runs } from '../../src/chat/settings.ts'
import { choiceText, failureText, noteText, refusedText } from '../../src/chat/words.ts'
import { actions, chat, chatId, draw, item, life, NOON, status, turn } from './build.ts'

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
    draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }))
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
    const { actions } = draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }))
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
    draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }))
    const fast = screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement
    expect(fast.checked).toBe(false)
    expect(fast.closest('.setting')?.textContent).toContain('May cost more.')
  })

  it('shows a change waiting for the turn running as from the next message', () => {
    const { container } = draw(chat({ turns: [turn({ life: life('Running') })], settings: { offered: offered(), chosen: [sonnet], own: [{ type: 'Model' }] } }))
    expect(select('Model').value).toBe('sonnet')
    expect(container.querySelector('.waiting')?.textContent).toBe('Sonnet, from your next message')
  })

  it('shows a change waiting for the session to open as from the next message', () => {
    const { container } = draw(chat({ state: { type: 'Idle' }, settings: { offered: offered(), chosen: [sonnet], own: [{ type: 'Model' }] } }))
    expect(container.querySelector('.waiting')?.textContent).toBe('Sonnet, from your next message')
  })

  it('shows a change sent at once as changing, and nothing once the agent has taken it', async () => {
    const { container, rerender } = draw(chat({ settings: { offered: offered(), chosen: [sonnet], own: [{ type: 'Model' }] } }))
    expect(container.querySelector('.waiting')?.textContent).toBe('Changing to Sonnet…')
    await rerender({ chat: chat({ settings: { offered: offered('sonnet'), chosen: [sonnet], own: [{ type: 'Model' }] } }) })
    // The same live region, emptied: one put in with its text isn't read out.
    expect(container.querySelector('[data-kind=Model] .waiting')?.textContent).toBe('')
    expect(select('Model').value).toBe('sonnet')
  })

  it('names a switch waiting by the setting', () => {
    const fast: SettingChoice = { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }
    const { container } = draw(chat({ settings: { offered: offered(), chosen: [fast], own: [{ type: 'Fast' }] } }))
    expect((screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement).checked).toBe(true)
    expect(container.querySelector('[data-kind=Fast] .waiting')?.textContent).toBe('Turning fast mode on…')
  })

  it('still shows a choice the agent no longer lists', () => {
    const gone: SettingChoice = { kind: { type: 'Model' }, value: { type: 'Choice', value: 'claude-2' } }
    draw(chat({ state: { type: 'Idle' }, settings: { offered: offered(), chosen: [gone], own: [{ type: 'Model' }] } }))
    expect(select('Model').value).toBe('claude-2')
  })

  it("says why a choice wasn't taken, and goes back to what it was", async () => {
    const given = actions()
    given.setSetting.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'NoSuchSetting' } }))
    draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }), { actions: given })
    await fireEvent.change(select('Model'), { target: { value: 'haiku' } })
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe("The agent doesn't offer that setting, or that choice of it."))
    expect(select('Model').value).toBe('default')
  })

  it('keeps the control and its focus while a change is sent, saying the bar is busy, and puts back what is chosen meanwhile', async () => {
    const given = actions()
    let done = () => {}
    given.setSetting.mockImplementation(() => new Promise<void>((resolve) => (done = resolve)))
    draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }), { actions: given })
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

  it('puts a refused choice back as the setting is now, if it changed meanwhile', async () => {
    const given = actions()
    let refuse = (_: unknown) => {}
    given.setSetting.mockImplementation(() => new Promise<void>((_, reject) => (refuse = reject)))
    const { rerender } = draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }), { actions: given })
    await fireEvent.change(select('Model'), { target: { value: 'haiku' } })
    // The agent fell back to Sonnet by itself while the change was sent.
    await rerender({ chat: chat({ settings: { offered: offered('sonnet'), chosen: [], own: [] } }) })
    refuse(new CafError({ type: 'Refused', value: { type: 'NoSuchSetting' } }))
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    expect(select('Model').value).toBe('sonnet')
  })

  it('puts a switch back as it was if the change is refused', async () => {
    const given = actions()
    let refuse = (_: unknown) => {}
    given.setSetting.mockImplementation(() => new Promise<void>((_, reject) => (refuse = reject)))
    const fast: SettingChoice = { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }
    draw(chat({ settings: { offered: offered(), chosen: [fast], own: [{ type: 'Fast' }] } }), { actions: given })
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
    draw(chat({ settings: { offered: offered(), chosen: [sonnet], own: [{ type: 'Model' }] } }))
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
    draw(chat({ settings: { offered: offered(), chosen: [], own: [] } }), options)
    expect(select('Model').disabled).toBe(true)
    expect((screen.getByRole('checkbox', { name: 'Fast mode' }) as HTMLInputElement).disabled).toBe(true)
  })

  it('chooses nothing in an archived chat, as CAF refuses it, and again once it is unarchived', async () => {
    const settings = { offered: offered(), chosen: [], own: [] }
    const { rerender } = draw(chat({ state: { type: 'Closed' }, archived: NOON, settings }))
    expect(select('Model').disabled).toBe(true)
    await rerender({ chat: chat({ state: { type: 'Idle' }, archived: null, settings }) })
    expect(select('Model').disabled).toBe(false)
  })

  it("draws a setting of a kind it doesn't know by its form, and names a form it doesn't know", () => {
    const later = [
      { kind: { type: 'Mode' }, name: 'Mode', form: { type: 'Choices', value: [choice('plan', 'Plan')] }, current: null },
      { kind: { type: 'Voice' }, name: 'Voice', form: { type: 'Slider', value: { max: 9 } }, current: null },
    ] as never as Setting[]
    draw(chat({ settings: { offered: later, chosen: [], own: [] } }))
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
    const { container } = draw(chat({ state: { type: 'Opening' }, settings: { offered: [], chosen: [sonnet], own: [{ type: 'Model' }] } }), {
      agent: status({ type: 'Host' }, undefined, false, { type: 'Known', value: offered() }),
    })
    expect(select('Model').value).toBe('sonnet')
    expect(container.querySelector('[data-kind=Model] .waiting')?.textContent).toBe('Sonnet, from your next message')
  })

  it("learns what the agent offers, if it isn't known", async () => {
    const { actions, container } = draw(opening, { agent: status({ type: 'Host' }) })
    expect(container.querySelector('.caf-settings')?.textContent).toContain('shows once its session opens')
    await fireEvent.click(screen.getByRole('button', { name: 'Learn them now' }))
    expect(actions.learnSettings).toHaveBeenCalledOnce()
  })

  it.each([
    ['agents', { act: true, agents: false }],
    ['chats', { act: false, agents: true }],
  ])('offers no learning where the host lets it only watch the %s', (_, may) => {
    const { container } = draw(opening, { agent: status({ type: 'Host' }), may })
    expect(container.querySelector('.caf-settings')).toBeNull()
  })

  it('says how learning goes in one status, keeping Learn in its place, then moves focus to the first setting', async () => {
    const at = (settings: AgentStatus['settings']) => status({ type: 'Host' }, undefined, false, settings)
    const { container, rerender, actions } = draw(opening, { agent: at({ type: 'Unknown' }) })
    const said = container.querySelector('.caf-settings [role=status]')!
    const learn = screen.getByRole('button', { name: 'Learn them now' })
    learn.focus()
    await fireEvent.click(learn)
    expect(actions.learnSettings).toHaveBeenCalledOnce()
    await rerender({ agent: at({ type: 'Learning' }) })
    // The same status and button, the one's text changed and the other off.
    expect(container.querySelector('.caf-settings [role=status]')).toBe(said)
    expect(said.textContent).toContain('Learning what the agent offers')
    expect(screen.getByRole('button', { name: 'Learn them now' })).toBe(learn)
    expect(learn.getAttribute('aria-disabled')).toBe('true')
    await fireEvent.click(learn)
    expect(actions.learnSettings).toHaveBeenCalledOnce()
    await rerender({ agent: at({ type: 'Failed', value: { type: 'StartTimedOut' } }) })
    expect(container.querySelector('.caf-settings [role=status]')).toBe(said)
    expect(said.textContent).toBe(
      "CAF couldn't learn which models and settings claude offers. claude didn't start in time. It tries again when the chat opens, or now.",
    )
    await rerender({ agent: at({ type: 'Known', value: offered() }) })
    await waitFor(() => expect(document.activeElement).toBe(select('Model')))
  })

  it('says while it learns, and why it couldn\'t, offering to try again', async () => {
    const learning = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Learning' }) })
    expect(learning.container.querySelector('.caf-settings [role=status]')?.textContent).toContain('Learning what the agent offers')
    cleanup()
    const failed = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Failed', value: { type: 'CouldNotStart', value: { error: 'No such file or directory (os error 2)' } } }) })
    expect(failed.container.querySelector('.caf-settings [role=status]')?.textContent).toBe(
      "CAF couldn't learn which models and settings claude offers. claude couldn't be started. It tries again when the chat opens, or now.",
    )
    // What the system said, apart, as its own.
    expect(failed.container.querySelector('.caf-settings .said.system code')?.textContent).toBe('No such file or directory (os error 2)')
    await fireEvent.click(screen.getByRole('button', { name: 'Learn them now' }))
    expect(failed.actions.learnSettings).toHaveBeenCalledOnce()
  })

  it("quotes what the agent said refusing, as the agent's", () => {
    const { container } = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Failed', value: { type: 'Refused', value: { message: 'Internal error' } } }) })
    expect(container.querySelector('.caf-settings [role=status]')?.textContent).toContain('claude refused to open a chat.')
    expect(container.querySelector('.caf-settings .said.agent')?.textContent).toBe('claude said: “Internal error”')
  })

  it.each([
    [{ type: 'NotInstalled' }, "claude isn't installed.", true],
    [{ type: 'NotLookedYet' }, "CAF hadn't checked yet whether claude is installed.", true],
    [{ type: 'SigningIn' }, 'claude was being signed in to.', true],
    [{ type: 'SignInNeeded' }, 'claude wants you to sign in first.', true],
    [{ type: 'CouldNotStart', value: { error: '' } }, "claude couldn't be started.", true],
    [{ type: 'Refused', value: { message: '' } }, 'claude refused to open a chat.', true],
    [{ type: 'StartTimedOut' }, "claude didn't start in time.", true],
    [{ type: 'OpenTimedOut' }, "claude didn't open a chat in time.", true],
    [{ type: 'Unreadable' }, "claude's answer made no sense.", true],
    [{ type: 'Restarted' }, 'claude was restarted meanwhile.', true],
    [{ type: 'Stopped' }, 'claude stopped.', true],
    [{ type: 'ShutDown' }, 'The app was closing.', false],
    [{ type: 'Incompatible' }, "This version of claude can't talk to this app.", false],
    [{ type: 'OwnTools' }, "claude can't be used: its own tools can't be turned off.", false],
    [{ type: 'Moonset' }, "The reason is one this viewer doesn't know (Moonset).", true],
  ])("says why it couldn't learn them, %j, and offers to try again only where that could help", (why, says, again) => {
    const { container } = draw(opening, { agent: status({ type: 'Host' }, undefined, false, { type: 'Failed', value: why as never }) })
    expect(container.querySelector('.caf-settings [role=status]')?.textContent).toBe(
      `CAF couldn't learn which models and settings claude offers. ${says}${again ? ' It tries again when the chat opens, or now.' : ''}`,
    )
    expect(container.querySelector('.caf-settings .said')).toBeNull()
    expect(screen.queryByRole('button', { name: 'Learn them now' }) !== null).toBe(again)
  })

  it('says why learning failed to start', async () => {
    const given = actions()
    given.learnSettings.mockRejectedValue(new CafError({ type: 'NotAllowed' }))
    draw(opening, { agent: status({ type: 'Host' }), actions: given })
    await fireEvent.click(screen.getByRole('button', { name: 'Learn them now' }))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe("The host doesn't let this viewer do that."))
  })
})

describe('opening a chat with its settings', () => {
  const open = async () => chatId(9)
  const fast: SettingChoice = { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }

  it('opens it, then chooses each', async () => {
    const chosen: [string, SettingChoice][] = []
    const choose = async (chat: string, choice: SettingChoice) => void chosen.push([chat, choice])
    expect(await openThenChoose(open, choose, [sonnet, fast])).toEqual({ chat: chatId(9), failed: [] })
    expect(chosen).toEqual([
      [chatId(9), sonnet],
      [chatId(9), fast],
    ])
  })

  it('tries every choice, and resolves with the chat and each that couldn\'t be chosen, so it isn\'t opened again', async () => {
    const refused = new CafError({ type: 'Refused', value: { type: 'NoSuchSetting' } })
    const effort: SettingChoice = { kind: { type: 'Effort' }, value: { type: 'Choice', value: 'max' } }
    const tried: SettingChoice[] = []
    const choose = async (_: string, choice: SettingChoice) => {
      tried.push(choice)
      if (choice !== fast) throw refused
    }
    expect(await openThenChoose(open, choose, [sonnet, fast, effort])).toEqual({
      chat: chatId(9),
      failed: [
        { choice: sonnet, problem: refused },
        { choice: effort, problem: refused },
      ],
    })
    expect(tried).toEqual([sonnet, fast, effort])
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
    // By the agent's name for the choice, where it said one, else by its id.
    [{ type: 'AgentChanged', value: { choice: sonnet, name: 'Sonnet 4.6' } }, 'The agent changed by itself to the model Sonnet 4.6, and this chat follows it.'],
    [{ type: 'AgentChanged', value: { choice: sonnet, name: null } }, 'The agent changed by itself to the model sonnet, and this chat follows it.'],
    [
      { type: 'NotOffered', value: { choice: { kind: { type: 'Fast' }, value: { type: 'Switch', value: true } }, name: null } },
      "The agent doesn't offer fast mode, so that choice was dropped: the app's default, or else the agent's own, applies.",
    ],
    [
      { type: 'NotOffered', value: { choice: { kind: { type: 'Model' }, value: { type: 'Choice', value: 'claude-opus-4-8' } }, name: null } },
      "The agent doesn't offer the model claude-opus-4-8, so that choice was dropped: the app's default, or else the agent's own, applies.",
    ],
    [
      { type: 'NotApplied', value: { choice: { kind: { type: 'Effort' }, value: { type: 'Choice', value: 'max' } }, name: 'Max' } },
      "The agent refused the effort Max, so that choice was dropped: the app's default, or else the agent's own, applies.",
    ],
  ])('draws the note %j', (note, says) => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Note', value: note as never })] })] }))
    expect(container.querySelector('.note')?.textContent).toContain(says)
    expect(noteText(note as never)).toBe(says)
  })

  it('shows an id the agent never named in a monospace font, and a name as it is', () => {
    const named = { type: 'NotApplied', value: { choice: sonnet, name: 'Sonnet 4.6' } }
    const unnamed = { type: 'NotOffered', value: { choice: { kind: { type: 'Model' }, value: { type: 'Choice', value: 'claude-opus-4-8' } }, name: null } }
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Note', value: named as never }), item({ type: 'Note', value: unnamed as never })] })] }))
    const [first, second] = [...container.querySelectorAll('.note')]
    expect(first!.querySelector('code')).toBeNull()
    expect([...second!.querySelectorAll('code')].map((code) => code.textContent)).toEqual(['claude-opus-4-8'])
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
