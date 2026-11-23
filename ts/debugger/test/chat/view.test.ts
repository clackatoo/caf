// The chat view draws everything in a chat's record, says CAF's reasons
// in CAF's words, names a reason it doesn't know by its tag, and does
// nothing while read-only.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type Spend } from '@clackatoo/caf'
import ChatView from '../../src/chat/ChatView.svelte'
import { actions, chat, chatId, draw, item, life, lost, NOON, question, turn, words } from './build.ts'

afterEach(cleanup)

/** The view's text, with runs of white space made one. */
function text(container: HTMLElement): string {
  return (container.textContent ?? '').replace(/\s+/g, ' ')
}

function button(name: string | RegExp): HTMLButtonElement {
  return screen.getByRole('button', { name }) as HTMLButtonElement
}

describe('the chat state', () => {
  it('says the chat is opening', () => {
    const { container } = draw(chat({ state: { type: 'Opening' } }))
    expect(text(container)).toContain('Opening the session')
  })

  it('says nothing more while open', () => {
    const { container } = draw(chat())
    expect(container.querySelector('[role=status]')).toBeNull()
  })

  it('says calmly that a chat with no session is continued by sending to it', () => {
    const { container } = draw(chat({ state: { type: 'Idle' } }))
    const banner = container.querySelector('[role=status]')!
    expect(banner.textContent).toBe('Not running: the next message starts it again, continuing where the agent can.')
    expect(banner.classList.contains('lost')).toBe(false)
    expect(container.querySelector('header .state')?.textContent).toBe('Not running')
    expect((screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement).disabled).toBe(false)
    expect(button('Close').disabled).toBe(false)
  })

  it('says a closed chat is archived, once, and offers no Close', () => {
    const { container } = draw(chat({ state: { type: 'Closed' }, archived: NOON }))
    expect(text(container).match(/This chat is archived\./g)).toHaveLength(1)
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull()
    cleanup()
    // Closed by a CAF from before archiving, with no time to say.
    const old = draw(chat({ state: { type: 'Closed' } }))
    expect(text(old.container)).toContain('This chat is archived.')
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull()
  })

  const reasons: [unknown, string[]][] = [
    [{ type: 'CouldNotStart', value: { error: 'No such file' } }, ["The agent couldn't be started.", 'The system said:', 'No such file']],
    [{ type: 'AgentStopped', value: { stderr: ['panic: oh no', 'at main'] } }, ['The agent stopped.', 'panic: oh no', 'at main']],
    [{ type: 'Restarted' }, ['The agent was restarted']],
    [{ type: 'SignInNeeded' }, ['The agent wants you to sign in first.']],
    [{ type: 'Refused', value: { message: 'Not today' } }, ['The agent refused to open the session.', 'The agent said:', 'Not today']],
    [{ type: 'Stopped' }, ['You stopped it opening.']],
    [{ type: 'Incompatible', value: { type: 'Protocol', value: { version: 9 } } }, ['speaks version 9 of ACP']],
    [{ type: 'Incompatible', value: { type: 'Protocol', value: { version: null } } }, ["speaks a version of ACP that CAF doesn't"]],
    [{ type: 'Incompatible', value: { type: 'HttpMcp' } }, ["can't reach MCP servers over HTTP"]],
    [{ type: 'Incompatible', value: { type: 'BadAnswer' } }, ['its answer made no sense']],
    [{ type: 'Incompatible', value: { type: 'OwnTools' } }, ["CAF can't turn off its own tools"]],
    [{ type: 'Incompatible', value: { type: 'Telepathy' } }, ["a reason this viewer doesn't know (Telepathy)"]],
    [{ type: 'InUse' }, ['Another chat already has this session open']],
    [{ type: 'ShutDown' }, ['CAF was shut down.']],
    [{ type: 'Eclipse' }, ["a reason this viewer doesn't know (Eclipse)"]],
  ]
  for (const [reason, says] of reasons) {
    it(`says why it was lost: ${JSON.stringify(reason)}`, () => {
      const { container } = draw(chat({ state: lost(reason) }))
      for (const words of says) expect(text(container)).toContain(words)
    })
  }

  it("names a state it doesn't know by its tag", () => {
    const { container } = draw(chat({ state: { type: 'Hibernating' } as never }))
    expect(text(container)).toContain("a state this viewer doesn't know (Hibernating)")
  })
})

