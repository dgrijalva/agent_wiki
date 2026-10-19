---
name: define
description: The default way of working on agent_wiki's design. Use for any design work — discussing what agent_wiki should do, making or recording decisions, updating the design doc. Covers where changes go, how to mark what isn't decided, and how to raise conflicts.
---

# Define

**discuss → decide → capture → apply.** Read `docs/process.md` first.

## Capturing a decision

1. **Find where the subject lives** in `docs/design/agent_wiki.md`. Look for an existing `[OPEN: …]` or `[PROVISIONAL: …]` it settles (`cargo run -q -p pending -- docs --section "<heading words>"`).
2. **Merge it into that place.** Resolve the marker, or narrow it to what remains. Don't append a change note; git holds history.
3. **Use Dave's words.** Change only what the decision covers. Don't add detail he didn't give.
4. **Check the rest of the doc for contradictions.**

## What stays out of the design doc

Content no person decided: your extrapolation, consequences you worked out, suggested requirements. Raise it in conversation, or record it in the plan/journal, or mark it `[PROVISIONAL]`.

## Conflicts

If a change would contradict something decided, surface it and let Dave decide. Say what the constraint is, what you'd change and why, and wait.
