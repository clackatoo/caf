// The app's inspector protocol is the server's: `PROTOCOL` in channel.ts
// is copied from caf-debugger's `inspector.rs`, which ts-rs can't export.

import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { expect, it } from 'vitest'
import { PROTOCOL } from '../../src/inspector/channel.ts'

it("is the server's", () => {
  const rust = readFileSync(resolve(import.meta.dirname, '../../../../crates/debugger/src/inspector.rs'), 'utf8')
  const server = /pub const PROTOCOL: u32 = (\d+);/.exec(rust)?.[1]
  expect(server, 'PROTOCOL in inspector.rs').toBeDefined()
  expect(PROTOCOL).toBe(Number(server))
})