describe('turns', () => {
  it('draws each starter apart', () => {
    const { container } = draw(
      chat({
        turns: [
          turn(),
          turn({ starter: { type: 'Agent' } }),
          turn({ starter: { type: 'Caf' } }),
          turn({ starter: { type: 'Notice', value: 'The build finished' } }),
          turn({ starter: { type: 'OtherChat', value: { from: chatId(3), message: { parts: [{ type: 'Text', value: 'From afar' }] } } } }),
          turn({ starter: { type: 'Ghost' } as never }),
        ],
      }),
    )
    const starters = [...container.querySelectorAll('.turn')].map((turn) => turn.getAttribute('data-starter'))
    expect(starters).toEqual(['User', 'Agent', 'Caf', 'Notice', 'OtherChat', 'Ghost'])
    const all = text(container)
    expect(all).toContain('You')
    expect(all).toContain('The agent, by itself')
    expect(all).toContain('A notice from the app')
    expect(all).toContain('The build finished')
    expect(all).toContain('From chat #000003')
    expect(all).toContain('From afar')
    expect(all).toContain("something this viewer doesn't know (Ghost)")
  })

  const lives: [string, string][] = [
    ['Waiting', 'Waiting'],
    ['Running', 'Running'],
    ['Stopping', 'Stopping'],
    ['Finished', 'Finished'],
    ['Stopped', 'Stopped'],
    ['CutOff', 'Cut off'],
    ['Reborn', 'Unknown (Reborn)'],
  ]
  for (const [type, badge] of lives) {
    it(`badges a turn ${type}`, () => {
      const { container } = draw(chat({ turns: [turn({ life: life(type) })] }))
      expect(container.querySelector('.life')?.textContent).toBe(badge)
    })
  }

  it("shows a failure's reason in CAF's words, and the agent's words quoted as the agent's", () => {
    const failed = life('Failed', { reason: { type: 'UsageLimit' }, agent_said: 'Try again at 5pm', remedy: { type: 'SendAgain' } })
    const { container } = draw(chat({ turns: [turn({ life: failed })] }))
    const failure = container.querySelector('.failure')!
    expect(container.querySelector('.life')?.textContent).toBe('Failed')
    expect(failure.querySelector('.why')?.textContent).toContain('The agent hit a usage limit.')
    expect(failure.querySelector('.why')?.textContent).not.toContain('Try again at 5pm')
    expect(failure.querySelector('figcaption')?.textContent).toBe('The agent said:')
    expect(failure.querySelector('blockquote')?.textContent).toBe('Try again at 5pm')
  })

  const failures: [string, string][] = [
    ['TooLong', "length limit"],
    ['TooManySteps', 'limit of steps'],
    ['Refused', 'The model refused.'],
    ['SignInNeeded', 'sign in'],
    ['AccessDenied', "can't be used"],
    ['BadRequest', "Sending it again won't help."],
    ['UsageLimit', 'usage limit'],
    ['ProviderError', 'overloaded or failed'],
    ['AgentStopped', 'stopped while working'],
    ['DidNotStop', "didn't stop when asked"],
    ['BadAnswer', 'made no sense'],
    ['Other', 'reported an error'],
    ['Meteor', "a reason this viewer doesn't know (Meteor)"],
  ]
  for (const [reason, says] of failures) {
    it(`says why a turn failed: ${reason}`, () => {
      const failed = life('Failed', { reason: { type: reason }, agent_said: null, remedy: null })
      const { container } = draw(chat({ turns: [turn({ life: failed })] }))
      expect(container.querySelector('.failure .why')?.textContent).toContain(says)
      expect(container.querySelector('.failure blockquote')).toBeNull()
    })
  }

  it('sends a failed turn again with its own message', async () => {
    const failed = life('Failed', { reason: { type: 'ProviderError' }, agent_said: null, remedy: { type: 'SendAgain' } })
    const message = { parts: [{ type: 'Text' as const, value: 'Fix the tests' }] }
    const { actions } = draw(chat({ turns: [turn({ starter: { type: 'User', value: message }, life: failed })] }))
    await fireEvent.click(button('Send again'))
    expect(actions.sendAgain).toHaveBeenCalledWith(message)
  })

  it('shows why Send again failed', async () => {
    const failed = life('Failed', { reason: { type: 'ProviderError' }, agent_said: null, remedy: { type: 'SendAgain' } })
    const given = actions()
    given.sendAgain.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'QueueFull' } }))
    const { container } = draw(chat({ turns: [turn({ life: failed })] }), { actions: given })
    await fireEvent.click(button('Send again'))
    await waitFor(() => expect(text(container)).toContain('Too many messages are waiting.'))
  })

  it("offers no Send again for a turn no message started", () => {
    const failed = life('Failed', { reason: { type: 'Other' }, agent_said: null, remedy: { type: 'SendAgain' } })
    draw(chat({ turns: [turn({ starter: { type: 'Agent' }, life: failed })] }))
    expect(screen.queryByRole('button', { name: 'Send again' })).toBeNull()
  })

  it('says the other remedies, and names one it doesn\'t know', () => {
    const remedies = ['NewChat', 'Pray'].map((type) =>
      turn({ life: life('Failed', { reason: { type: 'Other' }, agent_said: null, remedy: { type } }) }),
    )
    const { container } = draw(chat({ turns: remedies }))
    const all = text(container)
    expect(all).toContain('Start a new chat with the same agent and folder.')
    expect(all).toContain("a remedy this viewer doesn't know (Pray)")
    expect(screen.queryByRole('button', { name: 'Send again' })).toBeNull()
  })

  it('dims imported turns, and marks them history', () => {
    const { container } = draw(chat({ turns: [turn({ imported: true, window: { start: null, end: { type: 'Unknown' } } }), turn()] }))
    const [old, now] = container.querySelectorAll('.turn')
    expect(old!.classList.contains('imported')).toBe(true)
    expect(old!.textContent).toContain('History')
    expect(old!.querySelector('time')).toBeNull()
    expect(now!.classList.contains('imported')).toBe(false)
    expect(now!.textContent).not.toContain('History')
  })

  it('shows how long a turn took, once it has ended', () => {
    const { container } = draw(
      chat({ turns: [turn(), turn({ life: life('Running'), window: { start: 1, end: { type: 'Open' } } })] }),
    )
    const [ended, running] = container.querySelectorAll('.turn')
    expect(ended!.querySelector('.took')?.textContent).toBe('4.2 s')
    expect(running!.querySelector('.took')).toBeNull()
  })
})

