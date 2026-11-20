// Streaming words: rendered at most once every 50 ms, not on every chunk,
// and never while the user has text selected in them.

import { cleanup, render } from '@testing-library/svelte'
import { afterEach, describe, expect, it, vi } from 'vitest'
import Markdown from '../../src/chat/Markdown.svelte'
import * as markdown from '../../src/chat/markdown.ts'

afterEach(() => {
  cleanup()
  vi.useRealTimers()
  vi.restoreAllMocks()
})

describe('streaming Markdown', () => {
  it('draws at once, then at most once every 50 ms however many chunks come', async () => {
    vi.useFakeTimers()
    const renders = vi.spyOn(markdown, 'render')
    const view = render(Markdown, { text: 'Hello' })
    expect(view.container.querySelector('.markdown')?.textContent).toBe('Hello\n')
    let text = 'Hello'
    for (let i = 0; i < 40; i++) {
      text += ' more'
      await view.rerender({ text })
    }
    expect(renders).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(60)
    expect(renders).toHaveBeenCalledTimes(2)
    expect(view.container.querySelector('.markdown')?.textContent).toBe(`${text}\n`)
  })

  it('waits while text in it is selected, then catches up', async () => {
    vi.useFakeTimers()
    const view = render(Markdown, { text: 'First words' })
    const paragraph = view.container.querySelector('p')!
    const range = document.createRange()
    range.selectNodeContents(paragraph)
    document.getSelection()!.removeAllRanges()
    document.getSelection()!.addRange(range)

    await view.rerender({ text: 'First words, and more' })
    await vi.advanceTimersByTimeAsync(200)
    // The selected paragraph is still the one drawn: nothing replaced it.
    expect(view.container.querySelector('p')).toBe(paragraph)
    // But it says more has come.
    expect(view.container.querySelector('.stale')?.textContent).toContain('More has come')

    document.getSelection()!.removeAllRanges()
    document.dispatchEvent(new Event('selectionchange'))
    await vi.advanceTimersByTimeAsync(10)
    expect(view.container.querySelector('.markdown')?.textContent).toBe('First words, and more\n')
    expect(view.container.querySelector('.stale')).toBeNull()
  })
})
