// The list of chats, and New chat: an agent CAF offers, and a full path.

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type AgentStatus, type CafTool } from '@clackatoo/caf'
import { newChat } from '../../src/chat/actions.ts'
import ChatList from '../../src/chat/ChatList.svelte'

afterEach(cleanup)

const one = '00000000-0000-0000-0000-000000000001'
const two = '00000000-0000-0000-0000-000000000002'
const chats = [
  { id: one, agent: 'claude', cwd: '/home/me/app', label: null, shelf: null, archived: null },
  { id: two, agent: 'fake', cwd: '/home/me/lib', label: null, shelf: null, archived: null },
]

function draw(options: { readOnly?: boolean; open?: () => Promise<unknown>; offersTools?: boolean } = {}) {
  const actions = { select: vi.fn(), open: vi.fn(options.open ?? (async () => {})) }
  const view = render(ChatList, {
    chats,
    agents: ['claude', 'fake'],
    selected: two,
    actions,
    readOnly: options.readOnly ?? false,
    offersTools: options.offersTools ?? false,
  })
  return { ...view, actions }
}

describe('the chat list', () => {
  it("gives a new chat all of CAF's tools where they're offered, unless unticked, and asks again each time", async () => {
    const { actions } = draw({ offersTools: true })
    const all = [{ type: 'Read' }, { type: 'Edit' }, { type: 'Write' }, { type: 'Bash' }, { type: 'TaskStop' }]
    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    const tools = screen.getByRole('checkbox', { name: "CAF's tools" }) as HTMLInputElement
    expect(tools.checked).toBe(true)
    expect(tools.getAttribute('aria-describedby')).not.toBeNull()
    expect(document.getElementById(tools.getAttribute('aria-describedby')!)?.textContent).toMatch(/Read, Edit, Write and Bash/)
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/home/me/new' } })
    await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
    expect(actions.open).toHaveBeenLastCalledWith('claude', '/home/me/new', [], all)
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Open' })).toBeNull())

    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    const again = screen.getByRole('checkbox', { name: "CAF's tools" }) as HTMLInputElement
    await fireEvent.click(again)
    expect(again.checked).toBe(false)
    expect(document.getElementById(again.getAttribute('aria-describedby')!)?.textContent).toMatch(/agent's own tools/)
    await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/home/me/new' } })
    await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
    expect(actions.open).toHaveBeenLastCalledWith('claude', '/home/me/new', [], [])
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Open' })).toBeNull())

    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    expect((screen.getByRole('checkbox', { name: "CAF's tools" }) as HTMLInputElement).checked).toBe(true)
  })

  it('asks CAF for what the form gave, as the debugger opens it: its tools, or none outright when unticked', async () => {
    const asked: unknown[] = []
    const actions = {
      select: vi.fn(),
      open: vi.fn(async (agent: string, cwd: string, _settings: unknown, tools: readonly CafTool[] = []) => {
        asked.push(newChat(agent, cwd, tools))
      }),
    }
    render(ChatList, { chats, agents: ['claude'], selected: null, actions, offersTools: true })
    const all = [{ type: 'Read' }, { type: 'Edit' }, { type: 'Write' }, { type: 'Bash' }, { type: 'TaskStop' }]
    for (const ticked of [true, false]) {
      await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
      if (!ticked) await fireEvent.click(screen.getByRole('checkbox', { name: "CAF's tools" }))
      await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/home/me/new' } })
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      await waitFor(() => expect(screen.queryByRole('button', { name: 'Open' })).toBeNull())
    }
    expect(asked).toEqual([
      { agent: 'claude', cwd: '/home/me/new', session: { type: 'New' }, tools: { caf: all } },
      { agent: 'claude', cwd: '/home/me/new', session: { type: 'New' }, tools: { caf: [] } },
    ])
  })

  it("offers no tools where the host doesn't", async () => {
    draw()
    await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
    expect(screen.queryByRole('checkbox')).toBeNull()
  })

  it('lists the chats, marks the one shown, and picks another', async () => {
    const { container, actions } = draw()
    const items = [...container.querySelectorAll('li button')]
    expect(items.map((item) => item.querySelector('.name')?.textContent)).toEqual(['app', 'lib'])
    expect(items[1]!.getAttribute('aria-current')).toBe('true')
    await fireEvent.click(items[0]!)
    expect(actions.select).toHaveBeenCalledWith(one)
  })

  it('marks an archived chat so, its name muted', () => {
    const archived = [{ ...chats[0]!, archived: 1_800_000_000_000 }, chats[1]!]
    const { container } = render(ChatList, { chats: archived, agents: [], selected: null, actions: { select: vi.fn(), open: vi.fn() } })
    const [first, second] = [...container.querySelectorAll('li button')]
    expect(first!.querySelector('.meta')?.textContent?.replace(/\s+/g, ' ')).toBe('claude · #000001 · archived')
    expect(first!.classList.contains('archived')).toBe(true)
    expect(second!.querySelector('.meta')?.textContent).toBe('fake · #000002')
    expect(second!.classList.contains('archived')).toBe(false)
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
    expect(actions.open).toHaveBeenCalledWith('fake', '/home/me/new', [], [])
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
        own_tools_off: true, limits: { windows: [], extra_usage: null, cleared: null },
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
      { name: 'fake', install: { type: 'Host' }, sign_in: { type: 'Unknown' }, can_sign_in: false, own_tools_off: true, limits: { windows: [], extra_usage: null, cleared: null }, settings: { type: 'Unknown' } },
    ]

    async function form() {
      const actions = { select: vi.fn(), open: vi.fn(async () => {}) }
      const view = render(ChatList, { chats, agents: ['claude', 'fake'], statuses, selected: null, actions })
      await fireEvent.click(screen.getByRole('button', { name: 'New chat' }))
      await fireEvent.input(screen.getByRole('textbox'), { target: { value: '/home/me/new' } })
      return Object.assign(actions, { rerender: view.rerender })
    }

    /** Claude's status, its models as given, the first its current. */
    function offering(...ids: string[]): AgentStatus[] {
      const [claude, fake] = statuses as [AgentStatus, AgentStatus]
      const setting = {
        kind: { type: 'Model' as const },
        name: 'Model',
        form: { type: 'Choices' as const, value: ids.map((id) => choice(id, id)) },
        current: { type: 'Choice' as const, value: ids[0]! },
      }
      return [{ ...claude, settings: { type: 'Known', value: [setting] } }, fake]
    }

    it("keeps the model picked as the agents' statuses change, until it's no longer offered", async () => {
      const actions = await form()
      const model = () => screen.getByRole('combobox', { name: 'Model' }) as HTMLSelectElement
      await fireEvent.change(model(), { target: { value: 'sonnet' } })
      await actions.rerender({ statuses: offering('opus', 'sonnet', 'haiku') })
      expect(model().value).toBe('sonnet')
      await actions.rerender({ statuses: offering('opus', 'haiku') })
      expect(model().value).toBe('opus')
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('claude', '/home/me/new', [], [])
    })

    it("follows what the agent would use while nothing's picked", async () => {
      const actions = await form()
      await actions.rerender({ statuses: offering('haiku', 'sonnet') })
      expect((screen.getByRole('combobox', { name: 'Model' }) as HTMLSelectElement).value).toBe('haiku')
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('claude', '/home/me/new', [], [])
    })

    it('opens a chat with the model chosen', async () => {
      const actions = await form()
      const model = screen.getByRole('combobox', { name: 'Model' }) as HTMLSelectElement
      expect([...model.options].map((option) => option.textContent)).toEqual(['Default', 'Sonnet'])
      expect(model.value).toBe('default')
      const about = screen.getByText('Opus 4.7, for most work')
      expect(model.getAttribute('aria-describedby')).toBe(about.id)
      await fireEvent.change(model, { target: { value: 'sonnet' } })
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('claude', '/home/me/new', [
        { kind: { type: 'Model' }, value: { type: 'Choice', value: 'sonnet' } },
      ], [])
    })

    it("chooses nothing if the model is left as the agent's", async () => {
      const actions = await form()
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('claude', '/home/me/new', [], [])
    })

    it("offers no model for an agent whose models aren't known, and forgets one chosen for another", async () => {
      const actions = await form()
      await fireEvent.change(screen.getByRole('combobox', { name: 'Model' }), { target: { value: 'sonnet' } })
      await fireEvent.change(screen.getByRole('combobox', { name: 'Agent' }), { target: { value: 'fake' } })
      expect(screen.queryByRole('combobox', { name: 'Model' })).toBeNull()
      await fireEvent.click(screen.getByRole('button', { name: 'Open' }))
      expect(actions.open).toHaveBeenCalledWith('fake', '/home/me/new', [], [])
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
