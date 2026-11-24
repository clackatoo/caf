// What the chat view says about a tool call, and what it lets through of
// what an agent gave: an image only as a picture, a link only to the web.

import type { Action, CafTool, Cut, Decision, DeepReadonly, Ending, Location, ToolCall, ToolStatus } from '@clackatoo/caf'
import { count } from './figures.ts'
import { tagOf } from './words.ts'

/** Every one of CAF's own tools, as caf-core's `CafTool::ALL`. */
export const CAF_TOOLS: readonly CafTool[] = [{ type: 'Read' }, { type: 'Edit' }, { type: 'Write' }, { type: 'Bash' }, { type: 'TaskStop' }]

/** Whether a call has ended: Done, Failed, Refused or Cancelled. */
export function ended(status: DeepReadonly<ToolStatus>): boolean {
  return ['Done', 'Failed', 'Refused', 'Cancelled'].includes(status.type)
}

/**
 * What a job is, as TaskStop names it: one running in the background,
 * the agent answered as it went there and told when it ends, or one that
 * ran there and has `ended`.
 */
export function jobText(job: number, ended: boolean): string {
  return ended
    ? `Ran in the background, as task ${job}: the agent was told when it ended.`
    : `Running in the background, as task ${job}: the agent was answered and goes on, and is told when it ends.`
}

// CAF's notice of a job's end, as Claude Code words its own: the whole
// text in one tag.
const NOTIFICATION = /^\s*<task-notification>([\s\S]*)<\/task-notification>\s*$/

/**
 * What CAF told the agent of a background job's end, from a notice turn's
 * text, in words for the user: null for any other notice, such as the
 * app's. The sentence is the notice's own, or its `<summary>` if it's in
 * parts; parts with no summary are told as task, status and output.
 */
export function jobNotice(text: string): string | null {
  const inner = NOTIFICATION.exec(text)?.[1]
  if (inner === undefined) return null
  const part = (name: string) => new RegExp(`<${name}>([\\s\\S]*?)</${name}>`).exec(inner)?.[1]?.trim() ?? null
  const tidy = (words: string) => words.replace(/\s+/g, ' ').trim()
  if (!/<[a-z-]+>/.test(inner)) return tidy(inner) || 'A background task ended.'
  const summary = part('summary')
  if (summary !== null) return tidy(summary)
  const id = part('task-id')
  const status = part('status')
  const output = part('output-file')
  const said = [`${id === null ? 'A background task' : `Task ${id}`}${status === null ? ' ended' : ` ${status}`}.`]
  if (output !== null) said.push(`Output: ${output}`)
  return tidy(said.join(' '))
}

/** A size in bytes, short: `512 B`, `64 KB`, `2.4 MB`, counting 1,024 to a KB as CAF's caps do. */
export function bytes(n: number): string {
  if (n < 1024) return `${count(n)} B`
  const units = ['KB', 'MB', 'GB']
  let size = n / 1024
  let unit = 0
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024
    unit++
  }
  return `${size < 10 && !Number.isInteger(size) ? size.toFixed(1) : Math.round(size)} ${units[unit]}`
}

/** How much of something cut was kept. */
export function cutText(cut: DeepReadonly<Cut>): string {
  return `Kept ${bytes(cut.kept)} of ${bytes(cut.total)}`
}

/** Who allowed or refused a call, or null when there's nothing to say. */
export function decisionText(decision: DeepReadonly<Decision>): string | null {
  if (decision.type !== 'Allowed' && decision.type !== 'Refused') return null
  const verb = decision.type === 'Allowed' ? 'Allowed' : 'Refused'
  const by = decision.value
  switch (by.type) {
    case 'User':
      return `${verb} by you`
    case 'Policy':
      return by.value.reason === null || by.value.reason === '' ? `${verb} by the policy` : `${verb} by the policy: ${by.value.reason}`
    default:
      return `${verb}, by someone this viewer doesn't know (${tagOf(by)})`
  }
}

/** How a command ended. */
export function endingText(ending: DeepReadonly<Ending>): string {
  switch (ending.type) {
    case 'Exited': {
      const exit = ending.value
      switch (exit.type) {
        case 'Code':
          return `Exited with code ${exit.value}`
        case 'Signal':
          return `Killed by signal ${exit.value}`
        default:
          return `Ended in a way this viewer doesn't know (${tagOf(exit)})`
      }
    }
    case 'Unknown':
      return 'Ended, without saying how'
    default:
      return `Ended in a way this viewer doesn't know (${tagOf(ending)})`
  }
}

