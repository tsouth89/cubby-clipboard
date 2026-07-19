# Search architecture

Cubby keeps clipboard content encrypted at rest. Search therefore cannot use a
normal persistent SQLite FTS table without writing a second plaintext copy of
clipboard and OCR text to disk.

The search index is built in memory after encrypted storage migration completes:

- text, file-list text, previews, and OCR text are decrypted into process memory;
- case-normalized trigrams map to shared in-memory clip identifiers;
- exact substring verification removes trigram false positives;
- SQLite remains authoritative for deletion state, folder filters, pin order,
  timestamps, and pagination;
- only the final result page loads and decrypts full clip rows.

Capture, OCR completion, and deletion update the live index. Bulk operations and
imports invalidate it and trigger a generation-safe rebuild. A mutation that
races a rebuild changes the generation, so stale build output is discarded.

The index has no database table, cache file, or serialization path. It disappears
when Cubby exits; only the existing encrypted clip fields remain on disk.
