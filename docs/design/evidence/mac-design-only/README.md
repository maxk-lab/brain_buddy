# Mac design-only integration evidence

## Accepted outcome

Align existing Mac screens with the authoritative M01–M30 PNG boards from PR #521,
including both appearances, sidebar/inspector/capture geometry, settings/agent
presentation, bundled fonts, and the whole-window dimming defect when pressing Done.
This is a SHOW correction. No new product capability is included.

## Excluded scope

Live inferred voice steps, staged AI clarification, Quick Capture voice, new
Someday/Waiting workflows, metadata editing in New step, separate password-proof
stages, authentication/persistence/backend/API changes, and the plans/ADR for those
experiments are excluded. Their history remains on
`codex/mac-board-palette-20261017`; it is not imported into this PR.

Existing mandatory credential/password/consent requirements remain visible even
where the reference omits them. Search and auxiliary commands remain reachable
through the existing menus. Rendered PNGs take priority over older prose.

## Native acceptance evidence and provenance

- The final all-screen replay used `a54f915c2e905ff7e614c2d245b20d6e8dfdddfa`:
  23 reachable targets, each newly captured in Light and Dark (46 JPEG captures),
  17 October 2026 18:19–18:27 UTC, one signed binary across two synthetic profiles.
  `capture-manifest.json` records each source SHA, executable hash, image hash,
  dimensions and timestamp; representative original JPEGs are checked in here.
- Unavailable: M05 sign-in-provider/code flow, M06 Invite, M11 live voice steps,
  M17 staged AI, M26 sign-in methods, M27 change email, M28 delete account.
  No neighboring screen is claimed as target evidence.
- `metrics.json` retains all 23 unmasked mechanical comparisons. Explicit native
  2x surface registration, ICC normalization, RGB channel threshold >16, no resize,
  warp, local fit, ignored content or automatic visual pass. M09/M20 have Dark
  references; all other references are Light. Opposite-theme parity is unapproved.
- Remaining differences are explicit: native font/control/titlebar rendering,
  actual fixture counts/dates/agent text, Quick Capture desktop material,
  mandatory-security modal height (M21 +358px, M30 +160px), existing unsupported
  functions and rich editable token styling. This is not exact pixel parity.
- The final Done correction at `b8af493d9664d7150c9181e552d5b669f10eaa41`
  removes root model-save disabling while preserving the inspector edit lock,
  navigation guard and serialized durable writes. Manual Done changed Next 1→0;
  Edit→Undo restored 1. `done-after.jpg` is a fresh capture of that binary.
- 476 Swift Testing tests /57 suites and 8 XCTest tests passed on the Done build.
  The earlier broad local verification stopped before backend tests because its
  environment lacked pytest-xdist. Full CI must pass for this PR head.
- The integration branch uses the same product source tree as the Done build;
  historical planning documents are removed from scope and the design text is
  corrected to describe the retained single-stage password form.

## Limits

The 46-screen replay precedes the bounded Done correction; it is not relabeled as
an all-screen replay of the integration SHA. CI, merge queue and production smoke
must supply their own current exact-SHA results. No production completion is claimed.
Spoken VoiceOver, global shortcut registration and exhaustive keyboard acceptance
are not certified by the visual replay. System appearance was restored to Auto.

## Representative captures

| Surface | Evidence |
| --- | --- |
| Welcome | [Light](M01-light.jpg) |
| Project | [Dark](M09-dark.jpg) |
| Quick Capture | [Light](M12-light.jpg), [Dark](M12-dark.jpg) |
| Hand to an agent | [Light](M21-light.jpg), [Dark](M21-dark.jpg) |
| Agent settings | [Light](M29-light.jpg) |
| Add agent | [Light](M30-light.jpg) |
| Done result | [Dark](done-after.jpg) |
