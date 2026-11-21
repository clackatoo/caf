// The saved chats: what each is, whether it's open, why one can't be
// restored, and Restore, where the viewer may act.

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { CafError, type SavedChat, type ToViewer } from '@clackatoo/caf'
import SavedList from '../../src/chat/SavedList.svelte'
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
      label: null,
      first: 'Fix the tests',
      turns: 3,
      closed: false,
      ...fields,
    },
    state,
  }
}

function draw(list: SavedChat[], options: { act?: boolean; readOnly?: boolean; restore?: () => Promise<unknown> } = {}) {
  const actions = { select: vi.fn(), restore: vi.fn(options.restore ?? (async () => {})) }
  const view = render(SavedList, { saved: list, actions, act: options.act ?? true, readOnly: options.readOnly ?? false })
  return { ...view, actions }
}

const rows = (container: HTMLElement) => [...container.querySelectorAll('li')]

describe('the saved chats', () => {
  it("names each by its label, or else its first line, with its agent, folder, when it changed and its turns", () => {
    const { container } = draw([
      saved(1, { label: 'Release notes' }),
      saved(2),
      saved(3, { first: null, turns: 1, closed: true, cwd: '/srv/site' }),
    ])
    const [labelled, first, empty] = rows(container)
    expect(labelled!.querySelector('.name')?.textContent?.trim()).toBe('Release notes')
    expect(first!.querySelector('.name')?.textContent?.trim()).toBe('Fix the tests')
    expect(empty!.querySelector('.name')?.textContent?.trim()).toBe('Nothing said yet')
    expect(text(first!)).toContain('claude · project')
    expect(text(first!)).toContain('3 turns')
    expect(text(first!)).not.toContain('closed')
    expect(text(empty!)).toContain('claude · site')
    expect(text(empty!)).toContain('1 turn · closed')
    const when = first!.querySelector('time')!
    expect(when.getAttribute('datetime')).toBe(new Date(NOON).toISOString())
    expect(when.textContent?.trim()).toBe(new Date(NOON).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }))
  })

  it('says when there are none', () => {
    const { container } = draw([])
    expect(text(container)).toContain('No chats saved.')
  })

  it('restores one from its shelf', async () => {
    const { actions } = draw([saved(1)])
    await fireEvent.click(screen.getByRole('button', { name: 'Restore' }))
    expect(actions.restore).toHaveBeenCalledWith(chatId(1), 'work')
  })

  it('says it is restoring while CAF answers, and offers no second Restore', async () => {
    let finish = () => {}
    const { container, actions } = draw([saved(1), saved(2)], { restore: () => new Promise<void>((done) => (finish = done)) })
    await fireEvent.click(screen.getAllByRole('button', { name: 'Restore' })[0]!)
    expect(text(rows(container)[0]!)).toContain('Restoring…')
    const other = screen.getByRole('button', { name: 'Restore' }) as HTMLButtonElement
    expect(other.disabled).toBe(true)
    await fireEvent.click(other)
    expect(actions.restore).toHaveBeenCalledTimes(1)
    finish()
    await waitFor(() => expect(screen.getAllByRole('button', { name: 'Restore' })).toHaveLength(2))
  })

  const failures: [CafError, string][] = [
    [new CafError({ type: 'CannotRestore', value: { type: 'InUse' } }), "It can't be restored: another program has it open."],
    [new CafError({ type: 'NotAllowed' }), "The host doesn't let this viewer do that."],
    [new CafError({ type: 'Refused', value: { type: 'Loaded' } }), 'That chat is open already, or being restored.'],
  ]
  for (const [error, says] of failures) {
    it(`says why it wasn't restored, under it: ${error.message}`, async () => {
      const { container } = draw([saved(1), saved(2)], { restore: async () => Promise.reject(error) })
      await fireEvent.click(screen.getAllByRole('button', { name: 'Restore' })[1]!)
      await waitFor(() => expect(screen.getByRole('alert').textContent).toBe(says))
      expect(rows(container)[1]!.contains(screen.getByRole('alert'))).toBe(true)
    })
  }

  it('shows an open one, and offers no Restore for it', async () => {
    const { container, actions } = draw([saved(1, {}, { type: 'Loaded' })])
    expect(text(container)).toContain('Open')
    expect(screen.queryByRole('button', { name: 'Restore' })).toBeNull()
    await fireEvent.click(screen.getByRole('button', { name: 'Show' }))
    expect(actions.select).toHaveBeenCalledWith(chatId(1))
  })

  it('says one is being restored, and offers no Restore for it', () => {
    const { container } = draw([saved(1, {}, { type: 'Restoring' })])
    expect(text(container)).toContain('Restoring…')
    expect(screen.queryByRole('button')).toBeNull()
  })

  const reasons: [unknown, string][] = [
    [{ type: 'NotFound' }, "it isn't on its shelf any more."],
    [{ type: 'InUse' }, 'another program has it open.'],
    [{ type: 'Newer', value: { version: 3 } }, "a newer CAF saved it (in version 3 of the format), and this one can't read it."],
    [{ type: 'Damaged', value: 'line 4 is cut short' }, 'its save is damaged: line 4 is cut short'],
    [{ type: 'AppData', value: 'missing field `title`' }, "the app can't read what it saved with it: missing field `title`"],
    [{ type: 'Store', value: 'Permission denied' }, "it couldn't be read: Permission denied"],
    [{ type: 'Encrypted' }, "a reason this viewer doesn't know (Encrypted)."],
  ]
  for (const [reason, says] of reasons) {
    it(`says why the last restore failed: ${JSON.stringify(reason)}`, () => {
      const { container } = draw([saved(1, {}, { type: 'Failed', value: reason as never })])
      expect(text(container)).toContain(`It can't be restored: ${says}`)
      expect(screen.getByRole('button', { name: 'Try again' })).toBeTruthy()
    })
  }

  it("names a state it doesn't know by its tag", () => {
    const { container } = draw([saved(1, {}, { type: 'Moving' } as never)])
    expect(text(container)).toContain('Unknown (Moving)')
    expect(screen.queryByRole('button')).toBeNull()
  })

  it('offers no Restore where the host lets this viewer only watch', () => {
    draw([saved(1), saved(2, {}, { type: 'Failed', value: { type: 'InUse' } })], { act: false })
    expect(screen.queryByRole('button')).toBeNull()
  })

  it('restores nothing while read-only', async () => {
    const { actions } = draw([saved(1)], { readOnly: true })
    const restore = screen.getByRole('button', { name: 'Restore' }) as HTMLButtonElement
    expect(restore.disabled).toBe(true)
    await fireEvent.click(restore)
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
