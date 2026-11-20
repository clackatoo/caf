// A hand-written stand-in for the types ts-rs will generate from caf-core,
// for reviewing the API draft. Once built, `src/types/` is generated and
// checked in, and CI fails if it's stale. It shows the shape the generated
// types will have: every CAF enum is `{ type, value }`.

export type ChatId = number;
export type TurnId = number;
export type ItemId = number;
export type QuestionId = number;
export type ChangeNo = number;
export type Wall = number;
export type AgentName = string;
export type AgentSessionId = string;
export type OptionId = string;
export type RequestNo = number;

export type End = { type: "Open" } | { type: "At"; value: Wall } | { type: "Unknown" };
export type Window = { start: Wall | null; end: End };

export type Chat = {
  id: ChatId;
  agent: AgentName;
  cwd: string;
  state: ChatState;
  turns: Turn[];
  usage: Usage | null;
  questions: Question[];
};

export type ChatState =
  | { type: "Opening" }
  | { type: "Open"; value: { session: AgentSessionId } }
  | { type: "Lost"; value: { reason: Lost } }
  | { type: "Closed" };

export type Lost =
  | { type: "CouldNotStart"; value: { error: string } }
  | { type: "AgentStopped"; value: { stderr: string[] } }
  | { type: "Restarted" }
  | { type: "SignInNeeded" }
  | { type: "Refused"; value: { message: string } }
  | { type: "Stopped" }
  | { type: "Incompatible"; value: unknown }
  | { type: "InUse" }
  | { type: "ShutDown" };

export type Usage = { used: number; size: number; cost: { amount: number; currency: string } | null };

export type Turn = {
  id: TurnId;
  starter: Starter;
  life: TurnLife;
  window: Window;
  items: Item[];
  imported: boolean;
};

export type UserMessage = { parts: Part[] };
export type Part = { type: "Text"; value: string };

export type Starter =
  | { type: "User"; value: UserMessage }
  | { type: "Agent" }
  | { type: "OtherChat"; value: { from: ChatId; message: UserMessage } }
  | { type: "Notice"; value: string }
  | { type: "Caf" };

export type TurnLife =
  | { type: "Waiting" }
  | { type: "Running" }
  | { type: "Stopping" }
  | { type: "Finished" }
  | { type: "Stopped" }
  | { type: "Failed"; value: Failure }
  | { type: "CutOff" };

export type Failure = { reason: { type: string }; agent_said: string | null; remedy: { type: string } | null };

export type Item = { id: ItemId; window: Window; parent: ItemId | null; kind: ItemKind };

export type ItemKind =
  | { type: "Words"; value: string }
  | { type: "Thinking"; value: string }
  | { type: "Tool"; value: ToolCall }
  | { type: "Compacted"; value: Record<string, never> }
  | { type: "Note"; value: { type: "SessionGone" | "CannotContinue" | "SessionStuck" } }
  | { type: "App"; value: never }
  | { type: "Unknown"; value: { what: string; raw: unknown } };

export type ToolCall = {
  title: string;
  kind: { type: "Read" | "Edit" | "Delete" | "Move" | "Search" | "Execute" | "Think" | "Fetch" | "Other" };
  status: { type: "Pending" | "Running" | "Done" | "Failed" };
  input: unknown;
  output: string;
  app: never | null;
};

export type Question = { id: QuestionId; window: Window; kind: QuestionKind };
export type QuestionKind =
  | { type: "Permission"; value: { tool: ItemId | null; title: string; options: PermissionOption[] } }
  | { type: "SlowToOpen"; value: { adapter_answered: boolean } };
export type PermissionOption = {
  id: OptionId;
  label: string;
  kind: { type: "AllowOnce" | "AllowAlways" | "RejectOnce" | "RejectAlways" | "Other" };
};

export type Answer =
  | { type: "Permission"; value: OptionId }
  | { type: "SlowToOpen"; value: { type: "KeepWaiting" | "Retry" | "Stop" } };

export type Change =
  | { type: "State"; value: ChatState }
  | { type: "TurnAdded"; value: Turn }
  | { type: "TurnInserted"; value: { before: TurnId; turn: Turn } }
  | { type: "TurnLife"; value: { turn: TurnId; life: TurnLife; end: End } }
  | { type: "ItemAdded"; value: { turn: TurnId; item: Item } }
  | { type: "Appended"; value: { item: ItemId; text: string } }
  | { type: "ToolChanged"; value: { item: ItemId; tool: ToolCall } }
  | { type: "ItemEnded"; value: { item: ItemId; end: End } }
  | { type: "Usage"; value: Usage }
  | { type: "QuestionAsked"; value: Question }
  | { type: "QuestionGone"; value: QuestionId };

export type Numbered = { no: ChangeNo; change: Change };

export type Refused = { type: string };
export type SessionChoice = { type: "New" } | { type: "Continue"; value: AgentSessionId };
export type OpenRequest = { agent: AgentName; cwd: string; session: SessionChoice };

export type Outcome =
  | { type: "Opened"; value: ChatId }
  | { type: "Sent"; value: TurnId }
  | { type: "Done" }
  | { type: "Refused"; value: Refused }
  | { type: "NotAllowed" };

export type FromViewer =
  | { type: "Hello"; value: { min: number; max: number } }
  | { type: "Watch"; value: { chat: ChatId; since: ChangeNo | null } }
  | { type: "Unwatch"; value: { chat: ChatId } }
  | { type: "Open"; value: { request: RequestNo; open: OpenRequest } }
  | { type: "Send"; value: { request: RequestNo; chat: ChatId; message: UserMessage } }
  | { type: "Stop"; value: { request: RequestNo; chat: ChatId } }
  | { type: "Answer"; value: { request: RequestNo; question: QuestionId; answer: Answer } }
  | { type: "Close"; value: { request: RequestNo; chat: ChatId } };

export type ToViewer =
  | { type: "Welcome"; value: { version: number; agents: AgentName[]; chats: ChatId[] } }
  | { type: "NoCommonVersion"; value: { min: number; max: number } }
  | { type: "Chats"; value: ChatId[] }
  | { type: "Picture"; value: { chat: Chat; latest: ChangeNo } }
  | { type: "Changes"; value: { chat: ChatId; changes: Numbered[] } }
  | { type: "Ended"; value: { chat: ChatId } }
  | { type: "Done"; value: { request: RequestNo; outcome: Outcome } };
