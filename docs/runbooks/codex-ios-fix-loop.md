# iPhone fix loop for a local agent (Codex)

How an agent running on the owner's Mac finds a visual or interaction bug in the iPhone app,
fixes it and opens a PR. The iPhone twin of `codex-mac-fix-loop.md`: same rules, one bug per PR.

## 0. Rules

1. **Synthetic data only.** A dedicated simulator (below), the app's preview data or a local test
   server, throwaway `@example.com` accounts. Never the owner's real account or a real device.
2. **Never run `/verify-live`** and make no paid provider calls. The test agents are local and free.
3. **The board decides how it looks.** Compare against `docs/design/board/ios/` (index in its
   `README.md`). Use only existing design tokens and `BrainBuddyDesign` components; never type a
   hex, size or radius into a screen and never invent a colour, word or behaviour the board and the
   spec do not show. If the board and the spec disagree, or a fix would need a new behaviour, stop
   and write it in the PR as a question instead of choosing.
4. **A screen that is not built yet is not a bug.** If the app has no counterpart for a board cell
   (for example an agents screen still in progress), list it in your report and move on; do not
   build features in this loop.
5. **Do not touch** `.github/workflows/**`, `fly*.toml`, deploy files, `.claude/settings.json`, the
   `Makefile`, anything listed in `GUARDED_FILES` in `scripts/check_gate_integrity.py`, or any
   GitHub repository setting. Those need the owner.
6. **Content-free reports:** no tokens, keychain output, home paths or real emails in a PR or log.

## 1. Run the app

Toolchain and build are in `ios/README.md` (Xcode 26, XcodeGen, `xcodegen generate`).

1. Make a simulator the size of the board (393×852 pt, the iPhone 16 / 17-class frame) so the
   screenshots line up with the PNGs:

   ```sh
   RUNTIME=$(xcrun simctl list runtimes | awk '/iOS 26/ {print $NF; exit}')
   xcrun simctl create "Carry On Board" "iPhone 16" "$RUNTIME"
   xcrun simctl boot "Carry On Board" && open -a Simulator
   ```

2. Build and install:

   ```sh
   cd ios && xcodegen generate
   xcodebuild -project BrainBuddy.xcodeproj -scheme BrainBuddy \
     -destination 'platform=iOS Simulator,name=Carry On Board' \
     -derivedDataPath build CODE_SIGNING_ALLOWED=NO build
   xcrun simctl install "Carry On Board" "$(find build/Build/Products -name 'BrainBuddy.app' -path '*iphonesimulator*' | head -1)"
   ```

3. Launch it in one of three states (bundle id: `xcrun simctl listapps "Carry On Board" | grep -i brainbuddy`):
   - **First run (M01–M04):** `xcrun simctl uninstall` + `install` again for a clean install,
     then launch normally.
   - **Lists, task card, capture (M07–M20):** launch with sample data:
     `xcrun simctl launch "Carry On Board" <bundle id> -BBUsePreviewData`.
   - **Account and agents (M05–M06, M21–M30):** start the local test server, accounts, the
     `external_agent_relay` flag and the two test agents exactly as in
     `manual-mac-tests-028-029-030.md` §§ 1.3–1.5. In the iPhone app open Sign in, set the
     server address to `http://localhost:8000/api` (the simulator shares the Mac's localhost;
     the `/api` suffix is required) and sign in with Account A.

Dark mode: `xcrun simctl ui "Carry On Board" appearance dark` (and `light`). Bigger text for a
Dynamic Type check: `xcrun simctl ui "Carry On Board" content_size extra-extra-large`.

## 2. Find a bug

Work screen by screen through the board index. For each screen:

1. Bring the app to the same state as the board cell (same list, same selected step, same sheet).
   Use the board's sample words where the app lets you type them.
2. Screenshot: `xcrun simctl io "Carry On Board" screenshot shot.png`. Take it in the appearance
   the board cell uses (M09 and M20 dark, the rest light), then the other appearance as well:
   the board shows one, the app must work in both.
3. Compare with the board PNG for layout, spacing, type, colour role, words and what is shown or
   hidden. Data differences (counts, dates, names) are not bugs. Glass is system material: judge
   its place, size, shape and contents, not the blur (see the board README).
4. Then check interaction on that screen: what each button and swipe does, sheet detents and
   dismissal, keyboard (Return and the toolbar above it), VoiceOver labels on icon-only buttons.

Write each finding down before fixing anything: screen number, what the board shows, what the app
shows, both images.

## 3. Fix it

- Find the view: the iPhone app is `ios/BrainBuddy/Screens/<Area>/`; shared models and design
  components are in `ios/BrainBuddyKit/Sources/` (`BrainBuddyDesign` for components and tokens).
  New files need `xcodegen generate`.
- Keep the fix minimal: what the finding needs, no refactor around it.
- Add or update a test that would have caught it:
  - **A design component's look:** a snapshot in `CarryOnCatalogSnapshotTests.swift`
    (`assertSnapshot`), light and dark. Record with
    `CARRY_ON_SNAPSHOT_RECORD=1 swift test --filter CarryOnCatalogSnapshotTests` in
    `ios/BrainBuddyKit`, look at the new PNG in `__Snapshots__/`, then run again without the
    variable. Liquid Glass renders empty in these snapshots; keep glass out of them.
  - **Behaviour:** a Swift Testing test on the model in the kit. Logic goes in the model, not
    only in the view.
  - Name the test after the requirement it covers with the feature-qualified id, e.g.
    `` @Test("032-FR-004 Skip leaves an empty Inbox") ``. A bare `FR-004` is rejected.
- Run `cd ios/BrainBuddyKit && swift test` and the app build from § 1 until green.
- Reinstall the app and repeat the check from § 2 to see the fix in the running app.

## 4. Open the PR

- One finding per branch and PR, branch `codex/ios-<short-name>` from the latest `origin/main`.
- Conventional commit: `fix(ios): <what the user sees now>`.
- PR body:
  - **Before / After:** one sentence each, with the before screenshot, the after screenshot and
    the board PNG path (e.g. `docs/design/board/ios/M08-iPhone.png`).
  - **Test:** the test name and the `swift test` summary line.
  - **Questions:** anything from rule 3 you did not decide.
- Open it as a draft. Claude reviews it against the board and drives it to merge; push follow-up
  fixes to the same branch when asked.
