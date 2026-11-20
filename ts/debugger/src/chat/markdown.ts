// The agent's words as Markdown, made safe to put in the page.
//
// What an agent writes is untrusted: it may repeat a web page, a file, or
// anything else it read. HTML in it would let it script this page, and
// the page holds the token that lets it start agents. So:
//
// - HTML is off (`html: false`): `<script>` and `<img onerror>` come out
//   as text;
// - only http(s) links are links, and each opens in a new tab with
//   `rel="noopener noreferrer"`, so the page it opens can't reach this one;
// - images aren't loaded, since that would fetch an address the agent
//   chose as the page draws: each is drawn as a link to it instead.
//
// `render` is the only HTML the chat view puts in the page as it is (in
// `Markdown.svelte`); everything else goes through Svelte's escaping.

import MarkdownIt from 'markdown-it'

const md = MarkdownIt({ html: false, linkify: true, breaks: false })

/** Only http and https: no `javascript:`, `data:`, `file:`, `mailto:`, and nothing relative to this page. */
md.validateLink = (url) => /^https?:\/\//i.test(url.trim())

md.renderer.rules.link_open = (tokens, idx, options, _env, self) => {
  const token = tokens[idx]!
  token.attrSet('target', '_blank')
  token.attrSet('rel', 'noopener noreferrer')
  return self.renderToken(tokens, idx, options)
}

md.renderer.rules.image = (tokens, idx, options, env, self) => {
  const token = tokens[idx]!
  const src = String(token.attrGet('src') ?? '')
  const alt = self.renderInlineAsText(token.children ?? [], options, env)
  const text = md.utils.escapeHtml(alt === '' ? src : alt)
  return `<a href="${md.utils.escapeHtml(src)}" target="_blank" rel="noopener noreferrer" class="image-link">🖼 ${text}</a>`
}

/** Markdown as HTML that's safe to put in the page. */
export function render(text: string): string {
  return md.render(text)
}
