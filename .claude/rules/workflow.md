# Workflow Rules

## Plan Mode
- Enter plan mode for non-trivial tasks (3+ steps or architectural decisions)
- If something goes sideways, STOP and re-plan immediately
- Write detailed specs upfront to reduce ambiguity

## Subagent Strategy
- Use subagents liberally to keep main context window clean
- Offload research, exploration, and parallel analysis to subagents
- One task per subagent for focused execution

## Self-Improvement
- After ANY correction from the user: update `tasks/lessons.md` with the pattern
- Write rules for yourself that prevent the same mistake
- Review lessons at session start

## Verification
- Never mark a task complete without proving it works
- Diff behavior between main and your changes when relevant
- Ask yourself: "Would a staff engineer approve this?"
- Run tests, check logs, demonstrate correctness

## Autonomous Bug Fixing
- When given a bug report: just fix it. Don't ask for hand-holding.
- Point at logs, errors, failing tests — then resolve them.
- Zero context switching required from the user.

## Task Management
1. Plan first: write plan to `tasks/todo.md` with checkable items
2. Track progress: mark items complete as you go
3. Explain changes: high-level summary at each step
4. Document results: add review section to `tasks/todo.md`
5. Capture lessons: update `tasks/lessons.md` after corrections

## A/B Testing Strategy
- FastLED can switch between `--platformio` and fbuild
- When debugging serial behavior: compare Python fbuild vs Rust fbuild vs PlatformIO
- The Python integration tests are the source of truth
- If behavior diverges: study the Python implementation, not guesses
