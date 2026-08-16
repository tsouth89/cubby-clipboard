// Shared wording guard for the public product pages.
//
// Cubby stopped storing copied files as history in v1.2.4, so the pages must
// not advertise file-list history again. The guard has to tell a claim apart
// from a disclaimer: "Cubby does not store file lists" is a sentence we *want*
// to be free to write, so a bare keyword match would fail the release for
// saying the true thing.

/** Matches "file list", "file-list", "file lists", "file-lists". */
const FILE_LIST_MENTION = /file[\s-]*lists?/gi;

/** Verbs that assert support for whatever noun follows them. */
const CLAIM_VERB =
  /\b(?:supports?|stores?|records?|keeps?|saves?|captures?|includes?|holds?|remembers?|restores?|backs? up)\b/gi;

/** Words that turn the verb they precede, or the whole sentence, negative. */
const NEGATION =
  /\b(?:no|not|never|without|cannot|can[’']t|does\s+not|doesn[’']t|do\s+not|don[’']t|did\s+not|didn[’']t|is\s+not|isn[’']t|are\s+not|aren[’']t|was\s+not|wasn[’']t|were\s+not|weren[’']t|stopped|stops|dropped|drops|removed|removes|ignored|ignores|excluded|excludes)\b/gi;

/**
 * Split a page into sentence-sized pieces. HTML tags are boundaries too, so two
 * unrelated list items or paragraphs never share a piece.
 */
function segments(source) {
  return source
    .replace(/<[^>]*>/g, '\n')
    .split(/\n+|(?<=[.!?;:])\s+/)
    .map((piece) => piece.trim())
    .filter(Boolean);
}

/** Index where the last match of `pattern` starts, or -1. */
function lastMatchIndex(pattern, text) {
  pattern.lastIndex = 0;
  let index = -1;
  let match;
  while ((match = pattern.exec(text)) !== null) {
    index = match.index;
  }
  return index;
}

/**
 * Whether one mention of file lists reads as a claim of support.
 *
 * A claim verb in front of the mention decides it: "supports ... file lists" is
 * a claim, and stays one when the sentence negates something else later ("but
 * not folders"). A negation in front of that verb makes it a disclaimer. With
 * no claim verb in front, any negation in the sentence is taken as the
 * disclaimer ("File lists are no longer stored").
 */
function isClaim(segment, mentionIndex) {
  const before = segment.slice(0, mentionIndex);
  const verb = lastMatchIndex(CLAIM_VERB, before);
  if (verb !== -1) {
    return lastMatchIndex(NEGATION, before.slice(0, verb)) === -1;
  }
  return lastMatchIndex(NEGATION, segment) === -1;
}

/**
 * Sentences that claim Cubby keeps file lists in clipboard history.
 *
 * Empty for a page that only disclaims the support.
 */
export function findFileListHistoryClaims(source) {
  const claims = [];
  for (const segment of segments(source)) {
    FILE_LIST_MENTION.lastIndex = 0;
    let match;
    while ((match = FILE_LIST_MENTION.exec(segment)) !== null) {
      if (isClaim(segment, match.index)) {
        claims.push(segment);
        break;
      }
    }
  }
  return claims;
}
