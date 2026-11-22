// The saved chats: what each is, whether it's open, why one can't be
// restored, and Restore, where the viewer may act.

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type SavedChat, type ToViewer } from '@clackatoo/caf'
import SavedList from '../../src/chat/SavedList.svelte'
import { restoreOnceFree, unarchiveOrRestore } from '../../src/chat/actions.ts'
import { errorText, refusedText } from '../../src/chat/words.ts'
import { chatId, NOON } from './build.ts'

afterEach(cleanup)

/** The view's text, with runs of white space made one. */
function text(container: HTMLElement): string {
  return (container.textContent ?? '').replace(/\s+/g, ' ')
}

function saved(number: number, fields: Partial<SavedChat['header']> = {}, state: SavedChat['state'] = { type: 'Saved' }): SavedChat {
  return {
    shelf: 'work',
    header: {
      chat: chatId(number),
      agent: 'claude',
      profile: 'claude-agent-acp',
      cwd: '/home/me/project',
      made: NOON - 60_000,
      changed: NOON,
      archived: null,
      label: null,
      first: 'Fix the tests',
      turns: 3,
      closed: false,
      ...fields,
    },
    state,
  }
}

function draw(list: SavedChat[], options: { readOnly?: boolean; restore?: () => Promise<unknown> } = {}) {
  const actions = { select: vi.fn(), restore: vi.fn(options.restore ?? (async () => {})) }
  const view = render(SavedList, { saved: list, actions, readOnly: options.readOnly ?? false })
  return { ...view, actions }
}

const rows = (container: HTMLElement) => [...container.querySelectorAll('li')]

function button(name: string): HTMLButtonElement {
  return screen.getByRole('button', { name }) as HTMLButtonElement
}

/** Two saved chats, `one` and `two`, as listed now. */
const pair = (one: SavedChat['state'] = { type: 'Saved' }, two: SavedChat['state'] = { type: 'Saved' }) => [
  saved(1, { label: 'one' }, one),
  saved(2, { label: 'two' }, two),
]

