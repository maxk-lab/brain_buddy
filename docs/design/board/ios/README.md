# Carry On board: iPhone screens

PNG renders of the iPhone cells of the Carry On design board, so an agent that cannot open
claude.ai can compare the running app with the design. The board itself stays the source of
truth (https://claude.ai/artifact/1bwKiw15U2jpZqwu5pTMg6); these files are a snapshot of it.

- **Board version:** `1792165683-83f3`, rendered 2026-10-17 at 2x in Chromium with the
  board's own fonts (Instrument Sans / Instrument Serif). Colours are quantized to 256 per
  image to keep the files small, so read exact colour values from the token pages, not by
  sampling a screen render.
- Each screen is a 393×852 pt iPhone (786×1704 px in the PNG) with the number, title and a
  one-line caption. M09 and M20 are drawn dark; every other cell is light.
- The board draws Liquid Glass (tab bar, capture bar, floating action bars, sheets) with CSS
  blur. In the app these are the system glass materials; compare their position, size, shape
  and contents, not the exact blur or tint.
- If the board changes, ask for a re-render; do not edit these files by hand.

## Screens

| File | Screen | Area |
|---|---|---|
| `M01-iPhone.png` | 01 Welcome | first run |
| `M02-iPhone.png` | 02 First capture | first run |
| `M03-iPhone.png` | 03 First step | first run |
| `M04-iPhone.png` | 04 Landing | first run |
| `M05-iPhone.png` | 05 Sign in | account |
| `M06-iPhone.png` | 06 Invite | account |
| `M07-iPhone.png` | 07 Inbox | core |
| `M08-iPhone.png` | 08 Next actions | core |
| `M09-iPhone.png` | 09 Project (dark) | core |
| `M10-iPhone.png` | 10 Create task | core |
| `M11-iPhone.png` | 11 Voice capture | core |
| `M12-iPhone.png` | 12 Quick capture (widget and Control) | core |
| `M13-iPhone.png` | 13 Next action (task card) | core |
| `M14-iPhone.png` | 14 Date picker | core |
| `M15-iPhone.png` | 15 Not now | core |
| `M16-iPhone.png` | 16 Inbox item | core |
| `M17-iPhone.png` | 17 Clarify with AI | core |
| `M18-iPhone.png` | 18 Waiting | core |
| `M19-iPhone.png` | 19 Someday | core |
| `M20-iPhone.png` | 20 Done (dark) | core |
| `M21-iPhone.png` | 21 Hand off | agents |
| `M22-iPhone.png` | 22 Agent runs | agents |
| `M23-iPhone.png` | 23 Needs you | agents |
| `M24-iPhone.png` | 24 Result | agents |
| `M25-iPhone.png` | 25 Account | settings |
| `M26-iPhone.png` | 26 Sign-in methods | settings |
| `M27-iPhone.png` | 27 Change email | settings |
| `M28-iPhone.png` | 28 Delete account | settings |
| `M29-iPhone.png` | 29 Agent details | agents |
| `M30-iPhone.png` | 30 Add agent | agents |

## Design-system pages

| File | Page |
|---|---|
| `DSiPhone.png` | iPhone specifics: two layers (flat content, glass chrome), tab bar, capture bar |
| `../mac/DSFoundations.png` | Foundations: colour tokens (light and dark), type, spacing |
| `../mac/DSComponents.png` | Components |
| `../mac/DSCard.png` | The task card |
| `../mac/DSOnboard.png` | First run |
| `../mac/DSAccount.png` | Account and agents in Settings |
| `../mac/DSAgents.png` | Agents: hand-off, runs, states |

The shared design-system pages are the same for every platform and live next to the Mac renders.
