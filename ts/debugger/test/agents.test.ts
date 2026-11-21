// The Agents panel: each agent's install and sign-in state, with Install
// or Stop, Sign in or Sign out, and the link, code and paste box while
// one is being signed in to.

import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type AgentStatus } from '@clackatoo/caf'
import AgentsPanel, { type AgentActions } from '../src/AgentsPanel.svelte'
import { statuses } from './chat/build.ts'

afterEach(cleanup)

function actions(): { [K in keyof AgentActions]: ReturnType<typeof vi.fn> } & AgentActions {
  return {
    install: vi.fn(async () => {}),
    stopInstall: vi.fn(async () => {}),
    signIn: vi.fn(async () => {}),
    paste: vi.fn(async () => {}),
    stopSignIn: vi.fn(async () => {}),
    signOut: vi.fn(async () => {}),
  }
}

function draw(
  agents: AgentStatus[],
  options: { readOnly?: boolean; actions?: AgentActions; may?: boolean; inChat?: string } = {},
) {
  const given = options.actions ?? actions()
  const view = render(AgentsPanel, {
    agents,
    actions: given,
    readOnly: options.readOnly ?? false,
    may: options.may ?? true,
    inChat: options.inChat ?? null,
  })
  return { ...view, actions: given }
}

function row(name: string): HTMLElement {
  return document.querySelector(`[data-agent="${name}"]`) as HTMLElement
}

function buttons(name: string): string[] {
  return within(row(name))
    .queryAllByRole('button')
    .map((button) => button.textContent!.trim())
}

const text = (element: HTMLElement) => (element.textContent ?? '').replace(/\s+/g, ' ')

