// What the chat view says about CAF's states and reasons, in CAF's words.
//
// A later CAF may send a reason this view doesn't know, so every `switch`
// on one has a `default` that names it by its tag: the types list only
// the ones this version knows, but a new one still comes through.

import type {
  CannotInstall,
  ChatState,
  DeepReadonly,
  FailureReason,
  Incompatible,
  InstallFailure,
  InstallState,
  Lost,
  Note,
  OptionKind,
  Refused,
  Remedy,
  SignedInAs,
  SignInState,
  ToolKind,
  ToolStatus,
  TurnLife,
} from '@clackatoo/caf'
import { CafError } from '@clackatoo/caf'

/** The tag of a value this view doesn't know, from a later CAF. */
export function tagOf(value: unknown): string {
  if (typeof value === 'object' && value !== null && 'type' in value) {
    const type = (value as { type: unknown }).type
    if (typeof type === 'string') return type
  }
  return '?'
}

/** A chat's state, in a word or two. */
export function stateLabel(state: DeepReadonly<ChatState>): string {
  switch (state.type) {
    case 'Opening':
      return 'Opening'
    case 'Open':
      return 'Open'
    case 'Lost':
      return 'Lost'
    case 'Closed':
      return 'Closed'
    case 'WaitingForAgent':
      return 'Waiting for its agent'
    default:
      return `Unknown state (${tagOf(state)})`
  }
}

/** Why a chat lost its session, in one sentence. Detail (what the system or agent said) is drawn apart. */
export function lostText(reason: DeepReadonly<Lost>): string {
  switch (reason.type) {
    case 'CouldNotStart':
      return "The agent couldn't be started."
    case 'AgentStopped':
      return 'The agent stopped.'
    case 'Restarted':
      return 'The agent was restarted, for this chat or another sharing it.'
    case 'SignInNeeded':
      return 'The agent wants you to sign in first.'
    case 'Refused':
      return 'The agent refused to open the session.'
    case 'Stopped':
      return 'You stopped it opening.'
    case 'Incompatible':
      return `The agent can't do what this chat needs: ${incompatibleText(reason.value)}`
    case 'InUse':
      return 'Another chat already has this session open, and one session is never open twice.'
    case 'ShutDown':
      return 'CAF was shut down.'
    default:
      return `The session was lost, for a reason this viewer doesn't know (${tagOf(reason)}).`
  }
}

/**
 * What the user's next message does about a lost session, or null if it
 * can't help: after a shutdown, or an adapter that can't do what the chat
 * needs.
 */
export function lostNext(reason: DeepReadonly<Lost>): string | null {
  switch (reason.type) {
    case 'CouldNotStart':
      return 'Your next message tries to start it again.'
    case 'SignInNeeded':
      return 'Sign in, then send a message to open it again.'
    case 'Incompatible':
      return reason.value.type === 'BadAnswer' ? 'Your next message tries again.' : null
    case 'ShutDown':
      return null
    case 'AgentStopped':
    case 'Restarted':
    case 'Refused':
    case 'Stopped':
    case 'InUse':
      return 'Your next message opens it again.'
    default:
      return null
  }
}

/** What an adapter can't do, finishing `lostText`'s sentence. */
export function incompatibleText(what: DeepReadonly<Incompatible>): string {
  switch (what.type) {
    case 'Protocol':
      return what.value.version === null
        ? "it speaks a version of ACP that CAF doesn't."
        : `it speaks version ${what.value.version} of ACP, which CAF doesn't.`
    case 'HttpMcp':
      return "it can't reach MCP servers over HTTP, and this chat has one."
    case 'BadAnswer':
      return 'its answer made no sense.'
    default:
      return `a reason this viewer doesn't know (${tagOf(what)}).`
  }
}

/** Why a turn failed, in CAF's own words. */
export function failureText(reason: DeepReadonly<FailureReason>): string {
  switch (reason.type) {
    case 'TooLong':
      return "The answer hit the model's length limit."
    case 'TooManySteps':
      return 'The agent hit its limit of steps for one turn.'
    case 'Refused':
      return "The model refused. The message and everything after it are gone from the agent's memory."
    case 'SignInNeeded':
      return 'The agent wants you to sign in.'
    case 'AccessDenied':
      return "The account or its credentials can't be used: signing in again doesn't fix it."
    case 'BadRequest':
      return "The agent's provider refused the request as it stood. Sending it again won't help."
    case 'UsageLimit':
      return 'The agent hit a usage limit.'
    case 'ProviderError':
      return "The agent's provider was overloaded or failed."
    case 'AgentStopped':
      return 'The agent stopped while working on it.'
    case 'DidNotStop':
      return "The agent didn't stop when asked, so CAF ended the turn and restarted the session."
    case 'BadAnswer':
      return "The agent's answer made no sense."
    case 'Other':
      return 'The agent reported an error.'
    default:
      return `It failed, for a reason this viewer doesn't know (${tagOf(reason)}).`
  }
}

/**
 * What the user can do about a failure, for the remedies the view can't
 * carry out itself (Send again is a button). Null for Send again.
 */
