// The token: read from the fragment, gone from the address at once, and
// sent on each WebSocket and download.

import { describe, expect, it, vi } from 'vitest'
import { httpUrl, socketUrl, takeToken } from '../src/link.ts'

function at(href: string) {
  const url = new URL(href)
  const location = { href, hash: url.hash, pathname: url.pathname, search: url.search, protocol: url.protocol } as Location
  const history = { state: null, replaceState: vi.fn() } as unknown as History
  return { location, history }
}

describe('the token', () => {
  it('is taken from the fragment, and removed from the address', () => {
    const { location, history } = at('http://127.0.0.1:7320/?x=1#token=s3cret')
    expect(takeToken(location, history)).toBe('s3cret')
    expect(history.replaceState).toHaveBeenCalledWith(null, '', '/?x=1')
  })

  it('leaves the rest of the fragment', () => {
    const { location, history } = at('http://127.0.0.1:7320/#token=abc&tab=state')
    takeToken(location, history)
    expect(history.replaceState).toHaveBeenCalledWith(null, '', '/#tab=state')
  })

  it('goes on each WebSocket and download, and not in a fragment', () => {
    const { location, history } = at('http://127.0.0.1:7320/#token=a%2Bb')
    takeToken(location, history)
    expect(socketUrl('/viewer', location)).toBe('ws://127.0.0.1:7320/viewer?token=a%2Bb')
    expect(socketUrl('/inspect', location)).toBe('ws://127.0.0.1:7320/inspect?token=a%2Bb')
    expect(httpUrl('/recording', location)).toBe('http://127.0.0.1:7320/recording?token=a%2Bb')
  })

  it("isn't touched when there is none", () => {
    const { location, history } = at('http://127.0.0.1:7320/')
    expect(takeToken(location, history)).toBe('')
    expect(history.replaceState).not.toHaveBeenCalled()
  })
})