describe('items', () => {
  it('draws words as Markdown', () => {
    const { container } = draw(chat({ turns: [turn({ items: [words('Some **bold** and `code`\n\n- one\n- two')] })] }))
    expect(container.querySelector('.markdown strong')?.textContent).toBe('bold')
    expect(container.querySelector('.markdown code')?.textContent).toBe('code')
    expect(container.querySelectorAll('.markdown li')).toHaveLength(2)
  })

  it('folds thinking', () => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Thinking', value: 'Hmm, *tests*' })] })] }))
    const details = container.querySelector('details.thinking') as HTMLDetailsElement
    expect(details.open).toBe(false)
    expect(details.querySelector('summary')?.textContent).toBe('Thinking')
    expect(details.querySelector('em')?.textContent).toBe('tests')
  })

  it('draws a tool as a row that unfolds to its input and output', () => {
    const tool = item(
      {
        type: 'Tool',
        value: { title: 'cargo test', kind: { type: 'Execute' }, status: { type: 'Done' }, input: { command: 'cargo test' }, origin: { type: 'NotRecorded' }, action: null, input_cut: null, locations: [], locations_cut: 0, decision: { type: 'NotAsked' }, result: [{ type: 'Text', value: { text: 'ok: 3 passed', cut: null } }], app: null },
      },
      1500,
    )
    const { container } = draw(chat({ turns: [turn({ items: [tool] })] }))
    const row = container.querySelector('details.tool') as HTMLDetailsElement
    expect(row.open).toBe(false)
    const summary = row.querySelector('summary')!
    expect(summary.querySelector('.kind')?.textContent).toBe('Run')
    expect(summary.querySelector('.icon')?.getAttribute('aria-hidden')).toBe('true')
    expect(summary.querySelector('.title')?.textContent).toBe('cargo test')
    expect(summary.querySelector('.status')?.textContent).toBe('Done')
    expect(summary.querySelector('.time')?.textContent).toBe('1.5 s')
    const [input, output] = row.querySelectorAll('pre')
    expect(JSON.parse(input!.textContent!)).toEqual({ command: 'cargo test' })
    expect(output!.textContent).toBe('ok: 3 passed')
  })

  it("names a tool kind and status it doesn't know", () => {
    const tool = item({
      type: 'Tool',
      value: { title: 't', kind: { type: 'Teleport' } as never, status: { type: 'Pondering' } as never, input: null, origin: { type: 'NotRecorded' }, action: null, input_cut: null, locations: [], locations_cut: 0, decision: { type: 'NotAsked' }, result: [], app: null },
    })
    const { container } = draw(chat({ turns: [turn({ items: [tool] })] }))
    expect(container.querySelector('.tool .kind')?.textContent).toBe('Unknown kind (Teleport)')
    expect(container.querySelector('.tool .status')?.textContent).toBe('Unknown (Pondering)')
  })

  const notes: [string, string][] = [
    ['SessionGone', "no longer had this chat's session"],
    ['CannotContinue', "can't continue sessions"],
    ['SessionStuck', 'session was stuck'],
    ['Whisper', "A note this viewer doesn't know (Whisper)"],
  ]
  for (const [note, says] of notes) {
    it(`draws a note in CAF's voice: ${note}`, () => {
      const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Note', value: { type: note } as never })] })] }))
      const drawn = container.querySelector('.note')!
      expect(drawn.querySelector('.who')?.textContent).toBe('CAF')
      expect(drawn.textContent).toContain(says)
    })
  }

  it('folds an unknown item, with what it was and its raw data', () => {
    const unknown = item({ type: 'Unknown', value: { what: 'plan_v2', raw: { steps: ['a'] }, summary: null } })
    const { container } = draw(chat({ turns: [turn({ items: [unknown] })] }))
    const details = container.querySelector('details.unknown') as HTMLDetailsElement
    expect(details.open).toBe(false)
    expect(details.querySelector('summary')?.textContent).toContain('plan_v2')
    expect(JSON.parse(details.querySelector('pre')!.textContent!)).toEqual({ steps: ['a'] })
  })

  it("names an item kind it doesn't know", () => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Hologram', value: 1 } as never)] })] }))
    expect(text(container)).toContain("An item of a kind this viewer doesn't know: Hologram")
  })

  it("draws an app item this version of the app can't read by the summary it had, and keeps its data", () => {
    const unreadable = item({ type: 'Unknown', value: { what: 'App', raw: { route: ['Kyiv'] }, summary: 'Planned a route' } })
    const { container } = draw(chat({ turns: [turn({ items: [unreadable] })] }))
    const details = container.querySelector('details.unreadable') as HTMLDetailsElement
    expect(details.open).toBe(false)
    expect(text(details.querySelector('summary') as HTMLElement).trim()).toBe(
      "Planned a route · An item of the app's that this version can't read",
    )
    expect(JSON.parse(details.querySelector('pre')!.textContent!)).toEqual({ route: ['Kyiv'] })
    expect(container.querySelector('details.unknown')).toBeNull()
  })

  it("draws an app item this version can't read, with no summary, as the app's", () => {
    // Older data may lack the summary altogether.
    const unreadable = item({ type: 'Unknown', value: { what: 'App', raw: {} } as never })
    const { container } = draw(chat({ turns: [turn({ items: [unreadable] })] }))
    expect(text(container.querySelector('details.unreadable summary') as HTMLElement).trim()).toBe(
      "An item of the app's that this version can't read",
    )
  })

  it("draws an app item by the app's summary", () => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'App', value: { moved: 'Kyiv' } as never })] })] }))
    expect(container.querySelector('.app')?.textContent).toBe("An item of the app's own")
  })

  it('draws the compacted marker', () => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Compacted', value: {} })] })] }))
    expect(text(container)).toContain('The agent compacted its memory here.')
  })
})

