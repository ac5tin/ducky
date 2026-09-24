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

/** The pending steer that may be sent as a normal turn, or undefined when none
 *  may. A steer whose `chat_steer` invoke has not settled must not be sent: the
 *  backend may still accept it and inject the same text twice. A later steer
 *  must not jump ahead of one still in flight, or that late `Ok` lands in the
 *  wrong turn. */
export function nextFlushableSteer(
  queue: SteeringMessage[],
  inFlight: ReadonlySet<string>,
): SteeringMessage | undefined {
  const head = queue[0];
  if (!head || inFlight.has(head.id)) return undefined;
  return head;
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
