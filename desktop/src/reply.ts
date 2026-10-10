import type { Answer } from "./types";

// Clear the reply box only once the send succeeded, so a failed send keeps the typed text.
export async function replyThenClear(
  send: (a: Answer) => Promise<boolean>,
  answer: Answer,
  clear: () => void,
): Promise<void> {
  if (await send(answer)) clear();
}
