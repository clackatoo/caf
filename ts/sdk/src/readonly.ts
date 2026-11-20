/**
 * `T`, read-only all the way down: a chat a watch draws, or a copy's
 * chat, shares its turns and items with the SDK's own copy, so changing
 * any part of it would change the copy too.
 */
export type DeepReadonly<T> = T extends (...args: never[]) => unknown
  ? T
  : T extends readonly (infer U)[]
    ? readonly DeepReadonly<U>[]
    : T extends object
      ? { readonly [K in keyof T]: DeepReadonly<T[K]> }
      : T;
