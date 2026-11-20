// What changed between two JSON values: the paths of everything added,
// changed or removed, so the State tab can mark them.
//
// A path is its keys and indexes joined by `/`, from the root (`''`), as
// in `/chats/0/state`. Keys with `/` in them are escaped as JSON Pointer
// does (`~1`, and `~` as `~0`).

export interface Diff {
  /** Paths whose value is new or different, and every path above them. */
  changed: ReadonlySet<string>
  /** Paths that were there before and aren't now, with what was there. */
  removed: ReadonlyMap<string, unknown>
}

export function key(segment: string | number): string {
  return String(segment).replace(/~/g, '~0').replace(/\//g, '~1')
}

export function diff(before: unknown, after: unknown): Diff {
  const changed = new Set<string>()
  const removed = new Map<string, unknown>()
  walk(before, after, '', changed, removed)
  return { changed, removed }
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** Whether anything under `path` changed; marks `path` and everything under it that did. */
function walk(before: unknown, after: unknown, path: string, changed: Set<string>, removed: Map<string, unknown>): boolean {
  if (Array.isArray(before) && Array.isArray(after)) {
    let any = false
    for (let i = 0; i < after.length; i++) {
      const child = `${path}/${i}`
      if (i >= before.length) any = mark(after[i], child, changed) || any
      else any = walk(before[i], after[i], child, changed, removed) || any
    }
    for (let i = after.length; i < before.length; i++) {
      removed.set(`${path}/${i}`, before[i])
      any = true
    }
    if (any) changed.add(path)
    return any
  }
  if (isObject(before) && isObject(after)) {
    let any = false
    for (const [name, value] of Object.entries(after)) {
      const child = `${path}/${key(name)}`
      if (!Object.hasOwn(before, name)) any = mark(value, child, changed) || any
      else any = walk(before[name], value, child, changed, removed) || any
    }
    for (const [name, value] of Object.entries(before)) {
      if (!Object.hasOwn(after, name)) {
        removed.set(`${path}/${key(name)}`, value)
        any = true
      }
    }
    if (any) changed.add(path)
    return any
  }
  if (Object.is(before, after)) return false
  return mark(after, path, changed)
}

/** Marks a new value and everything in it. */
function mark(value: unknown, path: string, changed: Set<string>): true {
  changed.add(path)
  if (Array.isArray(value)) value.forEach((child, i) => mark(child, `${path}/${i}`, changed))
  else if (isObject(value)) for (const [name, child] of Object.entries(value)) mark(child, `${path}/${key(name)}`, changed)
  return true
}
