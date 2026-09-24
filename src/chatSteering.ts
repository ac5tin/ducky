// Pure steering-queue logic. No React, no Tauri imports.
import type { SteeringMessage } from "./types";

export type SteeringQueues = Record<string, SteeringMessage[]>;

/** A copy of `queues` with `message` appended for `conversationId`. */
export function withSteeringAdded(
  queues: SteeringQueues,
  conversationId: string,
  message: SteeringMessage,
): SteeringQueues {
  return {
    ...queues,
    [conversationId]: [...(queues[conversationId] ?? []), message],
  };
}

/** The first pending steer whose `chat_steer` invoke has settled. A steer
 *  still in flight must not be sent as a normal turn: the backend may accept
 *  it and inject the same text twice. */
export function nextFlushableSteer(
  queue: SteeringMessage[],
  inFlight: ReadonlySet<string>,
): SteeringMessage | undefined {
  return queue.find((m) => !inFlight.has(m.id));
}

/** A copy of `queues` with the message whose `id` matches removed for
 *  `conversationId`. Other conversations and other messages are untouched;
 *  removing an id that is not queued changes nothing. */
export function withSteeringRemoved(
  queues: SteeringQueues,
  conversationId: string,
  id: string,
): SteeringQueues {
  return {
    ...queues,
    [conversationId]: (queues[conversationId] ?? []).filter((m) => m.id !== id),
  };
}
