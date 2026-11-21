// The chat's agent: on a chat waiting for it, Install (with its size),
// progress, or why it can't be installed; while it's being signed in to,
// the link (https only, in a new tab, cut off from this page), the code,
// a paste box when allowed, and Stop; the Sign in remedy; and the notes
// CAF writes about installs and sign-ins.

import { cleanup, fireEvent, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it } from 'vitest'
import { CafError, type AgentStatus, type Chat } from '@clackatoo/caf'
import { bytes, cannotText, errorText, installLabel, refusedText, signInLabel, signInUrl, stateLabel } from '../../src/chat/words.ts'
import { showsAgent } from '../../src/chat/index.ts'
import { actions, chat, draw, item, life, question, status, statuses, turn } from './build.ts'

afterEach(cleanup)

const waiting = { type: 'WaitingForAgent' } as const

function text(container: HTMLElement): string {
  return (container.textContent ?? '').replace(/\s+/g, ' ')
}

function button(name: string | RegExp): HTMLButtonElement {
  return screen.getByRole('button', { name }) as HTMLButtonElement
}

const link = (url: string | null, code: string | null = null, paste = false): AgentStatus['sign_in'] => ({
  type: 'SigningIn',
  value: { url, code, paste },
})

describe('a chat waiting for its agent', () => {
  it("says it's waiting, not that its state is unknown", () => {
    const { container } = draw(chat({ state: waiting }))
    expect(container.querySelector('[data-state]')?.textContent).toBe('Waiting for its agent')
    expect(text(container)).toContain('Waiting for claude')
    expect(text(container)).not.toContain("doesn't know")
  })

  it("offers Install with the agent's size, and Not now, which stops the chat's message", async () => {
    const { container, actions: given } = draw(chat({ state: waiting }), {
      agent: status({ type: 'Missing', value: { size: 626_797_331 } }),
    })
    expect(text(container)).toContain(
      "claude isn't installed. Installing it downloads about 627 MB. Messages you send wait until it's installed.",
    )
    await fireEvent.click(button('Install'))
    expect(given.install).toHaveBeenCalledOnce()
    await fireEvent.click(button('Not now'))
    expect(given.stop).toHaveBeenCalledOnce()
  })

  it('shows progress while installing, and Stop installing', async () => {
    const { container, actions: given } = draw(chat({ state: waiting }), {
      agent: status({ type: 'Installing', value: { done: 313_398_665, size: 626_797_331 } }),
    })
    expect(text(container)).toContain('Installing claude. 313 MB of about 627 MB. Messages you send wait until it\'s done.')
    const progress = screen.getByRole('progressbar', { name: 'Installing claude' }) as HTMLProgressElement
    expect(progress.value).toBeCloseTo(0.5)
    expect(screen.queryByRole('button', { name: 'Install' })).toBeNull()
    await fireEvent.click(button('Stop installing'))
    expect(given.stopInstall).toHaveBeenCalledOnce()
  })

  it('never shows progress past the end', () => {
    draw(chat({ state: waiting }), { agent: status({ type: 'Installing', value: { done: 900, size: 0 } }) })
    expect((screen.getByRole('progressbar') as HTMLProgressElement).value).toBe(0)
    cleanup()
    draw(chat({ state: waiting }), { agent: status({ type: 'Installing', value: { done: 2e9, size: 1e9 } }) })
    expect((screen.getByRole('progressbar') as HTMLProgressElement).value).toBe(1)
  })

  const cannot: [AgentStatus['install'], string, boolean][] = [
    [{ type: 'Cannot', value: { type: 'Node', value: { needs: 22, has: 20 } } }, 'it needs Node 22 or newer, and this machine has Node 20.', false],
    [{ type: 'Cannot', value: { type: 'Node', value: { needs: 22, has: null } } }, 'it needs Node 22 or newer, and this machine has no Node.', false],
    [{ type: 'Cannot', value: { type: 'Npm' } }, 'it needs npm, and this machine has none.', false],
    [{ type: 'Cannot', value: { type: 'Platform' } }, "it isn't made for this machine's system or processor.", false],
    [{ type: 'Cannot', value: { type: 'NoRecipe' } }, 'CAF has no recipe for installing it: the app starts it itself.', false],
    [{ type: 'Cannot', value: { type: 'Failed', value: { type: 'Offline' } } }, 'the network was out of reach', true],
    [{ type: 'Cannot', value: { type: 'Failed', value: { type: 'Corrupt' } } }, "what was downloaded wasn't what CAF expected", true],
    [{ type: 'Cannot', value: { type: 'Failed', value: { type: 'NoSpace' } } }, 'the disk is full', true],
    [{ type: 'Cannot', value: { type: 'Failed', value: { type: 'Stopped' } } }, 'the last try was stopped', true],
    [{ type: 'Cannot', value: { type: 'Failed', value: { type: 'Other', value: 'npm ERR! code EACCES' } } }, 'the last try failed: npm ERR! code EACCES', true],
    [{ type: 'Cannot', value: { type: 'Failed', value: { type: 'Gremlins' } } } as never, "a reason this viewer doesn't know (Gremlins)", true],
    [{ type: 'Cannot', value: { type: 'Licence' } } as never, "a reason this viewer doesn't know (Licence)", false],
  ]
  for (const [install, says, retry] of cannot) {
    it(`says why it can't be installed: ${JSON.stringify(install)}`, async () => {
      const { container, actions: given } = draw(chat({ state: waiting }), { agent: status(install) })
      expect(text(container)).toContain("claude can't be installed: ")
      expect(text(container)).toContain(says)
      expect(screen.queryByRole('button', { name: 'Try again' }) !== null).toBe(retry)
      expect(screen.queryByRole('button', { name: 'Install' })).toBeNull()
      if (retry) {
        await fireEvent.click(button('Try again'))
        expect(given.install).toHaveBeenCalledOnce()
      }
      await fireEvent.click(button('Not now'))
      expect(given.stop).toHaveBeenCalledOnce()
    })
  }

  it.each([
    [{ type: 'Unknown' }, 'Checking whether claude is installed'],
    [{ type: 'Installed', value: { version: '0.98.0' } }, 'Starting claude'],
    [{ type: 'Host' }, 'Starting claude'],
    [{ type: 'Unpacking' }, "in a state this viewer doesn't know (Unpacking)"],
  ])('says what it waits on: %j', (install, says) => {
    const { container } = draw(chat({ state: waiting }), { agent: status(install as AgentStatus['install']) })
    expect(text(container)).toContain(says)
    expect(screen.queryAllByRole('button', { name: /Install|Not now/ })).toEqual([])
  })

  it("ignores another agent's status", () => {
    const { container } = draw(chat({ state: waiting }), {
      agent: { ...status({ type: 'Missing', value: { size: 1 } }), name: 'codex' },
    })
    expect(screen.queryByRole('button', { name: 'Install' })).toBeNull()
    expect(text(container)).toContain("Waiting for claude. Messages you send wait until it's ready.")
  })

  it('shows no install banner unless the chat waits', () => {
    const { container } = draw(chat(), { agent: status({ type: 'Missing', value: { size: 1 } }) })
    expect(container.querySelector('[data-agent-state]')).toBeNull()
  })

  it('does nothing while read-only', async () => {
    const { actions: given } = draw(chat({ state: waiting }), {
      agent: status({ type: 'Missing', value: { size: 1 } }),
      readOnly: true,
    })
    expect(button('Install').disabled).toBe(true)
    expect(button('Not now').disabled).toBe(true)
    await fireEvent.click(button('Install'))
    expect(given.install).not.toHaveBeenCalled()
  })

  it('says why Install failed, in CAF\'s words', async () => {
    const given = actions()
    given.install.mockRejectedValue(new CafError({ type: 'NotAllowed' }))
    draw(chat({ state: waiting }), { agent: status({ type: 'Missing', value: { size: 1 } }), actions: given })
    await fireEvent.click(button('Install'))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe("The host doesn't let this viewer do that."))
  })

  it('draws every state in the sample of the wire', () => {
    for (const agent of statuses()) {
      const { container } = draw(chat({ agent: agent.name, state: waiting }), { agent })
      expect(container.querySelector('[data-agent-state]')).not.toBeNull()
      expect(text(container)).not.toContain("doesn't know")
      cleanup()
    }
  })
})

