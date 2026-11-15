## Anti-slop (always on)

Load and follow these skills on every task that writes code, comments, UI or prose. They are part of the definition of done, not optional polish.

- **antislop** (core filter): load first for any code, UI or copy change. No generic filler, no invented requirements, no decorative output.
- **antislop-code** (comments): every comment must say something the code does not already show. One or two lines, why not what. No banner separators, ALL CAPS headers, step narration, empty labels, vague TODOs, decorative emoji, or end markers. Never change code, identifiers or formatting while editing comments.
- **antislop-human** (accessibility): for any HTML, CSS or user-facing output, verify contrast with the skill's contrast checker (4.5:1 body, 3:1 large text and UI edges), visible keyboard focus, 44px tap targets, no color-only status, and reduced-motion respected. Never claim a pairing passes without computing it.
- **antislop-ui** and **antislop-layoutmobile** when building or changing an interface: tokens over ad-hoc values, real hover/active/disabled states, layouts that reflow from phone to desktop without horizontal scroll.

If a skill is unavailable in a session, apply its rules from memory and say so in the final report. Before closing any task that touched code, comments, docs or UI, run the matching skill checklist and report the result.
