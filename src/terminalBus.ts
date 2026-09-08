// Terminal output arrives at keystroke/line rate; routing it through zustand
// state would re-render the whole chat per chunk. Panels subscribe here
// instead, and store.ts forwards the relevant backend events.
import type { BackendEvent } from "./types";

export type TerminalEvent = Extract<
  BackendEvent,
  { type: "terminal_output" } | { type: "terminal_closed" }
>;

type Listener = (event: TerminalEvent) => void;

const listeners = new Set<Listener>();

export function subscribeTerminal(listener: Listener): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function dispatchTerminalEvent(event: TerminalEvent) {
  for (const listener of listeners) listener(event);
}