describe("Markdown from the agent can't script the page", () => {
  it('draws HTML as text', () => {
    const { container } = draw(chat({ turns: [turn({ items: [words('<script>alert(1)</script> and <img src=x onerror="alert(2)">')] })] }))
    const markdown = container.querySelector('.markdown')!
    expect(markdown.querySelector('script')).toBeNull()
    expect(markdown.querySelector('img')).toBeNull()
    expect(markdown.querySelector('[onerror]')).toBeNull()
    expect(markdown.textContent).toContain('<script>alert(1)</script>')
    expect(markdown.textContent).toContain('<img src=x onerror="alert(2)">')
  })

  it('links only http(s), in a new tab that cannot reach back', () => {
    const { container } = draw(
      chat({
        turns: [
          turn({
            items: [
              words(
                '[web](https://example.com) [js](javascript:alert(1)) [data](data:text/html,hi) [file](file:///etc/passwd) [mail](mailto:a@b.c) [here](/viewer) https://bare.example.org',
              ),
            ],
          }),
        ],
      }),
    )
    const links = [...container.querySelectorAll('.markdown a')]
    expect(links.map((link) => link.getAttribute('href'))).toEqual(['https://example.com', 'https://bare.example.org'])
    for (const link of links) {
      expect(link.getAttribute('target')).toBe('_blank')
      expect(link.getAttribute('rel')).toBe('noopener noreferrer')
    }
  })

  it("doesn't load images, but links to them", () => {
    const { container } = draw(chat({ turns: [turn({ items: [words('![a cat](https://example.com/cat.png) ![x](javascript:alert(1))')] })] }))
    const markdown = container.querySelector('.markdown')!
    expect(markdown.querySelector('img')).toBeNull()
    const links = [...markdown.querySelectorAll('a')]
    expect(links.map((link) => link.getAttribute('href'))).toEqual(['https://example.com/cat.png'])
    expect(links[0]!.textContent).toContain('a cat')
  })

  it("draws a failure's agent text and a user's message as text", () => {
    const failed = life('Failed', { reason: { type: 'Other' }, agent_said: '<b>bold</b>', remedy: null })
    const { container } = draw(chat({ turns: [turn({ starter: { type: 'User', value: { parts: [{ type: 'Text', value: '<i>hi</i>' }] } }, life: failed })] }))
    expect(container.querySelector('b')).toBeNull()
    expect(container.querySelector('i')).toBeNull()
    expect(text(container)).toContain('<b>bold</b>')
    expect(text(container)).toContain('<i>hi</i>')
  })
})

