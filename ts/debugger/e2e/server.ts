// Starts caf-debugger for one test, on a free port, and gives the URL it
// prints, token and all.

import { spawn, type ChildProcess } from 'node:child_process'
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { test as base } from '@playwright/test'

const repo = resolve(import.meta.dirname, '../../..')

/** The binary: `CAF_DEBUGGER`, or the workspace's debug build. */
export const binary = process.env.CAF_DEBUGGER ?? resolve(repo, 'target/debug/caf-debugger')

/** The fake agent, named outright so the server never has to guess: `CAF_FAKE_AGENT_BIN`, or the workspace's debug build. */
export const fake = process.env.CAF_FAKE_AGENT_BIN ?? resolve(repo, 'target/debug/caf-fake-agent')

/** The checked-in recordings. */
export const traces = resolve(process.env.CAF_TRACES ?? resolve(repo, 'crates/core/src/core/tests/traces'))

export interface Server {
  /** The URL to open, with the token in its fragment. */
  url: string
  /** Everything it has printed. */
  output(): string
}

/** What a test starts caf-debugger with, beyond its arguments. */
export interface Setup {
  /**
   * An `agents.toml`, passed with `--agents`. `{fake}` in it becomes the
   * fake agent's program, and `{script}` the script the debugger writes
   * for it.
   */
  agents?: string
  /** What the fake agent's `auth` commands do (see caf-fake-agent's `Auth`), with `{state}` as the test's folder. */
  auth?: Record<string, unknown>
}

/** Starts caf-debugger with `args`; resolves once it says where it is. */
export function start(args: string[], setup: Setup = {}): Promise<Server & { process: ChildProcess; state: string }> {
  // Live runs are recorded under $XDG_STATE_HOME: a folder of the test's own.
  const state = mkdtempSync(join(tmpdir(), 'caf-e2e-'))
  const fill = (text: string) =>
    text
      .replaceAll('{fake}', fake)
      .replaceAll('{script}', join(state, 'caf', 'debugger', 'fake-agent.jsonl'))
      .replaceAll('{state}', state)
  const extra: string[] = []
  if (setup.agents !== undefined) {
    writeFileSync(join(state, 'agents.toml'), fill(setup.agents))
    extra.push('--agents', join(state, 'agents.toml'))
  }
  const auth = join(state, 'auth.json')
  if (setup.auth !== undefined) writeFileSync(auth, fill(JSON.stringify(setup.auth)))
  mkdirSync(join(state, 'caf', 'debugger'), { recursive: true })
  return new Promise((done, fail) => {
    // Agents CAF installs would go under $XDG_DATA_HOME: the same folder, so
    // a test never sees, nor touches, the agents installed on this machine.
    // No proxy, so a download to nowhere fails as offline, as the fake
    // profile's must, whatever this machine's settings.
    const env = {
      ...withoutProxies(process.env),
      XDG_STATE_HOME: state,
      XDG_DATA_HOME: state,
      CAF_FAKE_AGENT_BIN: fake,
      ...(setup.auth !== undefined ? { CAF_FAKE_AGENT_AUTH: auth } : {}),
    }
    const child = spawn(binary, [...args, ...extra, '--port', '0'], { stdio: ['ignore', 'pipe', 'pipe'], env })
    let printed = ''
    const hear = (chunk: Buffer) => {
      printed += chunk.toString()
      const url = /Open (http:\/\/127\.0\.0\.1:\d+\/#token=[0-9a-f]+)/.exec(printed)?.[1]
      if (url !== undefined) done({ url, output: () => printed, process: child, state })
    }
    child.stdout.on('data', hear)
    child.stderr.on('data', hear)
    child.on('error', fail)
    child.on('exit', (code) => fail(new Error(`caf-debugger exited (${code}) before it was ready:\n${printed}`)))
  })
}

/** `env` without any proxy settings, in either case. */
function withoutProxies(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(env).filter(([name]) => !/^(https?|all|no)_proxy$/i.test(name)))
}

/**
 * A test with `server`: caf-debugger started with the test's `serve`
 * arguments. Its page fails the test if the app's Content-Security-Policy
 * ever blocks anything, which would mean the policy and the app disagree.
 */
export const test = base.extend<{ serve: string[]; setup: Setup; server: Server & { state: string } }>({
  page: async ({ page }, use) => {
    const blocked: string[] = []
    page.on('console', (message) => {
      if (/Content Security Policy/i.test(message.text())) blocked.push(message.text())
    })
    await use(page)
    if (blocked.length > 0) throw new Error(`The page's CSP blocked:\n${blocked.join('\n')}`)
  },
  serve: [[], { option: true }],
  setup: [{}, { option: true }],
  server: async ({ serve, setup }, use) => {
    const server = await start(serve, setup)
    await use(server)
    server.process.kill('SIGTERM')
    rmSync(server.state, { recursive: true, force: true })
  },
})

export { expect, type Page } from '@playwright/test'