describe("the agent's sign-in", () => {
  it('links to an https page in a new tab, cut off from this one, with the code to type', () => {
    const { container } = draw(chat(), {
      agent: status({ type: 'Installed', value: { version: '1' } }, link('https://claude.com/cai/oauth/authorize?x=1', 'ABCD-1234')),
    })
    const a = screen.getByRole('link', { name: 'the sign-in page on claude.com' }) as HTMLAnchorElement
    expect(a.href).toBe('https://claude.com/cai/oauth/authorize?x=1')
    expect(a.target).toBe('_blank')
    expect(a.rel.split(' ').sort()).toEqual(['noopener', 'noreferrer'])
    expect(text(container)).toContain('When it asks for a code, type ABCD-1234')
    expect(screen.queryByRole('textbox', { name: /paste it here/ })).toBeNull()
  })

  it.each([
    'http://claude.com/cai/oauth/authorize',
    'javascript:alert(1)',
    'data:text/html,<b>hi</b>',
    'not a url',
    '//claude.com/x',
  ])("never links an address that isn't https: %s", (url) => {
    const { container } = draw(chat(), { agent: status({ type: 'Host' }, link(url)) })
    expect(container.querySelector('a')).toBeNull()
    expect(text(container)).toContain("isn't one this viewer opens")
    expect(container.querySelector('code')?.textContent).toBe(url)
  })

  it('waits for the link', () => {
    const { container } = draw(chat(), { agent: status({ type: 'Host' }, link(null)) })
    expect(text(container)).toContain("Waiting for the sign-in page's address")
    expect(container.querySelector('a')).toBeNull()
  })

  it('takes a pasted code, trimmed, and clears the box once CAF has it', async () => {
    const { actions: given } = draw(chat(), { agent: status({ type: 'Host' }, link('https://claude.ai/x', null, true)) })
    const box = screen.getByRole('textbox', { name: /paste it here/ }) as HTMLInputElement
    expect(button('Send the code').disabled).toBe(true)
    await fireEvent.input(box, { target: { value: '  code#state \n' } })
    await fireEvent.click(button('Send the code'))
    expect(given.paste).toHaveBeenCalledWith('code#state')
    await waitFor(() => expect(box.value).toBe(''))
  })

  it('keeps a pasted code CAF refused, and says why', async () => {
    const given = actions()
    given.paste.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'NotSigningIn' } }))
    draw(chat(), { agent: status({ type: 'Host' }, link('https://claude.ai/x', null, true)), actions: given })
    const box = screen.getByRole('textbox', { name: /paste it here/ }) as HTMLInputElement
    await fireEvent.input(box, { target: { value: 'abc' } })
    await fireEvent.click(button('Send the code'))
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe("The agent isn't being signed in to."))
    expect(box.value).toBe('abc')
  })

  it('stops', async () => {
    const { actions: given } = draw(chat(), { agent: status({ type: 'Host' }, link('https://claude.ai/x')) })
    await fireEvent.click(button('Stop signing in'))
    expect(given.stopSignIn).toHaveBeenCalledOnce()
  })

  it('shows on a chat waiting for its agent, in place of the install banner', () => {
    const { container } = draw(chat({ state: waiting }), { agent: status({ type: 'Installed', value: { version: '1' } }, link(null)) })
    expect(container.querySelector('[data-agent-state]')?.getAttribute('data-agent-state')).toBe('SigningIn')
  })

  it('is gone from a closed chat', () => {
    const { container } = draw(chat({ state: { type: 'Closed' } }), { agent: status({ type: 'Host' }, link('https://claude.ai/x')) })
    expect(container.querySelector('[data-agent-state]')).toBeNull()
  })

  it('does nothing while read-only', async () => {
    const { actions: given } = draw(chat(), {
      agent: status({ type: 'Host' }, link('https://claude.ai/x', null, true)),
      readOnly: true,
    })
    expect(button('Stop signing in').disabled).toBe(true)
    expect((screen.getByRole('textbox', { name: /paste it here/ }) as HTMLInputElement).disabled).toBe(true)
    await fireEvent.click(button('Stop signing in'))
    expect(given.stopSignIn).not.toHaveBeenCalled()
  })
})

