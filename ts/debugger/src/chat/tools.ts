// What the chat view says about a tool call, and what it lets through of
// what an agent gave: an image only as a picture, a link only to the web.

import type { Action, Cut, Decision, DeepReadonly, Ending, Location, ToolStatus } from '@clackatoo/caf'
import { count } from './figures.ts'
import { tagOf } from './words.ts'

/** Whether a call has ended: Done, Failed, Refused or Cancelled. */
export function ended(status: DeepReadonly<ToolStatus>): boolean {
  return ['Done', 'Failed', 'Refused', 'Cancelled'].includes(status.type)
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
 * about it: the file, from the chat's folder `cwd` when it's inside it,
 * as CAF's titles say it, or the command's first line, `…` after it if
 * it has more. Null for an action that's about neither.
 */
export function cafAbout(action: DeepReadonly<Action> | null, cwd: string): string | null {
  switch (action?.type) {
    case 'Read':
    case 'Edit':
    case 'Write': {
      const path = action.value.path
      const folder = cwd.replace(/\/+$/, '')
      const inside = path.startsWith(`${folder}/`) ? path.slice(folder.length + 1) : ''
      return inside === '' ? path : inside
    }
    case 'Run': {
      const [first = '', ...rest] = action.value.command.trim().split('\n')
      return rest.length > 0 ? `${first} …` : first
    }
    default:
      return null
  }
}
