// An agent's settings, as the chat view offers them: what each is now,
// what was chosen, and whether a change is waiting.
//
// The agent says what's on offer, never CAF or this view: settings are
// drawn by their form (choices, or a switch), whatever they're about, so
// one of a kind this view doesn't know still shows. Choices keep the
// agent's order and groups, and one such as Claude's `default` is shown
// as the agent names it.

import type { Choice, DeepReadonly, KnownSettings, Setting, SettingChoice, Settings, SettingValue } from '@clackatoo/caf'
import { tagOf } from './words.ts'

/** A run of choices the agent puts in the same group, or in none. */
export interface Run {
  group: string | null
  choices: DeepReadonly<Choice>[]
}

/**
 * A setting's choices as runs of the same group, in the agent's order: a
 * group the agent splits stays split, rather than pulled together out of
 * its order.
 */
export function runs(choices: readonly DeepReadonly<Choice>[]): Run[] {
  const all: Run[] = []
  for (const choice of choices) {
    const last = all.at(-1)
    if (last !== undefined && last.group === choice.group) last.choices.push(choice)
    else all.push({ group: choice.group, choices: [choice] })
  }
  return all
}

/**
 * The settings to offer: the chat's, as its agent last said, or before
 * its session first opens, those CAF knows the agent offers, if it knows.
 */
export function offered(
  settings: DeepReadonly<Settings>,
  known: DeepReadonly<KnownSettings> | undefined,
): readonly DeepReadonly<Setting>[] {
  if (settings.offered.length > 0) return settings.offered
  return known?.type === 'Known' ? known.value : []
}

/** What was chosen for a setting, if anything. */
export function chosenFor(setting: DeepReadonly<Setting>, chosen: readonly DeepReadonly<SettingChoice>[]): DeepReadonly<SettingValue> | null {
  return chosen.find((choice) => choice.kind.type === setting.kind.type)?.value ?? null
}

/** Whether two values are the same. */
export function same(a: DeepReadonly<SettingValue> | null, b: DeepReadonly<SettingValue> | null): boolean {
  return a?.type === b?.type && a?.value === b?.value
}

/** A setting as it stands: what to show, and the change waiting, if one is. */
export function standing(
  setting: DeepReadonly<Setting>,
  chosen: readonly DeepReadonly<SettingChoice>[],
): { value: DeepReadonly<SettingValue> | null; waiting: DeepReadonly<SettingValue> | null } {
  const choice = chosenFor(setting, chosen)
  if (choice === null) return { value: setting.current, waiting: null }
  return { value: choice, waiting: same(choice, setting.current) ? null : choice }
}

/** A value's name, as the agent calls it where it can: "Sonnet", "On". */
export function valueName(setting: DeepReadonly<Setting>, value: DeepReadonly<SettingValue>): string {
  switch (value.type) {
    case 'Choice': {
      const id = value.value
      const choices = setting.form.type === 'Choices' ? setting.form.value : []
      return choices.find((choice) => choice.id === id)?.name ?? id
    }
    case 'Switch':
      return value.value ? 'On' : 'Off'
    default:
      return `a value this viewer doesn't know (${tagOf(value)})`
  }
}
