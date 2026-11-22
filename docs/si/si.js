// CAF SI's shared composer: the one every page draws, so they can't drift
// apart. compose.html is where it's designed and explained; the other pages
// use it as it is. Needs si.css.
//
//   SI.composer(state) -> HTML
//
// A state says what the chat is doing; anything left out has its default
// (see DEFAULTS). Pages put what attaches on top of the box (a question, the
// agent banner, a refusal) in `attached`, with `joined` saying its tone.

(() => {
  const svg = (d, cls = '', style = '') => `<svg class="si-i ${cls}" viewBox="0 0 24 24"${style ? ` style="${style}"` : ''}>${d}</svg>`
  const icon = {
    send: svg('<path d="M12 19V5M5.5 11.5L12 5l6.5 6.5"/>', '', 'width:17px;height:17px'),
    stop: svg('<rect x="3" y="3" width="18" height="18" rx="3"/>', 'si-fill', 'width:12px;height:12px'),
    chev: svg('<path d="M6 9l6 6 6-6"/>', 'si-chev', 'width:12px;height:12px'),
    bolt: svg('<path d="M13 2L4 14h7l-1 8 9-12h-7z"/>', 'si-fill', 'width:13px;height:13px;color:var(--accent-text)'),
    boltLine: svg('<path d="M13 2L4 14h7l-1 8 9-12h-7z"/>'),
    boltFill: svg('<path d="M13 2L4 14h7l-1 8 9-12h-7z"/>', 'si-fill'),
    clock: svg('<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>', '', 'width:14px;height:14px'),
    tick: svg('<path d="M5 12l5 5 9-11"/>', '', 'width:15px;height:15px'),
    warn: svg('<path d="M12 3l10 18H2z"/><path d="M12 10v4M12 17.5v.5"/>', '', 'width:15px;height:15px'),
    lock: svg('<rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V8a4 4 0 018 0v3"/>', '', 'width:15px;height:15px'),
    unplug: svg('<path d="M9 7V3M15 7V3M7 7h10v4a5 5 0 01-10 0zM12 16v5M3 3l18 18"/>', '', 'width:15px;height:15px'),
    search: svg('<circle cx="11" cy="11" r="7"/><path d="M20 20l-4-4"/>', '', 'width:15px;height:15px'),
    star: svg('<path d="M12 3l2.7 5.6 6.1.8-4.5 4.2 1.1 6.1L12 16.8 6.6 19.7l1.1-6.1L3.2 9.4l6.1-.8z"/>', '', 'width:15px;height:15px'),
    starFill: svg('<path d="M12 3l2.7 5.6 6.1.8-4.5 4.2 1.1 6.1L12 16.8 6.6 19.7l1.1-6.1L3.2 9.4l6.1-.8z"/>', 'si-fill', 'width:15px;height:15px'),
    folder: svg('<path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z"/>'),
    spin: svg('<path d="M12 3a9 9 0 109 9"/>', 'si-spin', 'width:15px;height:15px'),
    download: svg('<path d="M12 4v11M7 10l5 5 5-5M5 20h14"/>', '', 'width:14px;height:14px'),
  }

  // The agents' marks, in their colours.
  function mark(agent = 'claude', size = 18) {
    const st = `width:${size}px;height:${size}px`
    if (agent === 'codex') return svg('<circle cx="12" cy="12" r="8.5"/><path d="M8 12h8M12 8v8"/>', '', `${st};color:var(--codex)`)
    if (agent === 'gemini') return svg('<path d="M12 2c.6 5.4 4.6 9.4 10 10-5.4.6-9.4 4.6-10 10-.6-5.4-4.6-9.4-10-10 5.4-.6 9.4-4.6 10-10z"/>', 'si-fill', `${st};color:var(--gemini)`)
    let rays = ''
    for (let n = 0; n < 12; n++) {
      const a = (n * Math.PI) / 6, r = n % 2 ? 7.2 : 10
      rays += `<line x1="12" y1="12" x2="${(12 + r * Math.cos(a)).toFixed(2)}" y2="${(12 + r * Math.sin(a)).toFixed(2)}"/>`
    }
    return svg(rays, '', `${st};color:var(--claude);stroke-width:2.6`)
  }
  const AGENT_NAME = { claude: 'Claude', codex: 'Codex', gemini: 'Gemini' }
  const AGENT_FULL = { claude: 'Claude Code', codex: 'Codex', gemini: 'Gemini' }

  // What Claude offers, as recorded live (claude-0.98.0-live.jsonl).
  const MODELS = [
    { name: 'Default (recommended)', d: 'Opus 5.5' },
    { name: 'Opus 5.5', d: 'For complex work and everyday tasks' },
    { name: 'Sonnet 5.5', d: 'Most efficient for simpler tasks' },
    { name: 'Haiku 5.5', d: 'Fastest for quick answers' },
  ]
  // What each model offers beside it: the agent says, and it changes with
  // the model. Opus's are as recorded; Sonnet's are made up, to show a model
  // offering fewer levels and no fast mode.
  const OFFERS = {
    'Opus 5.5': { efforts: ['Low', 'Medium', 'High', 'Xhigh', 'Max'], fast: true },
    'Sonnet 5.5': { efforts: ['Low', 'Medium', 'High'], fast: false },
  }
  // Favourites are the app's to keep, as clackatoo keeps its own.
  const FAVOURITES = ['Opus 5.5', 'Sonnet 5.5']
  // The agents a new chat may be with, and what each still needs.
  const AGENTS = [{ id: 'claude' }, { id: 'codex', needs: 'Not installed' }, { id: 'gemini', needs: 'Signed out' }]
  const QUEUED = ['Now update the callers in crates/save too, and run its tests.', 'Then write a changelog line for the rename.']

  const DEFAULTS = {
    agent: 'claude',
    text: '', running: false, stopping: false, waiting: 0, waitingFor: 'this turn', waitingNote: null, problem: null, gate: false,
    // A string, or { text, icon: 'lock' | 'unplug', bold, action }: nothing can be sent.
    // An action, such as Unarchive, is the one thing to do, as a button.
    blocked: null,
    // known | unknown | learning | failed | none
    settings: 'known',
    // Why CAF couldn't learn the settings, as the composer page words it, and
    // what the system or the agent said ({ by: 'system' | 'agent', text }),
    // and whether trying again could help.
    learnWhy: "Claude couldn't be started.", learnSaid: null, learnAgain: true, model: 'Opus 5.5', effort: 'High', fast: true, held: {},
    menu: false, filter: 'agent', menuAgent: null,
    // Percent of the context used, or null before there's any (a new chat).
    // The ring's dot for an account limit: false, or the lowest window's lowness, 'low' or 'out'.
    context: 9, ringOpen: false, ringDot: false,
    narrow: false,
    // A new chat: the menu's rail lists every agent. A folder chip only when the app lets users choose one.
    newChat: false, folder: null,
    attached: '', joined: null, placeholder: null, after: '',
  }

  function ring(s) {
    if (s.context == null) return ''
    const p = s.context, r = 15.5, c = 2 * Math.PI * r
    const cls = p > 95 ? ' si-full' : p > 80 ? ' si-filling' : ''
    return `<span class="si-ring${cls}${s.ringOpen ? ' si-open' : ''}" title="Context: ${p}% used"><svg viewBox="0 0 37 37">` +
      `<circle cx="18.5" cy="18.5" r="${r}" fill="none" stroke="var(--raised-edge)" stroke-width="2.5"/>` +
      `<circle cx="18.5" cy="18.5" r="${r}" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="${(c * p / 100).toFixed(1)} ${c.toFixed(1)}"/></svg>${p}%${s.ringDot ? `<span class="si-dot${s.ringDot === 'out' ? ' si-out' : ''}"></span>` : ''}</span>`
  }

  function control(s) {
    const m = mark(s.agent), name = AGENT_NAME[s.agent]
    if (s.settings === 'none') return `<span class="si-control si-quiet">${m}<span class="si-label">${name}</span></span>`
    if (s.settings === 'learning') return `<span class="si-control si-quiet">${m}<span class="si-label">Learning ${name}'s settings…</span>${icon.spin}</span>`
    if (s.settings === 'unknown') return `<span class="si-control si-quiet${s.menu ? ' si-open' : ''}">${m}<span class="si-label">${s.narrow ? 'Settings' : s.newChat ? name : 'Settings not known yet'}</span>${icon.chev}</span>`
    if (s.settings === 'failed') return `<span class="si-control si-quiet${s.menu ? ' si-open' : ''}">${m}<span class="si-label">${s.narrow ? 'Settings' : `Couldn't learn ${name}'s settings`}</span>${icon.chev}</span>`
    const part = (now, later) => later ? `<span class="si-part si-held" title="${s.running ? 'From the next turn' : 'Not applied yet'}">${icon.clock}${later}</span>` : `<span class="si-part">${now}</span>`
    const fast = OFFERS[s.model] && OFFERS[s.model].fast && s.fast
    return `<span class="si-control${s.menu ? ' si-open' : ''}">${m}<span class="si-label">${part(s.model, s.held.model)}<span class="si-sep">·</span>${part(s.effort, s.held.effort)}${fast ? icon.bolt : ''}</span>${icon.chev}</span>`
  }

  function rail(s, fast) {
    const off = s.held && s.held.model ? ' si-off' : ''
    let h = `<div class="si-rail"><span class="si-r${s.filter === 'favs' ? ' si-on' : ''}" title="Favourites">${icon.starFill}</span>`
    const agents = s.newChat ? AGENTS : [{ id: s.agent }]
    for (const a of agents) {
      const on = s.filter !== 'favs' && (s.menuAgent || s.agent) === a.id
      h += `<span class="si-r${on ? ' si-on' : ''}" title="${AGENT_FULL[a.id]}${a.needs ? ': ' + a.needs.toLowerCase() : ''}">${mark(a.id, 20)}${a.needs ? '<span class="si-need"></span>' : ''}</span>`
    }
    h += '<span class="si-grow"></span>'
    if (fast) h += `<span class="si-r${s.fast ? ' si-lit' : ''}${off}" title="Fast mode ${s.fast ? 'on' : 'off'}: faster answers, may cost more">${s.fast ? icon.boltFill : icon.boltLine}</span>`
    return h + '</div>'
  }

  function menu(s) {
    const name = AGENT_NAME[s.agent]
    if (s.menuAgent && s.menuAgent !== s.agent) {
      const a = AGENTS.find(x => x.id === s.menuAgent)
      const what = a.needs === 'Not installed' ? `${AGENT_FULL[a.id]} isn't installed yet.` : `${AGENT_FULL[a.id]} needs you to sign in.`
      return `<div class="si-menu" style="width:min(440px, calc(100% - 44px))">${rail(s, false)}<div class="si-main"><div class="si-note"><b>${what}</b><br>Choose it, and this chat will offer to ${a.needs === 'Not installed' ? 'install it' : 'sign you in'} once you send. Its models show here once it's ready.<br><span class="si-btn si-primary si-act">Use ${AGENT_NAME[a.id]}</span></div></div></div>`
    }
    if (s.settings === 'failed') {
      const said = !s.learnSaid ? '' : s.learnSaid.by === 'agent'
        ? `<div class="si-quote"><b>${name} said:</b> “${s.learnSaid.text}”</div>`
        : `<div class="si-sys">${s.learnSaid.text}</div>`
      const again = s.learnAgain ? ` It tries again when the chat opens, or now.${said || '<br>'}<span class="si-btn" style="margin-top:8px">Learn them now</span>` : said
      return `<div class="si-menu" style="width:min(440px, calc(100% - 44px))">${rail(s, false)}<div class="si-main"><div class="si-note">CAF couldn't learn which models and settings ${name} offers. <b>${s.learnWhy}</b>${again}</div></div></div>`
    }
    if (s.settings === 'unknown') {
      return `<div class="si-menu" style="width:min(440px, calc(100% - 44px))">${rail(s, false)}<div class="si-main"><div class="si-note">CAF doesn't know yet which models and settings ${name} offers. It finds out when the chat opens, or now, without sending a message.<br><span class="si-btn" style="margin-top:8px">Learn them now</span></div></div></div>`
    }
    const offers = OFFERS[s.model]
    let h = `<div class="si-menu">${rail(s, offers.fast)}<div class="si-main"><div class="si-search">${icon.search}Search models…</div><div class="si-list">`
    h += MODELS.filter(m => s.filter !== 'favs' || FAVOURITES.includes(m.name)).map(m => {
      const now = m.name === s.model, later = m.name === s.held.model, fav = FAVOURITES.includes(m.name)
      return `<div class="si-row${now ? ' si-chosen' : ''}"><span class="si-name">${m.name}</span>` +
        `<span class="si-tick">${now ? icon.tick : later ? `<span class="si-when">${icon.clock}after this turn</span>` : ''}</span>` +
        `<span class="si-star${fav ? ' si-fav' : ''}">${fav ? icon.starFill : icon.star}</span><span class="si-sub">${mark(s.agent, 13)}${m.d}</span></div>`
    }).join('') + '</div>'
    const E = offers.efforts, at = E.indexOf(s.effort), pct = n => (n / (E.length - 1)) * 100
    const aside = s.held.effort ? `<span class="si-aside si-held">${icon.clock}${s.held.effort} after this turn</span>` : `<span class="si-aside">${s.effort}</span>`
    h += `<div class="si-effort${s.held.model ? ' si-off' : ''}"><div class="si-heading"><span>Effort${s.held.model ? ' · ' + s.model + "'s: change it once " + s.held.model + ' applies' : ''}</span>${aside}</div><div class="si-slider"><div class="si-track"><div class="si-fill-bar" style="width:${pct(at)}%"></div>` +
      E.map((_, n) => `<span class="si-stop" style="left:${pct(n)}%"></span>`).join('') + `<span class="si-knob" style="left:${pct(at)}%"></span></div>` +
      `<div class="si-names">${E.map((e, n) => `<span class="${n === at ? 'si-on' : ''}">${e}</span>`).join('')}</div></div></div>`
    return h + '</div></div>'
  }

  function composer(state) {
    const s = { ...DEFAULTS, ...state, held: { ...(state.held || {}) } }
    const name = AGENT_NAME[s.agent]
    let h = '<div class="si-foot"><div class="si-column">'
    const waiting = typeof s.waiting === 'number' ? QUEUED.slice(0, s.waiting) : s.waiting
    if (waiting.length) {
      h += `<div class="si-tray"><div class="si-head"><span>Waiting for ${s.waitingFor} · ${waiting.length}</span><span>${s.waitingNote || `Stop drops ${waiting.length === 1 ? 'it' : 'these'} too`}</span></div>` +
        waiting.map(t => `<div class="si-item">${icon.clock}<span class="si-text">${t}</span></div>`).join('') + '</div>'
    }
    if (s.problem) h += `<div class="si-tray si-problem">${icon.warn}${s.problem}</div>`
    h += s.attached
    const joined = s.attached ? ` si-joined${s.joined ? ' si-' + s.joined : ''}` : ''
    h += `<div class="si-box${joined}">`
    if (s.blocked) {
      const b = typeof s.blocked === 'string' ? { text: s.blocked } : s.blocked
      h += `<div class="si-locked">${icon[b.icon || 'lock']}<span>${b.bold ? `<b>${b.bold}</b> ` : ''}${b.text}</span>${b.action ? `<span class="si-spacer"></span><span class="si-btn si-primary">${b.action}</span>` : ''}</div>`
      if (b.controls !== false) h += `<div class="si-controls"><span class="si-control si-quiet">${mark(s.agent)}<span class="si-label">${s.settings === 'known' ? s.model + ' · ' + s.effort : name}</span></span><span class="si-spacer"></span>${ring(s)}</div>`
      return h + `</div>${s.after}</div></div>`
    }
    const ph = s.placeholder || (s.newChat ? `Message ${name} to start` : s.gate ? `Message ${name}: it waits until ${name} is ready` : s.running ? `Message ${name}: it waits for this turn` : `Message ${name}`)
    h += s.text ? `<div class="si-draft">${s.text}<span class="si-caret"></span></div>` : `<div class="si-draft si-ph">${s.narrow ? ph.split(':')[0] : ph}</div>`
    h += `<div class="si-controls">${control(s)}`
    if (s.folder) h += `<span class="si-control">${icon.folder}<span class="si-mono">${s.folder}</span>${icon.chev}</span>`
    h += '<span class="si-spacer"></span>'
    if (s.running && s.text && !s.narrow) h += '<span class="si-waits">waits for this turn</span>'
    h += ring(s)
    if (s.stopping) h += `<span class="si-round" title="Stopping…">${icon.spin}</span>`
    else if (s.running && !s.text) h += `<span class="si-round si-lit" title="Stop${waiting.length ? ', dropping the waiting messages' : ''}">${icon.stop}</span>`
    else if (s.running) h += `<span class="si-round si-small" title="Stop">${icon.stop}</span><span class="si-round si-lit" title="Send: it waits for this turn">${icon.send}</span>`
    else h += `<span class="si-round${s.text ? ' si-lit' : ''}" title="Send">${icon.send}</span>`
    h += '</div></div>'
    if (s.menu) h += menu(s)
    return h + `${s.after}</div></div>`
  }

  window.SI = { composer, control, menu, ring, mark, icon, MODELS, OFFERS, FAVOURITES, AGENTS, AGENT_NAME }
})()
