// Where the app finds caf-debugger, and the token that lets it in.
//
// caf-debugger prints its address with a fresh token in the fragment,
// `http://127.0.0.1:7320/#token=…`, since a fragment never reaches a
// server's log. The app reads it once, as it starts, and takes it out of
// the address bar and the history at once, so it isn't left on screen or
// in a bookmark. From then on it's only in memory, and goes on each
// WebSocket's `?token=` and the bug report's download.

/** How a link to the server stands. */
export type LinkState = 'connecting' | 'open' | 'closed'

let token = ''

/**
 * Reads the token from `location.hash`, and removes it from the address.
 * Called once, before anything else runs. Other parts of the fragment are
 * kept.
 */
export function takeToken(location: Location = window.location, history: History = window.history): string {
  const fragment = new URLSearchParams(location.hash.replace(/^#/, ''))
  token = fragment.get('token') ?? ''
  if (fragment.has('token')) {
    fragment.delete('token')
    const rest = fragment.toString()
    history.replaceState(history.state, '', location.pathname + location.search + (rest === '' ? '' : `#${rest}`))
  }
  return token
}

/** Whether the page was opened with a token. */
export function hasToken(): boolean {
  return token !== ''
}

/** A WebSocket address on this server, with the token. */
export function socketUrl(path: string, location: Location = window.location): string {
  const url = new URL(path, location.href)
  url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
  url.searchParams.set('token', token)
  url.hash = ''
  return url.toString()
}

/** An HTTP address on this server, with the token. */
export function httpUrl(path: string, location: Location = window.location): string {
  const url = new URL(path, location.href)
  url.searchParams.set('token', token)
  url.hash = ''
  return url.toString()
}
