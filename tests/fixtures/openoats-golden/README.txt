Synthetic OpenOats data (no meeting data), shaped as OpenOats writes it at yazinsai/OpenOats
b4353cd71ca9677f330e74751ffb669172a0b575:

- session.json: SessionRepository.writeSessionMetadata (JSONEncoder .iso8601, prettyPrinted, sortedKeys)
- transcript.live.jsonl: SessionRecord lines (JSONEncoder .iso8601; CodingKeys order; nil keys omitted)
- transcript.live.jsonl.pre-cleanup.bak: rewriteJSONLWithCleanedText's backup (before refinedText)
- notes.md / notes.meta.json: saveNotes (NotesMeta: templateSnapshot, generatedAt)
- attachments/: importAttachment's folder
- expected-export.md: MarkdownMeetingWriter.buildMarkdown of this session with TimeZone
  America/New_York and NSFullUserName "Test Recorder" (2026-11-02 is EST, -05:00)
- templates.json: TemplateStore.save (StorageFormat {version, templates:[MeetingTemplate]})

crates/core/tests/openoats.rs checks oats-rs reads all of it, writes it back unchanged and
produces expected-export.md byte for byte.
