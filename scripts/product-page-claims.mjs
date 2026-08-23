// Shared wording guard for public docs (SBS-780, SBS-832).
//
// Cubby stopped storing copied files as history in v1.2.4, so README,
// SECURITY.md, and the product pages must not advertise file-list history
// again. The guard has to tell a claim apart from a disclaimer: "Cubby does
// not store file lists" is a sentence we *want* to be free to write, so a
// bare keyword match would fail the release for saying the true thing.
//
// The original README lie was a table cell with no verb ("...and file lists").
// The original SECURITY.md lie used "file-drop lists" / "retained". Both
// have to count as mentions.

/**
 * Matches "file list(s)", "file-list(s)", "file-drop list(s)", and "copied
 * file(s)".
 *
 * "list" is required after "drop" on purpose. A bare "file drop" mention was
 * both too wide and self-cancelling: `NEGATION` contains `\bdrops\b`, so a
 * no-verb cell such as "Text, HTML, RTF, images, and file drops" negated
 * itself and reported nothing, while "Windows file-drop (CF_HDROP) is a path
 * list" was reported as a claim.
 */
const FILE_LIST_MENTION =
  /\b(?:file[\s-]*lists?|file[\s-]*drop[\s-]*lists?|copied files?)\b/gi;

/** Verbs that assert support for whatever noun follows them. */
const CLAIM_VERB =
  /\b(?:supports?|stores?|records?|keeps?|saves?|captures?|includes?|holds?|remembers?|restores?|backs? up)\b/gi;

/** Words that turn the verb they precede, or the whole sentence, negative. */
const NEGATION =
  /\b(?:no|not|never|without|except|cannot|can[’']t|does\s+not|doesn[’']t|do\s+not|don[’']t|did\s+not|didn[’']t|is\s+not|isn[’']t|are\s+not|aren[’']t|was\s+not|wasn[’']t|were\s+not|weren[’']t|stopped|stops|dropped|drops|removed|removes|ignored|ignores|ignoring|excluded|excludes|excluding)\b/gi;

const CLAUSE_BOUNDARY = /,|\b(?:but|and)\b/gi;

/**
 * Split a page into sentence-sized pieces. Block tags are boundaries; inline
 * tags become spaces so "file <strong>lists</strong>" stays one mention.
 *
 * Exported because the weaker `\bfiles?\b` scan in check-release.mjs needs
 * the same boundaries: splitting on `[.!?]` alone does not break at `.</p>`
 * or `disk."`, which joined a whole install-steps section of start.html into
 * one span and made an unrelated sentence in it fail the release.
 */
export function claimSegments(source) {
  return segments(source);
}

function segments(source) {
  const decoded = source
    .replace(/&nbsp;|&#160;|&#x0*A0;/gi, ' ')
    .replace(/<(p|div|li|br|h[1-6]|ul|ol|tr|td|section|article)\b[^>]*>/gi, '\n')
    .replace(/<\/(p|div|li|h[1-6]|ul|ol|tr|td|section|article)>/gi, '\n')
    .replace(/<[^>]+>/g, ' ');
  return decoded
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

function lastClauseStart(text) {
  CLAUSE_BOUNDARY.lastIndex = 0;
  let end = 0;
  let match;
  while ((match = CLAUSE_BOUNDARY.exec(text)) !== null) {
    end = match.index + match[0].length;
  }
  return end;
}

/**
 * Whether one mention of file lists reads as a claim of support.
 *
 * A claim verb in the same clause as the mention decides it. A negation in
 * an earlier "and"/"but" clause does not excuse a later claim, and a
 * negation between the verb and the mention ("stores text without file
 * lists") is a disclaimer.
 */
function isClaim(segment, mentionIndex) {
  const before = segment.slice(0, mentionIndex);
  const verb = lastMatchIndex(CLAIM_VERB, before);
  if (verb !== -1) {
    const clauseStart = lastClauseStart(before.slice(0, verb));
    if (lastMatchIndex(NEGATION, before.slice(clauseStart, verb)) !== -1) {
      return false;
    }
    if (lastMatchIndex(NEGATION, before.slice(verb)) !== -1) {
      return false;
    }
    return true;
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

/**
 * SBS-1028: encrypted backups include HTML/RTF (`attach_export_formats`) and
 * live full-resolution originals (`attach_export_full_image`). A sentence that
 * says the archive omits those, or that they do not come back on import, is
 * the privacy.html lie this helper exists to keep off the public pages.
 */
const OMIT_HOLD =
  /\b(?:(?:does\s+not|doesn[’']t|do\s+not|don[’']t)\s+(?:hold|include|contain|store|keep)|omits?|omitted)\b/i;
const DO_NOT_COME_BACK =
  /\b(?:do\s+not|does\s+not|don[’']t|doesn[’']t|never)\s+come\s+back\b/i;
const HTML_AND_RTF = /\bHTML\b/i;
const RTF = /\bRTF\b/i;
const FULL_RESOLUTION = /\bfull[\s-]*resolution\b/i;
/** Current or previous sentence names the archive so anaphoric "It" still counts. */
const BACKUP_CONTEXT =
  /\b(?:archives?|backups?|import(?:s|ed|ing)?|export(?:s|ed|ing)?)\b/i;

export function findStaleBackupOmissionClaims(source) {
  const claims = [];
  const pieces = segments(source);
  for (let i = 0; i < pieces.length; i++) {
    const segment = pieces[i];
    const mentionsFormats = HTML_AND_RTF.test(segment) && RTF.test(segment);
    const mentionsFullRes = FULL_RESOLUTION.test(segment);
    if (!mentionsFormats && !mentionsFullRes) {
      continue;
    }

    const previous = i > 0 ? pieces[i - 1] : '';
    if (!BACKUP_CONTEXT.test(segment) && !BACKUP_CONTEXT.test(previous)) {
      continue;
    }

    const omit = segment.match(OMIT_HOLD);
    if (omit) {
      const before = segment.slice(0, omit.index);
      const negatedOmit =
        /^omit(?:s|ted)?$/i.test(omit[0]) &&
        lastMatchIndex(NEGATION, before.slice(lastClauseStart(before))) !== -1;
      if (!negatedOmit) {
        const after = segment.slice(omit.index + omit[0].length);
        if (
          (mentionsFormats && HTML_AND_RTF.test(after) && RTF.test(after)) ||
          (mentionsFullRes && FULL_RESOLUTION.test(after))
        ) {
          claims.push(segment);
          continue;
        }
      }
    }

    if (DO_NOT_COME_BACK.test(segment)) {
      claims.push(segment);
    }
  }
  return claims;
}

/**
 * Weaker scan used by the release check: a sentence that says files are
 * retained or supported without also negating that. Broader than
 * `findFileListHistoryClaims` so "Cubby stores files" still fails even when
 * it never says "file lists". `no`/`without`/`none`/`neither` have to count
 * as negation or a disclaimer such as "No files are retained" false-fails.
 */
export function findWeakerFileRetentionClaim(source) {
  const fileMentionPattern = /\bfiles?\b/i;
  const retentionClaimPattern =
    /\b(?:retained|stores?|supports?|includes?|records?(?:ed)?)\b/i;
  const negationPattern = /\b(?:no|not|never|without|none|neither|ignor\w*)\b/i;
  return claimSegments(source).find(
    (sentence) =>
      fileMentionPattern.test(sentence) &&
      retentionClaimPattern.test(sentence) &&
      !negationPattern.test(sentence)
  );
}
