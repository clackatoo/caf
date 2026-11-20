// The dev proxy passes on the dev page's own requests as from
// caf-debugger's origin, and nothing else.

import { describe, expect, it } from 'vitest'
import { origin } from '../dev-proxy.ts'

const server = 'http://127.0.0.1:7320'

describe("the dev proxy's Origin", () => {
  it("is the server's for the dev page's own", () => {
    expect(origin('http://localhost:5173', 'localhost:5173', server)).toBe(server)
    expect(origin('http://127.0.0.1:5199', '127.0.0.1:5199', server)).toBe(server)
  })

  it('is left as it came for any other page, for the server to refuse', () => {
    expect(origin('https://evil.example', 'localhost:5173', server)).toBe('https://evil.example')
    expect(origin('http://localhost:9999', 'localhost:5173', server)).toBe('http://localhost:9999')
    expect(origin(undefined, 'localhost:5173', server)).toBeUndefined()
  })

  it('is left as it came for a name that only points here', () => {
    expect(origin('http://evil.example:5173', 'evil.example:5173', server)).toBe('http://evil.example:5173')
  })
})