describe('the Agents panel', () => {
  it('draws every agent in the sample of the wire, each with what can be done', () => {
    draw(statuses())
    const all = statuses()
    expect(screen.getAllByRole('listitem')).toHaveLength(all.length)
    // agent-0: started by the app, sign-in unknown.
    expect(text(row('agent-0'))).toContain('Started by the app')
    expect(buttons('agent-0')).toEqual(['Sign in'])
    // agent-1: not yet surveyed.
    expect(text(row('agent-1'))).toContain('Checking…')
    expect(buttons('agent-1')).toEqual([])
    // agent-2: missing, with a sign-in under way but no link yet.
    expect(text(row('agent-2'))).toContain('About 627 MB to download.')
    expect(text(row('agent-2'))).toContain("Waiting for the sign-in page's address")
    expect(buttons('agent-2')).toEqual(['Install', 'Stop signing in'])
    // agent-3: installing.
    expect(within(row('agent-3')).getByRole('progressbar')).toBeTruthy()
    expect(buttons('agent-3')).toContain('Stop installing')
    // agent-4: installed and signed in.
    expect(text(row('agent-4'))).toContain('Installed, version 0.98.0')
    expect(text(row('agent-4'))).toContain('Signed in with an account')
    expect(buttons('agent-4')).toEqual(['Sign in', 'Sign out'])
    // agent-5: can't, for want of Node.
    expect(text(row('agent-5'))).toContain('it needs Node 22 or newer, and this machine has Node 20.')
    expect(buttons('agent-5')).toEqual([])
    // agent-9: CAF has no recipe for it.
    expect(text(row('agent-9'))).toContain('CAF has no recipe for installing it')
    expect(buttons('agent-9')).toEqual([])
    // agent-13: a failed install may be tried again.
    expect(buttons('agent-13')).toEqual(['Try again'])
    for (const agent of all) expect(text(row(agent.name))).not.toContain('Unknown (')
  })

  it('installs, stops, signs in and out, each agent by name', async () => {
    const { actions: given } = draw([
      { name: 'a', install: { type: 'Missing', value: { size: 1e9 } }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
      { name: 'b', install: { type: 'Installing', value: { done: 1, size: 2 } }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
      { name: 'c', install: { type: 'Installed', value: { version: '1' } }, sign_in: { type: 'SignedOut' }, can_sign_in: true, own_tools_off: true, settings: { type: 'Unknown' } },
      { name: 'd', install: { type: 'Host' }, sign_in: { type: 'SignedIn', value: { type: 'ApiKey' } }, can_sign_in: true, own_tools_off: true, settings: { type: 'Unknown' } },
    ])
    // Each named with its agent, for a screen reader.
    await fireEvent.click(screen.getByRole('button', { name: 'Install a' }))
    await fireEvent.click(screen.getByRole('button', { name: 'Stop installing b' }))
    await fireEvent.click(screen.getByRole('button', { name: 'Sign in to c' }))
    await fireEvent.click(screen.getByRole('button', { name: 'Sign out of d' }))
    expect(given.install).toHaveBeenCalledWith('a')
    expect(given.stopInstall).toHaveBeenCalledWith('b')
    expect(given.signIn).toHaveBeenCalledWith('c')
    expect(given.signOut).toHaveBeenCalledWith('d')
  })

  it('shows the link, the code, and takes a paste, for the agent being signed in to', async () => {
    const { actions: given } = draw([
      {
        name: 'claude',
        install: { type: 'Installed', value: { version: '0.98.0' } },
        sign_in: { type: 'SigningIn', value: { url: 'https://claude.com/cai/oauth/authorize', code: 'ABCD-1234', paste: true } },
        can_sign_in: true,
        own_tools_off: true,
        settings: { type: 'Unknown' },
      },
    ])
    const a = screen.getByRole('link') as HTMLAnchorElement
    expect(a.href).toBe('https://claude.com/cai/oauth/authorize')
    expect(a.rel.split(' ').sort()).toEqual(['noopener', 'noreferrer'])
    expect(a.target).toBe('_blank')
    expect(text(row('claude'))).toContain('ABCD-1234')
    expect(buttons('claude')).not.toContain('Sign in')
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: 'code#state' } })
    await fireEvent.click(screen.getByRole('button', { name: 'Send the code' }))
    expect(given.paste).toHaveBeenCalledWith('claude', 'code#state')
    await fireEvent.click(screen.getByRole('button', { name: 'Stop signing in' }))
    expect(given.stopSignIn).toHaveBeenCalledWith('claude')
  })

  it('says why something was refused, by its agent', async () => {
    const given = actions()
    given.install.mockRejectedValue(new CafError({ type: 'Refused', value: { type: 'NotInstallable' } }))
    draw(
      [
        { name: 'a', install: { type: 'Missing', value: { size: 1 } }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
        { name: 'b', install: { type: 'Missing', value: { size: 1 } }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
      ],
      { actions: given },
    )
    await fireEvent.click(screen.getByRole('button', { name: 'Install a' }))
    await waitFor(() => expect(within(row('a')).getByRole('alert').textContent).toContain("CAF can't install this agent"))
    expect(within(row('b')).queryByRole('alert')).toBeNull()
  })

  it('does nothing while read-only', async () => {
    const { actions: given } = draw(statuses(), { readOnly: true })
    for (const button of screen.getAllByRole('button')) {
      expect((button as HTMLButtonElement).disabled).toBe(true)
      await fireEvent.click(button)
    }
    for (const action of Object.values(given)) expect(action).not.toHaveBeenCalled()
  })

  it("offers Sign in only where CAF can sign in now, as the status says", () => {
    draw([
      { name: 'no', install: { type: 'Host' }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
      { name: 'out', install: { type: 'Installed', value: { version: '1' } }, sign_in: { type: 'SignedIn', value: { type: 'Account' } }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
      { name: 'yes', install: { type: 'Host' }, sign_in: { type: 'SignedOut' }, can_sign_in: true, own_tools_off: true, settings: { type: 'Unknown' } },
    ])
    expect(buttons('no')).toEqual([])
    // Signing out is its own command.
    expect(buttons('out')).toEqual(['Sign out'])
    expect(buttons('yes')).toEqual(['Sign in'])
  })

  it('offers nothing to a viewer the host doesn\'t let act on agents, but shows where each stands', () => {
    draw(statuses(), { may: false })
    expect(screen.queryAllByRole('button')).toEqual([])
    expect(screen.queryByRole('textbox')).toBeNull()
    expect(text(row('agent-4'))).toContain('Installed, version 0.98.0')
    expect(text(row('agent-4'))).toContain('Signed in with an account')
  })

  it('leaves the sign-in of the chat shown beside to the chat, so there is one paste box', () => {
    const signingIn = (name: string): AgentStatus => ({
      name,
      install: { type: 'Host' },
      sign_in: { type: 'SigningIn', value: { url: 'https://caf.example/x', code: null, paste: true } },
      can_sign_in: true,
      own_tools_off: true,
      settings: { type: 'Unknown' },
    })
    draw([signingIn('shown'), signingIn('other')], { inChat: 'shown' })
    expect(text(row('shown'))).toContain('Signing in: see the chat.')
    expect(within(row('shown')).queryByRole('textbox')).toBeNull()
    expect(within(row('other')).getByRole('textbox')).toBeTruthy()
  })

  it('says when none are offered', () => {
    draw([])
    expect(screen.getByText('No agents offered.')).toBeTruthy()
  })
})