describe('questions', () => {
  const permission = () =>
    question({
      type: 'Permission',
      value: {
        tool: null,
        title: 'Run rm -rf build',
        options: [
          { id: 'other', label: 'Something else', kind: { type: 'Other' } },
          { id: 'yes', label: 'Allow once', kind: { type: 'AllowOnce' } },
          { id: 'always', label: 'Always allow', kind: { type: 'AllowAlways' } },
          { id: 'no', label: 'Reject', kind: { type: 'RejectOnce' } },
        ],
      },
    })

  it("offers the agent's options, in its order, with none chosen", () => {
    const asked = permission()
    const { container } = draw(chat({ questions: [asked] }))
    const pinned = container.querySelector('.questions')!
    expect(pinned.textContent).toContain('Run rm -rf build')
    const options = [...pinned.querySelectorAll('button')]
    expect(options.map((option) => option.textContent)).toEqual(['Something else', 'Allow once', 'Always allow', 'Reject'])
    expect(options.some((option) => option.hasAttribute('autofocus') || option === document.activeElement)).toBe(false)
    // Pinned above the composer.
    expect(pinned.compareDocumentPosition(container.querySelector('.composer')!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy()
  })

  it('answers with the option chosen', async () => {
    const asked = permission()
    const { actions } = draw(chat({ questions: [asked] }))
    await fireEvent.click(button('Always allow'))
    expect(actions.answer).toHaveBeenCalledWith(asked.id, { type: 'Permission', value: 'always' })
  })

  it("doesn't move focus onto the next question's option once one is answered", async () => {
    const ask = (title: string, first: string) =>
      question({
        type: 'Permission',
        value: { tool: null, title, options: [{ id: 'a', label: first, kind: { type: 'AllowAlways' } }, { id: 'r', label: 'Reject', kind: { type: 'RejectOnce' } }] },
      })
    const q1 = ask('Read notes.txt', 'Allow (Q1)')
    const q2 = ask('Run rm -rf ~', 'Always allow (Q2)')
    const view = draw(chat({ questions: [q1, q2] }))
    const first = button('Allow (Q1)')
    first.focus()
    await fireEvent.click(first)
    // CAF takes Q1 away.
    await view.rerender({ chat: chat({ questions: [q2] }) })
    const focused = document.activeElement
    const pinned = view.container.querySelector('.questions')!
    expect(focused instanceof HTMLButtonElement && pinned.contains(focused)).toBe(false)
    expect(view.actions.answer).toHaveBeenCalledTimes(1)
  })

  it('still draws two questions with one id', () => {
    const asked = question({ type: 'SlowToOpen', value: { adapter_answered: true } })
    const { container } = draw(chat({ questions: [asked, { ...asked }] }))
    expect(container.querySelectorAll('.question')).toHaveLength(2)
  })

  it("answers nothing on Enter, even with an option focused by the page's last click", async () => {
    const { actions, container } = draw(chat({ questions: [permission()] }))
    const box = screen.getByRole('textbox', { name: 'Message' })
    box.focus()
    await fireEvent.keyDown(box, { key: 'Enter' })
    expect(actions.answer).not.toHaveBeenCalled()
    expect(container.querySelector('.questions button:focus')).toBeNull()
  })

  it("answers nothing on Enter: no option is the default", async () => {
    const { actions } = draw(chat({ questions: [permission()] }))
    await fireEvent.keyDown(document.body, { key: 'Enter' })
    expect(actions.answer).not.toHaveBeenCalled()
  })

  it('offers keep waiting, retry or stop for a session slow to open', async () => {
    const slow = question({ type: 'SlowToOpen', value: { adapter_answered: true } })
    const { container, actions } = draw(chat({ state: { type: 'Opening' }, questions: [slow] }))
    expect(text(container)).toContain('The session is taking long to open.')
    await fireEvent.click(button('Retry'))
    expect(actions.answer).toHaveBeenLastCalledWith(slow.id, { type: 'SlowToOpen', value: { type: 'Retry' } })
    await waitFor(() => expect(button('Stop').disabled).toBe(false))
    await fireEvent.click(button('Stop'))
    expect(actions.answer).toHaveBeenLastCalledWith(slow.id, { type: 'SlowToOpen', value: { type: 'Stop' } })
    await waitFor(() => expect(button('Keep waiting').disabled).toBe(false))
    await fireEvent.click(button('Keep waiting'))
    expect(actions.answer).toHaveBeenLastCalledWith(slow.id, { type: 'SlowToOpen', value: { type: 'KeepWaiting' } })
  })

  it("says when the adapter hasn't answered at all", () => {
    const { container } = draw(chat({ questions: [question({ type: 'SlowToOpen', value: { adapter_answered: false } })] }))
    expect(text(container)).toContain("hasn't answered at all yet")
  })

  it('keeps waiting on Escape', async () => {
    const slow = question({ type: 'SlowToOpen', value: { adapter_answered: true } })
    const { actions } = draw(chat({ questions: [slow] }))
    await fireEvent.keyDown(document.body, { key: 'Escape' })
    expect(actions.answer).toHaveBeenCalledWith(slow.id, { type: 'SlowToOpen', value: { type: 'KeepWaiting' } })
  })

  it("names a question it doesn't know", () => {
    const { container } = draw(chat({ questions: [question({ type: 'Riddle', value: {} } as never)] }))
    expect(text(container)).toContain("doesn't know how to ask (Riddle)")
  })

  it('shows why an answer failed', async () => {
    const given = actions()
    given.answer.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'NoSuchQuestion' } }))
    const { container } = draw(chat({ questions: [permission()] }), { actions: given })
    await fireEvent.click(button('Reject'))
    await waitFor(() => expect(text(container)).toContain('That question has already gone.'))
  })
})

describe('the composer', () => {
  it('sends a message, and clears', async () => {
    const { actions } = draw(chat())
    const box = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    await fireEvent.input(box, { target: { value: 'Fix the tests' } })
    await fireEvent.keyDown(box, { key: 'Enter' })
    expect(actions.send).toHaveBeenCalledWith({ parts: [{ type: 'Text', value: 'Fix the tests' }] })
    await waitFor(() => expect(box.value).toBe(''))
  })

  it('keeps Send on while a turn runs, so the message waits, and offers Stop', async () => {
    const { actions } = draw(chat({ turns: [turn({ life: life('Running') })] }))
    const box = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    expect(box.placeholder).toContain('waits for the running turn')
    await fireEvent.input(box, { target: { value: 'And then this' } })
    await fireEvent.click(button('Send'))
    expect(actions.send).toHaveBeenCalled()
    await fireEvent.click(button('Stop'))
    expect(actions.stop).toHaveBeenCalled()
  })

  it('shows Stopping while the turn stops', () => {
    draw(chat({ turns: [turn({ life: life('Stopping') })] }))
    expect(button('Stopping…').disabled).toBe(true)
  })

  it('offers no Stop with nothing running', () => {
    draw(chat({ turns: [turn()] }))
    expect(screen.queryByRole('button', { name: 'Stop' })).toBeNull()
  })

  it('gives the message back, and says why, if sending fails', async () => {
    const given = actions()
    given.send.mockRejectedValue(new CafError({ type: 'Disconnected' }))
    const { container } = draw(chat(), { actions: given })
    const box = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    await fireEvent.input(box, { target: { value: 'Hello?' } })
    await fireEvent.click(button('Send'))
    await waitFor(() => expect(text(container)).toContain('The link to CAF dropped'))
    expect(box.value).toBe('Hello?')
  })

  it('closes the chat', async () => {
    const { actions } = draw(chat())
    await fireEvent.click(button('Close'))
    expect(actions.close).toHaveBeenCalled()
  })
})