describe('the saved chats', () => {
  it('marks a chat archived as of when it was', () => {
    const { container } = draw([saved(1, { closed: true, archived: NOON - 1000 })])
    expect(text(rows(container)[0]!)).toContain('3 turns · archived')
  })

  it("names each by its label, or else its first line, with its agent, folder, when it changed and its turns", () => {
    const { container } = draw([
      saved(1, { label: 'Release notes' }),
      saved(2),
      saved(3, { first: null, turns: 1, closed: true, cwd: '/srv/site' }),
    ])
    const [labelled, first, empty] = rows(container)
    expect(labelled!.querySelector('.name')?.textContent?.trim()).toBe('Release notes')
    expect(first!.querySelector('.name')?.textContent?.trim()).toBe('Fix the tests')
    expect(empty!.querySelector('.name')?.textContent?.trim()).toBe('Untitled')
    expect(text(first!)).toContain('claude · project')
    expect(text(first!)).toContain('3 turns')
    expect(text(first!)).not.toContain('archived')
    expect(text(empty!)).toContain('claude · site')
    // Saved before CAF kept when it was archived: closed is archived.
    expect(text(empty!)).toContain('1 turn · archived')
    const when = first!.querySelector('time')!
    expect(when.getAttribute('datetime')).toBe(new Date(NOON).toISOString())
    expect(when.textContent?.trim()).toBe(new Date(NOON).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }))
  })

  it('says when there are none', () => {
    const { container } = draw([])
    expect(text(container)).toContain('No chats saved.')
  })

  it('restores one from its shelf, by a button named for it', async () => {
    const { actions } = draw(pair())
    expect(button('Restore one').textContent?.trim()).toBe('Restore')
    await fireEvent.click(button('Restore two'))
    expect(actions.restore).toHaveBeenCalledWith(chatId(2), 'work')
  })

  it('says it is restoring while CAF answers, and offers no second Restore', async () => {
    let finish = () => {}
    const { container, actions } = draw(pair(), { restore: () => new Promise<void>((done) => (finish = done)) })
    await fireEvent.click(button('Restore one'))
    expect(text(rows(container)[0]!)).toContain('Restoring…')
    expect(screen.queryByRole('button', { name: 'Restore one' })).toBeNull()
    expect(button('Restore two').disabled).toBe(true)
    await fireEvent.click(button('Restore two'))
    expect(actions.restore).toHaveBeenCalledTimes(1)
    finish()
  })

  it('stays restoring once CAF has said it is open, until the list says it is loaded', async () => {
    let finish = () => {}
    const { container, rerender } = draw(pair(), { restore: () => new Promise<void>((done) => (finish = done)) })
    await fireEvent.click(button('Restore one'))
    finish()
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(text(rows(container)[0]!)).toContain('Restoring…')
    expect(button('Restore two').disabled).toBe(true)
    // Another chat changing meanwhile changes nothing here.
    await rerender({ saved: pair({ type: 'Saved' }, { type: 'Failed', value: { type: 'InUse' } }) })
    expect(text(rows(container)[0]!)).toContain('Restoring…')
    expect(button('Try again to restore two').disabled).toBe(true)
    await rerender({ saved: pair({ type: 'Loaded' }) })
    expect(text(rows(container)[0]!)).toContain('Loaded')
    expect(button('Restore two').disabled).toBe(false)
  })

  it('stops saying it is restoring once its own state moves on from what it was when CAF answered', async () => {
    let finish = () => {}
    const { container, rerender } = draw(pair({ type: 'Restoring' }), {
      restore: () => new Promise<void>((done) => (finish = done)),
    })
    await rerender({ saved: pair() })
    await fireEvent.click(button('Restore one'))
    await rerender({ saved: pair({ type: 'Restoring' }) })
    finish()
    await new Promise((resolve) => setTimeout(resolve, 0))
    await rerender({ saved: pair({ type: 'Restoring' }) })
    expect(text(rows(container)[0]!)).toContain('Restoring…')
    // Closed at once, say: put away again.
    await rerender({ saved: pair() })
    expect(button('Restore one').disabled).toBe(false)
  })

  it('shows it loaded when the list says so before CAF answers', async () => {
    let finish = () => {}
    const { container, rerender } = draw(pair(), { restore: () => new Promise<void>((done) => (finish = done)) })
    await fireEvent.click(button('Restore one'))
    await rerender({ saved: pair({ type: 'Loaded' }) })
    expect(text(rows(container)[0]!)).toContain('Loaded')
    expect(button('Restore two').disabled).toBe(false)
    finish()
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(text(rows(container)[0]!)).toContain('Loaded')
    expect(button('Restore two').disabled).toBe(false)
  })

  it('stops saying it is restoring when the link drops before the list says so', async () => {
    const { container, rerender } = draw(pair())
    await fireEvent.click(button('Restore one'))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(text(rows(container)[0]!)).toContain('Restoring…')
    await rerender({ readOnly: true })
    expect(button('Restore one').disabled).toBe(true)
  })

  const failures: [CafError, string][] = [
    [new CafError({ type: 'CannotRestore', value: { type: 'InUse' } }), "It can't be restored: another program has it open."],
    [new CafError({ type: 'NotAllowed' }), "The host doesn't let this viewer do that."],
    [new CafError({ type: 'Refused', value: { type: 'Loaded' } }), 'That chat is open already, or being restored.'],
  ]
  for (const [error, says] of failures) {
    it(`says why it wasn't restored, under it, and offers to try again: ${error.message}`, async () => {
      const { container } = draw(pair(), { restore: async () => Promise.reject(error) })
      await fireEvent.click(button('Restore two'))
      await waitFor(() => expect(screen.getByRole('alert').textContent).toBe(says))
      expect(rows(container)[1]!.contains(screen.getByRole('alert'))).toBe(true)
      expect(button('Try again to restore two').textContent?.trim()).toBe('Try again')
    })
  }

  const lasting: [CafError, string][] = [
    [new CafError({ type: 'CannotRestore', value: { type: 'Damaged', value: 'cut short' } }), 'its save is damaged: cut short'],
    [new CafError({ type: 'Gone' }), 'CAF has shut down'],
    [new CafError({ type: 'Refused', value: { type: 'BadShelf' } }), "That shelf's name can't be used."],
  ]
  for (const [error, says] of lasting) {
    it(`offers no Try again where trying again can't help: ${error.message}`, async () => {
      draw(pair(), { restore: async () => Promise.reject(error) })
      await fireEvent.click(button('Restore two'))
      await waitFor(() => expect(screen.getByRole('alert').textContent).toContain(says))
      expect(screen.queryByRole('button', { name: /two/ })).toBeNull()
    })
  }

  it("draws a time that isn't one a date can hold", () => {
    const { container } = draw([saved(1, { changed: 1e20 })])
    expect(container.querySelector('time')?.hasAttribute('datetime')).toBe(false)
  })

  it("forgets why it wasn't restored once the chat's state moves on, as when another viewer restores it", async () => {
    const { container, rerender } = draw(pair(), { restore: async () => Promise.reject(new CafError({ type: 'NotAllowed' })) })
    await fireEvent.click(button('Restore two'))
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    // Another chat changing leaves it.
    await rerender({ saved: pair({ type: 'Loaded' }) })
    expect(screen.getByRole('alert')).toBeTruthy()
    await rerender({ saved: pair({ type: 'Loaded' }, { type: 'Loaded' }) })
    expect(screen.queryByRole('alert')).toBeNull()
    await rerender({ saved: pair({ type: 'Loaded' }) })
    expect(screen.queryByRole('alert')).toBeNull()
    expect(text(rows(container)[1]!)).not.toContain("doesn't let")
    expect(button('Restore two')).toBeTruthy()
  })

  it('shows a loaded one, by a button named for it, and offers no Restore for it', async () => {
    const { container, actions } = draw(pair({ type: 'Loaded' }))
    expect(text(rows(container)[0]!)).toContain('Loaded')
    expect(screen.queryByRole('button', { name: 'Restore one' })).toBeNull()
    await fireEvent.click(button('Show one'))
    expect(actions.select).toHaveBeenCalledWith(chatId(1))
  })

  it('says one is being restored, and offers no Restore for it', () => {
    const { container } = draw([saved(1, {}, { type: 'Restoring' })])
    expect(text(container)).toContain('Restoring…')
    expect(screen.queryByRole('button')).toBeNull()
  })

  const reasons: [unknown, string, boolean][] = [
    [{ type: 'NotFound' }, "it isn't on its shelf any more.", false],
    [{ type: 'InUse' }, 'another program has it open.', true],
    [{ type: 'Newer', value: { version: 3 } }, "a newer CAF saved it (in version 3 of the format), and this one can't read it.", false],
    [{ type: 'Damaged', value: 'line 4 is cut short' }, 'its save is damaged: line 4 is cut short', false],
    [{ type: 'AppData', value: 'missing field `title`' }, "the app can't read what it saved with it: missing field `title`", false],
    [{ type: 'Store', value: 'Permission denied' }, "it couldn't be read: Permission denied", true],
    [{ type: 'Encrypted' }, "a reason this viewer doesn't know (Encrypted).", true],
  ]
  for (const [reason, says, again] of reasons) {
    it(`says why the last restore failed, and ${again ? 'offers' : "doesn't offer"} to try again: ${JSON.stringify(reason)}`, () => {
      const { container } = draw([saved(1, { label: 'one' }, { type: 'Failed', value: reason as never })])
      expect(text(container)).toContain(`It can't be restored: ${says}`)
      expect(screen.queryByRole('button', { name: 'Try again to restore one' }) !== null).toBe(again)
    })
  }

  it("names a state it doesn't know by its tag", () => {
    const { container } = draw([saved(1, {}, { type: 'Moving' } as never)])
    expect(text(container)).toContain('Unknown (Moving)')
    expect(screen.queryByRole('button')).toBeNull()
  })

  it('restores nothing while read-only', async () => {
    const { actions } = draw(pair(), { readOnly: true })
    expect(button('Restore one').disabled).toBe(true)
    await fireEvent.click(button('Restore one'))
    expect(actions.restore).not.toHaveBeenCalled()
  })

  it("draws every state in version 1's sample", () => {
    const file = resolve(import.meta.dirname, '../../../../samples/viewer/v1.json')
    const sample = JSON.parse(readFileSync(file, 'utf8')) as { to_viewer: ToViewer[] }
    const message = sample.to_viewer.find((each) => each.type === 'Saved')
    if (message?.type !== 'Saved') throw new Error('the sample has no Saved')
    const { container } = draw(message.value)
    expect(rows(container)).toHaveLength(message.value.length)
    expect(text(container)).not.toContain("doesn't know")
  })
})