describe('the Sign in remedy', () => {
  const failed = life('Failed', { reason: { type: 'SignInNeeded' }, agent_said: null, remedy: { type: 'SignIn' } })
  /** The chat's agent, which CAF can sign in to now. */
  const can = status({ type: 'Installed', value: { version: '1' } }, { type: 'SignedOut' }, true)

  it('signs in, and keeps Send again', async () => {
    const { actions: given } = draw(chat({ turns: [turn({ life: failed })] }), { agent: can })
    await fireEvent.click(button('Sign in'))
    expect(given.signIn).toHaveBeenCalledOnce()
    expect(given.sendAgain).not.toHaveBeenCalled()
    await fireEvent.click(button('Send again'))
    expect(given.sendAgain).toHaveBeenCalledWith({ parts: [{ type: 'Text', value: 'Hello' }] })
  })

  it('says why signing in was refused', async () => {
    const given = actions()
    given.signIn.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'CannotSignIn' } }))
    draw(chat({ turns: [turn({ life: failed })] }), { actions: given, agent: can })
    await fireEvent.click(button('Sign in'))
    await waitFor(() => expect(screen.getByText("CAF can't sign in to or out of this agent now.")).toBeTruthy())
  })

  it('does nothing while read-only', async () => {
    const { actions: given } = draw(chat({ turns: [turn({ life: failed })] }), { readOnly: true, agent: can })
    expect(button('Sign in').disabled).toBe(true)
    await fireEvent.click(button('Sign in'))
    expect(given.signIn).not.toHaveBeenCalled()
  })

  it.each([
    ['CAF can\'t sign in to it now', status({ type: 'Host' }, { type: 'SignedOut' }, false)],
    ['there is no status', undefined],
    ['the status is another agent\'s', { ...can, name: 'codex' }],
  ])('offers no Sign in where %s, but says to, and keeps Send again', async (_, agent) => {
    const { container, actions: given } = draw(chat({ turns: [turn({ life: failed })] }), { ...(agent ? { agent } : {}) })
    expect(screen.queryByRole('button', { name: 'Sign in' })).toBeNull()
    expect(text(container)).toContain('Sign in to the agent, then send again.')
    await fireEvent.click(button('Send again'))
    expect(given.sendAgain).toHaveBeenCalledOnce()
  })
})