describe('an archived chat', () => {
  // Closed, as CAF keeps an archived chat, restored or not, until it's
  // unarchived: archiving sticks.
  const archived = () => chat({ state: { type: 'Closed' }, archived: NOON })

  it('says so, and that sending brings it back, offering Unarchive in place of Close', async () => {
    const { container, actions } = draw(archived())
    expect(container.querySelector('header .state')?.textContent?.trim()).toBe('Archived')
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull()
    expect(container.querySelector('.archived[role=status]')?.textContent).toContain(
      'This chat is archived. Sending a message brings it back.',
    )
    await fireEvent.click(button('Unarchive'))
    expect(actions.unarchive).toHaveBeenCalledOnce()
  })

  it('can be written in, as sending to it unarchives it', async () => {
    const { actions } = draw(archived())
    const box = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    expect(box.disabled).toBe(false)
    await fireEvent.input(box, { target: { value: 'Carry on' } })
    await fireEvent.click(button('Send'))
    expect(actions.send).toHaveBeenCalledWith({ parts: [{ type: 'Text', value: 'Carry on' }] })
    expect(actions.unarchive).not.toHaveBeenCalled()
  })

  it('is an ordinary chat once unarchived, here or by a message sent to it', async () => {
    const { container, rerender } = draw(archived())
    // Unarchived, it's idle.
    await rerender({ chat: { ...archived(), state: { type: 'Idle' }, archived: null } })
    expect(container.querySelector('.archived')).toBeNull()
    expect(screen.queryByRole('button', { name: 'Unarchive' })).toBeNull()
    expect(button('Close').disabled).toBe(false)
  })

  it("offers no settings to choose, nor what CAF knows of the agent's", () => {
    const { container } = draw(archived(), {
      agent: { name: 'claude', install: { type: 'Host' }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, limits: { windows: [], extra_usage: null, cleared: null }, settings: { type: 'Unknown' } },
    })
    expect(container.querySelector('.caf-settings')).toBeNull()
  })

  it('unarchives once at a time: Unarchive is off while it goes', async () => {
    const given = actions()
    let finish = () => {}
    given.unarchive.mockImplementation(() => new Promise<void>((done) => (finish = done)))
    draw(archived(), { actions: given })
    await fireEvent.click(button('Unarchive'))
    expect(button('Unarchive').disabled).toBe(true)
    await fireEvent.click(button('Unarchive'))
    expect(given.unarchive).toHaveBeenCalledOnce()
    finish()
    await waitFor(() => expect(button('Unarchive').disabled).toBe(false))
  })

  it("says why, in CAF SI's words, when a message to it or Unarchive couldn't restore it", async () => {
    const cannot = (type: string) => new CafError({ type: 'CannotRestore', value: { type } as never })
    const given = actions()
    given.send.mockRejectedValue(cannot('NotFound'))
    given.unarchive.mockRejectedValue(cannot('InUse'))
    const { container } = draw(archived(), { actions: given })
    const box = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    await fireEvent.input(box, { target: { value: 'Carry on' } })
    await fireEvent.click(button('Send'))
    await waitFor(() => expect(container.querySelector('.composer [role=alert]')?.textContent).toBe("Couldn't send: this chat is gone."))
    expect(box.value).toBe('Carry on')
    await fireEvent.click(button('Unarchive'))
    await waitFor(() =>
      expect(container.querySelector('.caf-chat > [role=alert]')?.textContent).toBe("Couldn't unarchive: this chat is open somewhere else."),
    )
  })

  it('says why unarchiving failed', async () => {
    const given = actions()
    given.unarchive.mockRejectedValue(new CafError({ type: 'NotAllowed' }))
    const { container } = draw(archived(), { actions: given })
    await fireEvent.click(button('Unarchive'))
    await waitFor(() => expect(container.querySelector('[role=alert]')).toBeTruthy())
  })

  it('offers no Unarchive to a viewer that may not act, while read-only, or to a host that gives none', () => {
    draw(archived(), { may: { act: false, agents: true, usage: true } })
    expect(screen.queryByRole('button', { name: 'Unarchive' })).toBeNull()
    cleanup()
    draw(archived(), { readOnly: true })
    expect(screen.queryByRole('button', { name: 'Unarchive' })).toBeNull()
    cleanup()
    const { unarchive: _, ...without } = actions()
    const { container } = draw(archived(), { actions: without })
    expect(screen.queryByRole('button', { name: 'Unarchive' })).toBeNull()
    expect(container.querySelector('.archived')?.textContent).toContain('This chat is archived.')
  })
})