export function remedyText(remedy: DeepReadonly<Remedy>): string | null {
  switch (remedy.type) {
    case 'SendAgain':
      return null
    case 'SignIn':
      return 'Sign in to the agent, then send again.'
    case 'NewChat':
      return 'Start a new chat with the same agent and folder.'
    default:
      return `CAF suggests a remedy this viewer doesn't know (${tagOf(remedy)}).`
  }
}

/** Something CAF says in a turn. */
export function noteText(note: DeepReadonly<Note>): string {
  switch (note.type) {
    case 'SessionGone':
      return "The agent no longer had this chat's session, so a new one was opened: it remembers nothing from before."
    case 'CannotContinue':
      return "The agent can't continue sessions, so a new one was opened: it remembers nothing from before."
    case 'SessionStuck':
      return "The agent's session was stuck and couldn't be closed, so a new one was opened: it remembers nothing from before."
    case 'NotInstalled':
      return "The agent isn't installed, so the message that waited for it was stopped."
    case 'InstallFailed':
      return "The agent's install failed, so the message that waited for it was stopped."
    case 'SignedIn':
      return 'You signed in to the agent: send the message again to try it now.'
    default:
      return `A note this viewer doesn't know (${tagOf(note)}).`
  }
}

/** A turn's life, as its badge says it. */
export function lifeLabel(life: DeepReadonly<TurnLife>): string {
  switch (life.type) {
    case 'Waiting':
      return 'Waiting'
    case 'Running':
      return 'Running'
    case 'Stopping':
      return 'Stopping'
    case 'Finished':
      return 'Finished'
    case 'Stopped':
      return 'Stopped'
    case 'Failed':
      return 'Failed'
    case 'CutOff':
      return 'Cut off'
    default:
      return `Unknown (${tagOf(life)})`
  }
}

/** A word on what a life means, for the badge's tooltip. */
export function lifeHint(life: DeepReadonly<TurnLife>): string {
  switch (life.type) {
    case 'Waiting':
      return 'Waiting to be sent: it goes once the turn before it ends.'
    case 'Running':
      return 'The agent is working on it.'
    case 'Stopping':
      return 'Asked to stop: waiting for the agent to.'
    case 'Finished':
      return 'The agent finished.'
    case 'Stopped':
      return 'Stopped before the agent finished.'
    case 'Failed':
      return 'It failed: see why below.'
    case 'CutOff':
      return 'Cut off: the session ended before the agent finished.'
    default:
      return "A state this viewer doesn't know."
  }
}

/** A tool's kind: an icon and its name. */
export function toolKind(kind: DeepReadonly<ToolKind>): { icon: string; name: string } {
  switch (kind.type) {
    case 'Read':
      return { icon: '📄', name: 'Read' }
    case 'Edit':
      return { icon: '✏️', name: 'Edit' }
    case 'Delete':
      return { icon: '🗑️', name: 'Delete' }
    case 'Move':
      return { icon: '↔️', name: 'Move' }
    case 'Search':
      return { icon: '🔍', name: 'Search' }
    case 'Execute':
      return { icon: '▶️', name: 'Run' }
    case 'Think':
      return { icon: '💭', name: 'Think' }
    case 'Fetch':
      return { icon: '🌐', name: 'Fetch' }
    case 'Other':
      return { icon: '🔧', name: 'Tool' }
    default:
      return { icon: '🔧', name: `Unknown kind (${tagOf(kind)})` }
  }
}

/** A tool call's status. */
export function toolStatus(status: DeepReadonly<ToolStatus>): string {
  switch (status.type) {
    case 'Pending':
      return 'Pending'
    case 'Running':
      return 'Running'
    case 'Done':
      return 'Done'
    case 'Failed':
      return 'Failed'
    default:
      return `Unknown (${tagOf(status)})`
  }
}

/** What a permission option means, for its style: allow, reject or neither. */
export function optionTone(kind: DeepReadonly<OptionKind>): 'allow' | 'reject' | 'other' {
  switch (kind.type) {
    case 'AllowOnce':
    case 'AllowAlways':
      return 'allow'
    case 'RejectOnce':
    case 'RejectAlways':
      return 'reject'
    default:
      return 'other'
  }
}

/** Why the core refused something asked of it. */
export function refusedText(reason: DeepReadonly<Refused>): string {
  switch (reason.type) {
    case 'NoSuchChat':
      return 'There is no such chat.'
    case 'Closed':
      return 'The chat is closed.'
    case 'NoSuchAgent':
      return 'There is no such agent.'
    case 'RelativeFolder':
      return 'The folder must be a full path, such as /home/me/project.'
    case 'Unsendable':
      return "The message can't be sent as it is."
    case 'QueueFull':
      return 'Too many messages are waiting.'
    case 'Unsupported':
      return "The agent doesn't support that."
    case 'CommandNotAlone':
      return 'A command must be sent on its own.'
    case 'NoSuchQuestion':
      return 'That question has already gone.'
    case 'WrongAnswer':
      return "That answer doesn't fit the question."
    case 'NothingToStop':
      return 'There is nothing to stop.'
    case 'NotInstallable':
      return "CAF can't install this agent: the host starts it, or it can't be installed on this machine."
    case 'AlreadyInstalled':
      return 'The agent is already installed.'
    case 'CannotSignIn':
      return "CAF can't sign in to or out of this agent now."
    case 'NotSigningIn':
      return "The agent isn't being signed in to."
    default:
      return `Refused, for a reason this viewer doesn't know (${tagOf(reason)}).`
  }
}