describe('the words for restoring', () => {
  it.each([
    ['NotSaved', "The chat isn't saved."],
    ['Loaded', 'That chat is open already, or being restored.'],
    ['BadShelf', "That shelf's name can't be used."],
  ])('says %s', (reason, says) => {
    expect(refusedText({ type: reason } as never)).toBe(says)
    expect(errorText(new CafError({ type: 'Refused', value: { type: reason } as never }))).toBe(says)
  })

  it('says why a chat could not be restored', () => {
    const error = new CafError({ type: 'CannotRestore', value: { type: 'NotFound' } })
    expect(errorText(error)).toBe("It can't be restored: it isn't on its shelf any more.")
  })
})

describe('unarchiving a chat', () => {
  const gone = () => new CafError({ type: 'Refused', value: { type: 'NoSuchChat' } })

  it('unarchives one CAF has, restoring nothing', async () => {
    const unarchive = vi.fn().mockResolvedValue(undefined)
    const restore = vi.fn()
    await unarchiveOrRestore(unarchive, restore)
    expect(unarchive).toHaveBeenCalledOnce()
    expect(restore).not.toHaveBeenCalled()
  })

  it('restores one put away as it was archived, then unarchives it', async () => {
    const order: string[] = []
    const unarchive = vi.fn(async () => {
      order.push('unarchive')
      if (order.length === 1) throw gone()
    })
    await unarchiveOrRestore(unarchive, async () => void order.push('restore'))
    expect(order).toEqual(['unarchive', 'restore', 'unarchive'])
  })

  it("leaves the refusal to the caller with nothing to restore, or for any other failure, or if restoring fails", async () => {
    await expect(unarchiveOrRestore(vi.fn().mockRejectedValue(gone()))).rejects.toEqual(gone())
    const notAllowed = new CafError({ type: 'NotAllowed' })
    const restore = vi.fn()
    await expect(unarchiveOrRestore(vi.fn().mockRejectedValue(notAllowed), restore)).rejects.toBe(notAllowed)
    expect(restore).not.toHaveBeenCalled()
    const inUse = new CafError({ type: 'CannotRestore', value: { type: 'InUse' } })
    await expect(unarchiveOrRestore(vi.fn().mockRejectedValue(gone()), vi.fn().mockRejectedValue(inUse))).rejects.toBe(inUse)
  })
})

describe('restoring a chat just put away', () => {
  const loaded = () => new CafError({ type: 'Refused', value: { type: 'Loaded' } })

  it('tries again for a moment while CAF holds it, then has it', async () => {
    const restore = vi.fn().mockRejectedValueOnce(loaded()).mockRejectedValueOnce(loaded()).mockResolvedValue(chatId(7))
    await expect(restoreOnceFree(restore, { wait: 1 })).resolves.toBe(chatId(7))
    expect(restore).toHaveBeenCalledTimes(3)
  })

  it('gives up after a while, and never tries again after any other failure', async () => {
    const held = vi.fn().mockRejectedValue(loaded())
    await expect(restoreOnceFree(held, { tries: 3, wait: 1 })).rejects.toEqual(loaded())
    expect(held).toHaveBeenCalledTimes(3)
    const inUse = new CafError({ type: 'CannotRestore', value: { type: 'InUse' } })
    const other = vi.fn().mockRejectedValue(inUse)
    await expect(restoreOnceFree(other, { wait: 1 })).rejects.toBe(inUse)
    expect(other).toHaveBeenCalledTimes(1)
  })
})