describe('what the host lets this viewer do', () => {
  const failed = life('Failed', { reason: { type: 'SignInNeeded' }, agent_said: null, remedy: { type: 'SignIn' } })
  const missing = status({ type: 'Missing', value: { size: 626_797_331 } }, { type: 'SignedOut' }, true)

  it('without agents: says what is happening, but offers no Install, Try again, Stop or Sign in', () => {
    const may = { act: true, agents: false }
    const { container } = draw(chat({ state: waiting, turns: [turn({ life: failed })] }), { agent: missing, may })
    expect(text(container)).toContain("claude isn't installed. The app can install it, which downloads about 627 MB.")
    expect(screen.queryByRole('button', { name: 'Install' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Sign in' })).toBeNull()
    expect(screen.getByRole('button', { name: 'Not now' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Send again' })).toBeTruthy()
    cleanup()
    draw(chat({ state: waiting }), {
      agent: status({ type: 'Installing', value: { done: 1, size: 2 } }),
      may,
    })
    expect(screen.queryByRole('button', { name: 'Stop installing' })).toBeNull()
    cleanup()
    draw(chat({ state: waiting }), {
      agent: status({ type: 'Cannot', value: { type: 'Failed', value: { type: 'Offline' } } }),
      may,
    })
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull()
  })

  it('without agents: a sign-in under way is said, with no link, code or paste box', () => {
    const { container } = draw(chat(), {
      agent: status({ type: 'Host' }, link(null, null, true)),
      may: { act: true, agents: false },
    })
    expect(text(container)).toContain('Signing in to claude, from the app.')
    expect(screen.queryByRole('textbox', { name: /paste it here/ })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Stop signing in' })).toBeNull()
  })

  it('without act: a waiting question can\'t be answered, nor a failed turn sent again', async () => {
    const again = life('Failed', { reason: { type: 'ProviderError' }, agent_said: null, remedy: { type: 'SendAgain' } })
    const asked = question({
      type: 'Permission',
      value: { tool: 1, title: 'Run ls', options: [{ id: 'allow', label: 'Allow', kind: { type: 'AllowOnce' } }] },
    } as never)
    const { actions: given } = draw(chat({ turns: [turn({ life: again })], questions: [asked] }), {
      may: { act: false, agents: true },
    })
    expect(screen.queryByRole('button', { name: 'Send again' })).toBeNull()
    const allow = button('Allow')
    expect(allow.disabled).toBe(true)
    await fireEvent.click(allow)
    expect(given.answer).not.toHaveBeenCalled()
  })

  it('without act: the chat can be watched, not acted in', () => {
    const { container } = draw(chat({ state: waiting, turns: [turn({ life: failed })] }), {
      agent: missing,
      may: { act: false, agents: true },
    })
    expect(text(container)).toContain('The app lets this viewer watch this chat, not act in it.')
    for (const name of ['Not now', 'Close', 'Send again']) expect(screen.queryByRole('button', { name })).toBeNull()
    // Installing and signing in are the agent's, which it may still do.
    expect(screen.getByRole('button', { name: 'Install' })).toBeTruthy()
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeTruthy()
    expect((screen.getByRole('textbox', { name: 'Message' }) as HTMLTextAreaElement).disabled).toBe(true)
  })

  it('says only one short line as a live region, not progress or buttons', () => {
    const { container } = draw(chat({ state: waiting }), {
      agent: status({ type: 'Installing', value: { done: 1, size: 2 } }),
    })
    const live = container.querySelectorAll('[data-agent-state] [role=status], [data-agent-state][role=status]')
    expect(live).toHaveLength(1)
    expect(live[0]!.textContent).toBe('Installing claude.')
    expect(live[0]!.querySelector('button, progress')).toBeNull()
  })
})

describe("CAF's notes and refusals", () => {
  it.each([
    ['NotInstalled', "The agent isn't installed, so the message that waited for it was stopped."],
    ['InstallFailed', "The agent's install failed, so the message that waited for it was stopped."],
    ['SignedIn', 'You signed in to the agent: send the message again to try it now.'],
  ])('draws %s', (note, says) => {
    const { container } = draw(chat({ turns: [turn({ items: [item({ type: 'Note', value: { type: note } as never })] })] }))
    expect(container.querySelector('.note')?.textContent).toContain(says)
  })

  it.each([
    ['NotInstallable', "CAF can't install this agent"],
    ['AlreadyInstalled', 'The agent is already installed.'],
    ['CannotSignIn', "CAF can't sign in to or out of this agent now."],
    ['NotSigningIn', "The agent isn't being signed in to."],
    ['OwnTools', "CAF can't turn off this agent's own tools, so it won't run it."],
  ])('says %s', (reason, says) => {
    expect(refusedText({ type: reason } as never)).toContain(says)
    expect(errorText(new CafError({ type: 'Refused', value: { type: reason } as never }))).toContain(says)
  })
})

describe('the words for agents', () => {
  it('sizes downloads in MB or GB', () => {
    expect(bytes(626_797_331)).toBe('627 MB')
    expect(bytes(1_500_000_000)).toBe('1.5 GB')
    expect(bytes(999_999)).toBe('less than 1 MB')
    expect(bytes(Number.NaN)).toBe('an unknown size')
  })

  it('opens only https addresses', () => {
    expect(signInUrl('https://claude.ai/x')?.host).toBe('claude.ai')
    expect(signInUrl('HTTPS://claude.ai/x')?.host).toBe('claude.ai')
    expect(signInUrl('http://claude.ai/x')).toBeNull()
    expect(signInUrl(' javascript:alert(1)')).toBeNull()
    expect(signInUrl(null)).toBeNull()
  })

  it('labels every state in the sample, and names one it doesn\'t know', () => {
    for (const agent of statuses()) {
      expect(installLabel(agent.install)).not.toContain('Unknown (')
      expect(signInLabel(agent.sign_in)).not.toContain('Unknown (')
      if (agent.install.type === 'Cannot') expect(cannotText(agent.install.value)).not.toContain("doesn't know")
    }
    expect(installLabel({ type: 'Updating' } as never)).toBe('Unknown (Updating)')
    expect(signInLabel({ type: 'Expired' } as never)).toBe('Unknown (Expired)')
    expect(signInLabel({ type: 'SignedIn', value: { type: 'Passkey' } } as never)).toBe('Signed in (Passkey)')
    expect(stateLabel({ type: 'Hibernating' } as never)).toBe('Unknown state (Hibernating)')
  })
})

describe('where the chat shows its agent', () => {
  const states: [Chat['state'], boolean][] = [
    [{ type: 'Opening' }, true],
    [{ type: 'Open', value: { session: 's' } }, true],
    [{ type: 'WaitingForAgent' }, true],
    [{ type: 'Lost', value: { reason: { type: 'Stopped' } } }, true],
    [{ type: 'Closed' }, false],
  ]
  for (const [state, shows] of states) {
    it(`${shows ? 'shows' : "doesn't show"} it on a chat ${state.type}, as showsAgent says`, () => {
      const drawn = chat({ state })
      expect(showsAgent(drawn)).toBe(shows)
      const { container } = draw(drawn, { agent: status({ type: 'Host' }, link('https://claude.ai/x', null, true)) })
      expect(container.querySelector('[data-agent-state]') !== null).toBe(shows)
    })
  }
})
