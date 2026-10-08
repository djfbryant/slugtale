# Ask Before Launch at Login

Slugtale will ask during onboarding whether it should launch at login, rather than enabling startup automatically. This keeps resident background behavior under user control while making the option easy to discover.

## Readiness decision (2026-10-08, slugtale-9bx)

Launch at Login is informational and optional. The readiness report lists it so Settings can point at the row, but the item is never required and never reported unready: choosing not to start Slugtale at sign-in is a valid preference, not a missing step, and a disabled preference must never block dictation. The stored preference is preserved and reconciled with the OS login item on startup.
