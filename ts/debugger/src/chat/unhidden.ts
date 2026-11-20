// Text from an agent with nothing in it that could go unseen or change
// how the text around it shows: the TypeScript twin of caf-core's
// `unhidden` (`core/clean.rs`). The core cleans what it keeps as text,
// such as titles and what an agent said, but not the agent's raw JSON
// (a tool's input, an unknown item), so the view cleans that itself:
// a right-to-left override inside a command could otherwise show it in
// a different order from the one it runs in.
//
// Control characters but line breaks and tabs show as `^U` (DEL as `^?`).
// Characters that reorder text, characters of no width, and tag
// characters show as `<U+202E>`. `\r\n` becomes `\n`.

function hidden(code: number): boolean {
  return (
    // C1 controls.
    (code >= 0x80 && code <= 0x9f) ||
    // Bidirectional controls.
    code === 0x061c ||
    code === 0x200e ||
    code === 0x200f ||
    (code >= 0x202a && code <= 0x202e) ||
    (code >= 0x2066 && code <= 0x2069) ||
    // No width: spaces, joiners, the word joiner, invisible operators, the
    // byte-order mark and the soft hyphen.
    (code >= 0x200b && code <= 0x200d) ||
    (code >= 0x2060 && code <= 0x2064) ||
    code === 0xfeff ||
    code === 0xad ||
    // Tags, and the supplementary variation selectors.
    (code >= 0xe0000 && code <= 0xe007f) ||
    (code >= 0xe0100 && code <= 0xe01ef)
  )
}

export function unhidden(text: string): string {
  let shown = ''
  for (const ch of text.replace(/\r\n/g, '\n')) {
    const code = ch.codePointAt(0)!
    if (ch === '\n' || ch === '\t') shown += ch
    else if (code === 0x7f) shown += '^?'
    else if (code < 0x20) shown += `^${String.fromCharCode(0x40 + code)}`
    else if (hidden(code)) shown += `<U+${code.toString(16).toUpperCase().padStart(4, '0')}>`
    else shown += ch
  }
  return shown
}

/** An agent's raw JSON, laid out, with nothing in it hidden. */
export function rawJson(value: unknown): string {
  return unhidden(JSON.stringify(value, null, 2) ?? 'undefined')
}
