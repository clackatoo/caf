// `npm run dev`'s proxy to caf-debugger. The server only takes requests
// from its own origin and for its own Host, so the dev server passes on
// its own page's as if they came from there, and always names the
// server's Host (`changeOrigin`). Anything else goes as it came, for the
// server to refuse: rewriting every Origin would let any website reach
// caf-debugger through the dev server, with only the token to stop it.

/**
 * The Origin to send on: the server's, for the dev page's own, else the
 * one given.
 */
export function origin(given: string | undefined, host: string | undefined, server: string): string | undefined {
  // The dev page's own origin is its Host, on this machine. A name that
  // isn't loopback, such as a page whose name points here (DNS
  // rebinding), isn't the dev page.
  const local = host !== undefined && /^(localhost|127\.0\.0\.1|\[::1\]):\d+$/.test(host)
  return local && given === `http://${host}` ? server : given
}

type Request = { setHeader(name: string, value: string): void; removeHeader(name: string): void }
type Incoming = { headers: Record<string, string | string[] | undefined> }

/** A Vite proxy route to caf-debugger at `server`. */
export function route(server: string) {
  const pass = (request: Request, incoming: Incoming) => {
    const given = incoming.headers.origin
    const host = incoming.headers.host
    const sent = origin(typeof given === 'string' ? given : undefined, typeof host === 'string' ? host : undefined, server)
    if (sent === undefined) request.removeHeader('origin')
    else request.setHeader('origin', sent)
  }
  return {
    target: server,
    ws: true,
    // The Host header becomes the server's; `pass` reads the dev page's
    // own from the request as it came.
    changeOrigin: true,
    configure: (proxy: { on(event: string, listener: (request: Request, incoming: Incoming) => void): void }) => {
      proxy.on('proxyReq', pass)
      proxy.on('proxyReqWs', pass)
    },
  }
}