describe('usage', () => {
  const spend = (fields: Partial<Spend> = {}): Spend => ({
    tokens: { input: 1_200_000, cache_read: 800_000, cache_write: 50_000, output: 60_000, thinking: null },
    reported: 1.5,
    estimated: 0,
    unpriced: 0,
    turns: 3,
    ...fields,
  })
  const ring = () => screen.getByRole('button', { name: /This chat's usage/ })
  const popover = () => screen.queryByRole('region', { name: "This chat's usage" })

  it('shows how full the memory is on the ring, amber past 80% and red past 95%', () => {
    const { container } = draw(chat({ usage: { used: 50_000, size: 200_000, cost: null } }))
    expect(ring().getAttribute('aria-label')).toBe("This chat's usage: context 25% full")
    expect(ring().textContent?.trim()).toBe('25%')
    expect(ring().dataset.tone).toBeUndefined()
    expect(container.querySelector('.ring .used')?.getAttribute('stroke-dasharray')).toMatch(/^24\.3/)
  })

  it.each([
    [170_000, 'caution'],
    [195_000, 'warning'],
  ])('a ring %d of 200,000 full is %s', (used, tone) => {
    draw(chat({ usage: { used, size: 200_000, cost: null } }))
    expect(ring().dataset.tone).toBe(tone)
  })

  it("is an empty ring before the agent says", () => {
    const { container } = draw(chat())
    expect(ring().getAttribute('aria-label')).toBe("This chat's usage")
    expect(container.querySelector('.ring .used')).toBeNull()
  })

  it('opens what the chat has used and spent, and closes again', async () => {
    draw(chat({ usage: { used: 195_000, size: 1_000_000, cost: null }, spend: spend() }))
    expect(popover()).toBeNull()
    await fireEvent.click(ring())
    const shown = popover()!
    expect(text(shown)).toContain('This chat')
    expect(text(shown)).toContain('Context 195K / 1M')
    expect(shown.querySelector('.cost')?.textContent?.trim()).toBe('$1.50')
    expect(text(shown)).toContain('As the agent reported it.')
    expect(shown.querySelector('.estimate')).toBeNull()
    expect(shown.querySelector('.processed')?.textContent?.trim()).toBe('2.1M')
    expect(shown.querySelector('.processed')?.getAttribute('title')).toBe('2,110,000 tokens, over 3 turns')
    // No usage page to go to.
    expect(screen.queryByRole('button', { name: /Open usage/ })).toBeNull()
    await fireEvent.keyDown(window, { key: 'Escape' })
    expect(popover()).toBeNull()
    await fireEvent.click(ring())
    await fireEvent.pointerDown(document.body)
    expect(popover()).toBeNull()
  })

  it('marks a cost estimated at API prices, and says what no price covered', async () => {
    draw(chat({ spend: spend({ reported: 1, estimated: 0.84, unpriced: 40_000 }) }))
    await fireEvent.click(ring())
    const shown = popover()!
    expect(shown.querySelector('.cost')?.textContent?.trim()).toBe('$1.84 API estimate')
    expect(text(shown)).toContain('$1.00 as the agent reported it, $0.84 estimated at API prices.')
    expect(text(shown)).toContain("40K tokens on models with no known price aren't in it.")
    expect(text(shown)).toContain('Context Not said yet')
  })

  it('says when no price is known and the agent reported no cost, rather than that it reported nothing', async () => {
    draw(chat({ spend: spend({ reported: 0, estimated: 0, unpriced: 2_110_000 }) }))
    await fireEvent.click(ring())
    const shown = popover()!
    expect(shown.querySelector('.cost')?.textContent?.trim()).toBe('$0.00')
    expect(text(shown)).toContain('No price is known for the models it used, and the agent reported no cost.')
    expect(text(shown)).not.toContain('As the agent reported it.')
  })

  it('takes Escape for itself, and gives focus back to the ring', async () => {
    draw(chat({ spend: spend() }))
    await fireEvent.click(ring())
    const prevented: boolean[] = []
    const later = (event: KeyboardEvent) => prevented.push(event.defaultPrevented)
    window.addEventListener('keydown', later)
    document.body.focus()
    await fireEvent.keyDown(document.body, { key: 'Escape' })
    window.removeEventListener('keydown', later)
    expect(prevented).toStrictEqual([true])
    expect(popover()).toBeNull()
    expect(document.activeElement).toBe(ring())
  })

  it('leaves a question waiting alone when Escape closes it', async () => {
    const { actions } = draw(
      chat({ spend: spend(), questions: [question({ type: 'SlowToOpen', value: { adapter_answered: true } })] }),
    )
    await fireEvent.click(ring())
    await fireEvent.keyDown(document.body, { key: 'Escape' })
    expect(popover()).toBeNull()
    expect(actions.answer).not.toHaveBeenCalled()
    expect(actions.stop).not.toHaveBeenCalled()
  })

  it('says nothing has been spent before a turn has a record', async () => {
    draw(chat())
    await fireEvent.click(ring())
    expect(popover()!.querySelector('.cost')?.textContent?.trim()).toBe('Nothing yet')
    expect(popover()!.querySelector('.processed')?.textContent?.trim()).toBe('0')
  })

  it('opens the usage page, if the app has one', async () => {
    const openUsage = vi.fn()
    render(ChatView, { chat: chat({ spend: spend() }), actions: actions(), openUsage })
    await fireEvent.click(ring())
    await fireEvent.click(screen.getByRole('button', { name: /Open usage/ }))
    expect(openUsage).toHaveBeenCalledOnce()
    expect(popover()).toBeNull()
  })

  it('opens while read-only: it only shows', async () => {
    draw(chat({ spend: spend() }), { readOnly: true })
    await fireEvent.click(ring())
    expect(popover()).not.toBeNull()
  })
})

describe('read-only', () => {
  it('draws everything, and lets nothing be done', async () => {
    const failed = life('Failed', { reason: { type: 'Other' }, agent_said: null, remedy: { type: 'SendAgain' } })
    const { container, actions } = draw(
      chat({
        turns: [turn({ life: failed }), turn({ life: life('Running'), items: [words('Working')] })],
        questions: [
          question({ type: 'Permission', value: { tool: null, title: 'Edit', options: [{ id: 'y', label: 'Allow once', kind: { type: 'AllowOnce' } }] } }),
          question({ type: 'SlowToOpen', value: { adapter_answered: true } }),
        ],
      }),
      { readOnly: true },
    )
    expect(text(container)).toContain('Working')
    expect(text(container)).toContain('As it was then')
    // All but the context ring, which only shows.
    const buttons = [...container.querySelectorAll('button')].filter((each) => !each.classList.contains('ring'))
    expect(buttons.length).toBeGreaterThan(0)
    for (const each of buttons) expect(each.disabled, each.textContent ?? '').toBe(true)
    expect((screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement).disabled).toBe(true)
    for (const each of buttons) await fireEvent.click(each)
    await fireEvent.keyDown(document.body, { key: 'Escape' })
    for (const action of Object.values(actions)) expect(action).not.toHaveBeenCalled()
  })
})

describe("the app's line for a chat", () => {
  it('heads the chat, and names it', () => {
    const { container } = draw(chat(), { label: 'Fix the tests' })
    expect(container.querySelector('header .label')?.textContent).toBe('Fix the tests')
    expect(container.querySelector('header .agent')?.textContent).toBe('claude')
    expect(screen.getByRole('region', { name: 'Fix the tests' })).toBeTruthy()
  })

  it("isn't there for a chat without one", () => {
    const { container } = draw(chat())
    expect(container.querySelector('header .label')).toBeNull()
    expect(screen.getByRole('region', { name: 'Chat #000007' })).toBeTruthy()
  })
})

describe('a different chat in the same view', () => {
  it("doesn't carry a draft, or why something failed, from one chat to the next", async () => {
    const given = actions()
    given.send.mockRejectedValue(new CafError({ type: 'Disconnected' }))
    const view = draw(chat({ id: chatId(1) }), { actions: given })
    const box = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    await fireEvent.input(box, { target: { value: 'meant for chat 1' } })
    await fireEvent.click(button('Send'))
    await waitFor(() => expect(text(view.container)).toContain('The link to CAF dropped'))
    await view.rerender({ chat: chat({ id: chatId(2) }) })
    const now = screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement
    expect(now.value).toBe('')
    expect(text(view.container)).not.toContain('The link to CAF dropped')
  })
})

describe('a different chat in the same view, again', () => {
  it("doesn't carry why closing failed from one chat to the next", async () => {
    const given = actions()
    given.close.mockRejectedValue(new CafError({ type: 'Disconnected' }))
    const view = draw(chat({ id: chatId(1) }), { actions: given })
    await fireEvent.click(button('Close'))
    await waitFor(() => expect(text(view.container)).toContain('The link to CAF dropped'))
    await view.rerender({ chat: chat({ id: chatId(2) }) })
    expect(text(view.container)).not.toContain('The link to CAF dropped')
  })
})

describe('two chat views on one page', () => {
  it('Escape in one keeps only its own question waiting', async () => {
    const slow = () => question({ type: 'SlowToOpen', value: { adapter_answered: true } })
    const one = draw(chat({ id: chatId(1), questions: [slow()] }))
    const two = draw(chat({ id: chatId(2), questions: [slow()] }))
    const box = two.container.querySelector('textarea')!
    box.focus()
    await fireEvent.keyDown(box, { key: 'Escape' })
    expect(two.actions.answer).toHaveBeenCalledTimes(1)
    expect(one.actions.answer).not.toHaveBeenCalled()
  })
})

describe('words for what comes next', () => {
  const next: [unknown, string | null][] = [
    [{ type: 'ShutDown' }, null],
    [{ type: 'SignInNeeded' }, 'Sign in, then send a message to open it again.'],
    [{ type: 'Incompatible', value: { type: 'HttpMcp' } }, null],
    [{ type: 'Incompatible', value: { type: 'Protocol', value: { version: 2 } } }, null],
    [{ type: 'Restarted' }, 'Your next message opens it again.'],
    [{ type: 'Eclipse' }, null],
  ]
  for (const [reason, says] of next) {
    it(`after ${JSON.stringify(reason)}, says ${says === null ? 'nothing about a next message' : says}`, () => {
      const { container } = draw(chat({ state: lost(reason) }))
      if (says === null) expect(text(container)).not.toMatch(/next message|send a message/)
      else expect(text(container)).toContain(says)
    })
  }
})

describe('a tool that ended', () => {
  it('says None for no output once it has ended, and None yet while it runs', () => {
    const tool = (status: string) =>
      item({ type: 'Tool', value: { title: 't', kind: { type: 'Read' }, status: { type: status } as never, input: null, origin: { type: 'NotRecorded' }, action: null, input_cut: null, locations: [], locations_cut: 0, decision: { type: 'NotAsked' }, result: [], app: null } })
    const { container } = draw(chat({ turns: [turn({ items: [tool('Done'), tool('Failed'), tool('Running')] })] }))
    const outputs = [...container.querySelectorAll('.tool .none')].map((none) => none.textContent)
    expect(outputs).toEqual(['None', 'None', 'None', 'None', 'None', 'None yet'])
  })
})

describe("text that hides or reorders itself", () => {
  it('shows bidi controls and invisible characters in raw JSON as escapes', () => {
    const tool = item({
      type: 'Tool',
      value: { title: 't', kind: { type: 'Execute' }, status: { type: 'Done' }, input: { command: 'echo \u202Egnp.exe\u200B' }, origin: { type: 'NotRecorded' }, action: null, input_cut: null, locations: [], locations_cut: 0, decision: { type: 'NotAsked' }, result: [{ type: 'Text', value: { text: 'out\u2066put', cut: null } }], app: null },
    })
    const unknown = item({ type: 'Unknown', value: { what: 'x\u202E', raw: { a: '\u202Eb' }, summary: null } })
    const { container } = draw(chat({ turns: [turn({ items: [tool, unknown] })] }))
    const all = container.textContent ?? ''
    expect(all).not.toMatch(/[\u202A-\u202E\u2066-\u2069\u200B]/)
    expect(all).toContain('<U+202E>')
    expect(all).toContain('<U+200B>')
    expect(all).toContain('<U+2066>')
  })
})
