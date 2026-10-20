// The host's limits on a question's answers (core/src/session/mod.rs). Over any of them
// it refuses the whole answer with "answers too large", so the Send is refused here first.
export const MAX_ANSWERS = 32;
export const MAX_ANSWER_BYTES = 4096;

const utf8 = (s: string) => new TextEncoder().encode(s).length;

export function answersTooLarge(answers: Record<string, string>): boolean {
  const entries = Object.entries(answers);
  if (entries.length > MAX_ANSWERS) return true;
  return entries.some(([k, v]) => utf8(k) > MAX_ANSWER_BYTES || utf8(v) > MAX_ANSWER_BYTES);
}