/** Whether an ending is a failure: an exit code but 0, or a signal. */
export function endedBadly(ending: DeepReadonly<Ending>): boolean {
  return ending.type === 'Exited' && !(ending.value.type === 'Code' && ending.value.value === 0)
}

/** A place in a file: `path` or `path:line`. */
export function locationText(location: DeepReadonly<Location>): string {
  return location.line === null ? location.path : `${location.path}:${location.line}`
}

/** `N earlier lines not kept`, with the plural as it should be. */
export function droppedText(dropped: number): string {
  return `${count(dropped)} earlier ${dropped === 1 ? 'line' : 'lines'} not kept`
}

/** `and N more hunks`, with the plural as it should be. */
export function moreHunks(more: number): string {
  return `and ${count(more)} more ${more === 1 ? 'hunk' : 'hunks'}`
}

// A picture's type: `image/` and a subtype of the letters a type has.
const IMAGE = /^image\/[a-z0-9][a-z0-9.+-]*$/i
// Base64, whole: groups of four, the last padded.
const BASE64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/

/** How a picture is shown: drawn from its data URI, or why it isn't. */
export type Shown =
  | { type: 'Drawn'; source: string }
  | { type: 'TooBig' }
  | { type: 'NotImage' }
  | { type: 'Unreadable' }

/**
 * How a picture is shown. It's drawn only from a data URI of an image
 * type, with whole base64 data. Anything else, such as `text/html`, never
 * becomes a URI: one too big to keep has no data; one of another type
 * isn't an image; one of an image type whose data isn't whole base64
 * can't be read.
 */
export function imageShown(mime: string, data: string | null): Shown {
  if (data === null) return { type: 'TooBig' }
  if (!IMAGE.test(mime)) return { type: 'NotImage' }
  if (data === '' || !BASE64.test(data)) return { type: 'Unreadable' }
  return { type: 'Drawn', source: `data:${mime.toLowerCase()};base64,${data}` }
}

/**
 * A link's address, parsed, if it's one this view lets the user follow: a
 * web page, over http or https. Anything else (`javascript:`, `data:`, a
 * file) is shown as text, never followed. The address is the parsed one,
 * the one the browser would go to, its host in ASCII (`xn--…` for one
 * spelled in other scripts), so what's shown is where it goes.
 */
export function followable(uri: string): string | null {
  let url: URL
  try {
    url = new URL(uri)
  } catch {
    return null
  }
  return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : null
}

/** A link's name, or null if it has none worth showing: empty, or only spaces. */
export function linkName(name: string): string | null {
  return name.trim() === '' ? null : name
}

/**
 * What CAF asks about a call to one of its own tools, in its own words,
 * from what the call does: never the agent's words. `agent` names the
 * agent, such as `The agent`.
 */
export function cafAsks(action: DeepReadonly<Action> | null, agent: string): string {
  switch (action?.type) {
    case 'Read':
      return `${agent} wants to read a file`
    case 'Edit':
      return `${agent} wants to change a file`
    case 'Write':
      return `${agent} wants to write a file`
    case 'Run':
      return `${agent} wants to run a command`
    default:
      return `${agent} wants to use a tool`
  }
}

/**
 * What a call to one of CAF's own tools is about, for CAF's question
 * about it, from the title CAF wrote for it: the file, without the title's
 * `Read `, `Edit ` or `Write `, or for a command, the title as it is,
 * which is already its first line, capped. So the question never says
 * the verb twice, nor works anything out the core didn't. Null for any
 * other action.
 */
export function cafAbout(tool: DeepReadonly<Pick<ToolCall, 'action' | 'title'>>): string | null {
  const verb = { Read: 'Read ', Edit: 'Edit ', Write: 'Write ' } as Record<string, string>
  switch (tool.action?.type) {
    case 'Read':
    case 'Edit':
    case 'Write': {
      const prefix = verb[tool.action.type]!
      return tool.title.startsWith(prefix) ? tool.title.slice(prefix.length) : tool.title
    }
    case 'Run':
      return tool.title
    default:
      return null
  }
}