/** Why something asked of CAF wasn't done, for any error a callback throws. */
export function errorText(error: unknown): string {
  if (error instanceof CafError) {
    const why = error.why
    switch (why.type) {
      case 'Refused':
        return refusedText(why.value)
      case 'NotAllowed':
        return "The host doesn't let this viewer do that."
      case 'Gone':
        return 'CAF has shut down: nothing was done.'
      case 'Unknown':
        return `It went in a way this viewer doesn't know (${why.value.outcome}).`
      case 'Disconnected':
        return 'The link to CAF dropped before the answer came.'
      case 'NoCommonVersion':
        return 'This viewer and CAF have no version of the wire in common.'
      default:
        return `It wasn't done (${tagOf(why)}).`
    }
  }
  return error instanceof Error ? error.message : String(error)
}

/** A number of bytes, roughly, in MB or GB (powers of 1000, as downloads are sized). */
export function bytes(count: number): string {
  if (!Number.isFinite(count) || count < 0) return 'an unknown size'
  if (count >= 1e9) return `${(count / 1e9).toFixed(1)} GB`
  if (count >= 1e6) return `${Math.round(count / 1e6)} MB`
  return 'less than 1 MB'
}

/** How far an install has got, as a share from 0 to 1. */
export function installed(done: number, size: number): number {
  if (!(size > 0) || !(done > 0)) return 0
  return Math.min(1, done / size)
}

/** Whether an agent is installed, in a few words. */
export function installLabel(state: DeepReadonly<InstallState>): string {
  switch (state.type) {
    case 'Host':
      return 'Started by the app'
    case 'Unknown':
      return 'Checking…'
    case 'Missing':
      return 'Not installed'
    case 'Installing':
      return 'Installing'
    case 'Installed':
      return `Installed, version ${state.value.version}`
    case 'Cannot':
      return "Can't install"
    default:
      return `Unknown (${tagOf(state)})`
  }
}

/** Why an agent can't be installed, finishing "It can't be installed: ". */
export function cannotText(why: DeepReadonly<CannotInstall>): string {
  switch (why.type) {
    case 'Node':
      return why.value.has === null
        ? `it needs Node ${why.value.needs} or newer, and this machine has no Node.`
        : `it needs Node ${why.value.needs} or newer, and this machine has Node ${why.value.has}.`
    case 'Npm':
      return 'it needs npm, and this machine has none.'
    case 'Platform':
      return "it isn't made for this machine's system or processor."
    case 'NoRecipe':
      return 'CAF has no recipe for installing it: the app starts it itself.'
    case 'Failed':
      return failedText(why.value)
    default:
      return `a reason this viewer doesn't know (${tagOf(why)}).`
  }
}

/** Why an install failed, finishing "It can't be installed: ". */
export function failedText(why: DeepReadonly<InstallFailure>): string {
  switch (why.type) {
    case 'Offline':
      return 'the last try failed because the network was out of reach.'
    case 'Corrupt':
      return "the last try failed: what was downloaded wasn't what CAF expected."
    case 'NoSpace':
      return 'the last try failed: the disk is full.'
    case 'Stopped':
      return 'the last try was stopped.'
    case 'Other':
      return `the last try failed: ${why.value}`
    default:
      return `the last try failed, for a reason this viewer doesn't know (${tagOf(why)}).`
  }
}

/** Whether trying to install again may help: only after a failed try. */
export function canRetry(state: DeepReadonly<InstallState>): boolean {
  return state.type === 'Cannot' && state.value.type === 'Failed'
}

/** Whether an agent is signed in, in a few words. */
export function signInLabel(state: DeepReadonly<SignInState>): string {
  switch (state.type) {
    case 'Unknown':
      return 'Sign-in not known yet'
    case 'SignedOut':
      return 'Signed out'
    case 'SigningIn':
      return 'Signing in'
    case 'SignedIn':
      return `Signed in${signedInAs(state.value)}`
    default:
      return `Unknown (${tagOf(state)})`
  }
}

/** How an agent is signed in, finishing "Signed in". */
function signedInAs(how: DeepReadonly<SignedInAs>): string {
  switch (how.type) {
    case 'Account':
      return ' with an account'
    case 'ApiKey':
      return ' with an API key'
    case 'Gateway':
      return ' through a gateway'
    case 'Unknown':
      return ''
    default:
      return ` (${tagOf(how)})`
  }
}

/**
 * A sign-in link, if it's one to open: an https address, and nothing
 * else (`javascript:`, `data:`, `http:`). CAF has already checked its
 * site against the agent's profile; this is the view's own check.
 */
export function signInUrl(url: string | null): URL | null {
  if (url === null) return null
  try {
    const parsed = new URL(url)
    return parsed.protocol === 'https:' ? parsed : null
  } catch {
    return null
  }
}
