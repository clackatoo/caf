// The list of chats, and New chat: an agent CAF offers, and a full path.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type AgentStatus } from '@clackatoo/caf'
import ChatList from '../../src/chat/ChatList.svelte'

afterEach(cleanup)

const one = '00000000-0000-0000-0000-000000000001'
const two = '00000000-0000-0000-0000-000000000002'
const chats = [
  { id: one, agent: 'claude', cwd: '/home/me/app', label: null, shelf: null },
  { id: two, agent: 'fake', cwd: '/home/me/lib', label: null, shelf: null },
]

function draw(options: { readOnly?: boolean; open?: () => Promise<unknown> } = {}) {
  const actions = { select: vi.fn(), open: vi.fn(options.open ?? (async () => {})) }
  const view = render(ChatList, { chats, agents: ['claude', 'fake'], selected: two, actions, readOnly: options.readOnly ?? false })
  return { ...view, actions }
}

describe('the chat list', () => {
  it('lists the chats, marks the one shown, and picks another', async () => {
    const { container, actions } = draw()
    const items = [...container.querySelectorAll('li button')]
    expect(items.map((item) => item.querySelector('.name')?.textContent)).toEqual(['app', 'lib'])
    expect(items[1]!.getAttribute('aria-current')).toBe('true')
    await fireEvent.click(items[0]!)
    expect(actions.select).toHaveBeenCalledWith(one)
  })

  it("names a chat by the app's line for it, with its folder, or else by its folder, with its id's end", () => {
    const labelled = [{ ...chats[0]!, label: 'Fix the tests', shelf: 'work' }, chats[1]!]
    const { container } = render(ChatList, { chats: labelled, agents: [], selected: null, actions: { select: vi.fn(), open: vi.fn() } })
    const [first, second] = [...container.querySelectorAll('li button')]
    expect(first!.querySelector('.name')?.textContent).toBe('Fix the tests')
    expect(first!.querySelector('.meta')?.textContent).toBe('claude · app')
    expect(second!.querySelector('.name')?.textContent).toBe('lib')
    expect(second!.querySelector('.meta')?.textContent).toBe('fake · #000002')
  })

  it('opens a new chat with an agent and a full path', async () => {
    const { actions } = draw()
    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    const agent = screen.getByRole('combobox') as HTMLSelectElement
    expect([...agent.options].map((option) => option.value)).toEqual(['claude', 'fake'])
    await fireEvent.change(agent, { target: { value: 'fake' } })
    const folder = screen.getByRole('textbox') as HTMLInputElement
    await fireEvent.input(folder, { target: { value: 'relative/path' } })
    expect((screen.getByRole('button', { name: 'Open' }) as HTMLButtonElement).disabled).toBe(true)
    await fireEvent.input(folder, { target: { value: '/home/me/new' } })
    await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
    expect(actions.open).toHaveBeenCalledWith('fake', '/home/me/new', [])
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Open' })).toBeNull())
  })

  describe('with the agents\' models known', () => {
    const choice = (id: string, name: string, description: string | null = null) => ({ id, name, description, group: null })
    const statuses: AgentStatus[] = [
      {
        name: 'claude',
        install: { type: 'Host' },
        sign_in: { type: 'Unknown' },
        can_sign_in: false,
        own_tools_off: true,
        settings: {
          type: 'Known',
          value: [
            {
              kind: { type: 'Model' },
              name: 'Model',
              form: { type: 'Choices', value: [choice('default', 'Default', 'Opus 4.7, for most work'), choice('sonnet', 'Sonnet')] },
              current: { type: 'Choice', value: 'default' },
            },
          ],
        },
      },
      { name: 'fake', install: { type: 'Host' }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, settings: { type: 'Unknown' } },
    ]

    async function form() {
      const actions = { select: vi.fn(), open: vi.fn(async () => {}) }
      render(ChatList, { chats, agents: ['claude', 'fake'], statuses, selected: null, actions })
      await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
      await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/home/me/new' } })
      return actions
    }

    it('opens a chat with the model chosen', async () => {
      const actions = await form()
      const model = screen.getByRole('combobox', { name: 'Model' }) as HTMLSelectElement
      expect([...model.options].map((option) => option.textContent)).toEqual(['Default', 'Sonnet'])
      expect(model.value).toBe('default')
      expect(screen.getByText('Opus 4.7, for most work')).toBeTruthy()
      await fireEvent.change(model, { target: { value: 'sonnet' } })
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('claude', '/home/me/new', [
        { kind: { type: 'Model' }, value: { type: 'Choice', value: 'sonnet' } },
      ])
    })

    it("chooses nothing if the model is left as the agent's", async () => {
      const actions = await form()
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('claude', '/home/me/new', [])
    })

    it("offers no model for an agent whose models aren't known, and forgets one chosen for another", async () => {
      const actions = await form()
      await fireEvent.change(screen.getByRole('combobox', { name: 'Model' }), { target: { value: 'sonnet' } })
      await fireEvent.change(screen.getByRole('combobox', { name: 'Agent' }), { target: { value: 'fake' } })
      expect(screen.queryByRole('combobox', { name: 'Model' })).toBeNull()
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('fake', '/home/me/new', [])
    })
  })

  it("says why a chat wasn't opened", async () => {
    const { container } = draw({ open: async () => Promise.reject(new CafError({ type: 'NotAllowed' })) })
    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/x' } })
    await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
    await waitFor(() => expect(container.textContent).toContain("The host doesn't let this viewer do that."))
  })

  it('opens nothing while read-only', () => {
    draw({ readOnly: true })
    expect((screen.getByRole('button', { name: 'New chat' }) as HTMLButtonElement).disabled).toBe(true)
  })

  it('opens nothing from a form left open when the list turned read-only', async () => {
    const { actions, rerender } = draw()
    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/x' } })
    await rerender({ readOnly: true })
    await fireEvent.submit(screen.getByRole('button', { name: 'Open' }).closest('form')!)
    expect(actions.open).not.toHaveBeenCalled()
  })
})
