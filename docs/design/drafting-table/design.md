# Oats Meetings: UI design

## Direction
"Drafting table." The icon's blueprint blue and Plex Mono become the app: an ink-blue dark theme (and a cool paper light theme) with a faint 24px blueprint grid behind content. One bold element: recording state. When recording, the window gets a 3px red top edge, the Live strip turns red-tinted with a pulsing lamp and a 38px mono clock, and a red dot sits on the Live tab. Everything else stays quiet. Transcript text is 17px IBM Plex Sans with a left-colored speaker tag in Plex Mono, so who is talking reads before the words do.

Departures from the current Catppuccin look: tabs move to a left rail (7 destinations, grouped), buttons become real hierarchy (one primary per screen), and color carries meaning only (accent = selection/primary, red = recording/destructive, amber = warning/paused, green = ok, speaker hues).

Files: `mockup.html` (open with `?state=rec|paused|idle&theme=dark|light#live|sessions|notes|templates|calendar|models|settings`), PNGs `*-dark.png` / `*-light.png` rendered with headless Chrome at 1100x720.

## Tokens
Fonts: `IBM Plex Sans` (UI, transcript, notes) and `IBM Plex Mono` (brand, clock, timestamps, speaker ids, numbers, code). Fallbacks: system UI / Menlo. Bundle both in the app (assets), do not rely on installed fonts.

| Token | Dark | Light | Use |
|---|---|---|---|
| bg | #0E1A2B | #F4F7FB | window content |
| side | #0A1423 | #E8EEF6 | rail, title, status bar |
| surface | #14233A | #FFFFFF | cards, lists, strip |
| raised | #1B2E4A | #EAF0F8 | hover, table head, tag bg |
| line | #243A59 | #D2DCE9 | dividers, card borders |
| line2 | #2F4B72 | #B8C6D9 | control borders, toggle off |
| text | #E8EFF8 | #12233B | primary |
| dim | #93A6BF | #52657F | secondary |
| faint | #62789A | #8393A9 | timestamps, headers (large/meta only) |
| accent | #5C9DF5 | #1E4D8C | primary button, selection, focus |
| accent-ink | #06142A | #FFFFFF | text on accent |
| accent-soft | #16305A | #DCE8F8 | selected row/tab bg |
| rec | #FF5A52 | #D12B2B | recording, Stop/Start button |
| rec-soft | #3A1A24 | #FBE3E1 | recording strip, error banner bg |
| pause | #F2B134 | #A56A00 | paused lamp |
| ok | #4CC38A | #14804F | meters, "In use", saved |
| warn / warn-soft | #F2B134 / #33290F | #8A5A00 / #FBF0D3 | warning banner |
| err | #FF7A72 | #B3261E | error text, destructive |
| speaker you | #78AEFF | #1E5BB8 | |
| speaker remote_1 | #F2B866 | #A15C00 | |
| speaker remote_2 | #5ED3B9 | #0B7F69 | |
| speaker remote_3 | #C79BFF | #6B3FB5 | remote_4+ cycle from remote_1 |
| grid | rgba(92,157,245,.07) | rgba(30,77,140,.06) | background lines |

Light theme is justified: meetings happen in bright rooms and David may run macOS light. Follow system appearance by default (Settings > Appearance).

Type scale (px): 38 mono clock / 22 note title / 20 page title / 17 transcript / 15 row title, note body / 14 body / 13 controls, menu / 12 meta, tags, status / 11 timestamps, table head. Weights 400, 500 (controls), 600 (titles, speaker). Transcript line-height 1.5, max width 68ch. Sentence case everywhere, no all-caps labels.

Spacing (4px base): 4, 8, 12, 16, 20, 28 (page gutter), 22 (page bottom). Rail 176 wide. Title bar 38. Status bar 32. Row heights: list 60, model 64, settings 52, control 32, big control 44.
Radii: 6 (icon buttons, menu items), 7 (controls, tabs), 9 (banner, big buttons), 10 (cards, lists), 12 (record strip), pill 10 (tags), full (lamp, radio).
Shadow: only on dropdown menu (0 12 32 rgba(0,0,0,.35)). Focus ring: 3px accent-soft outside a 1px accent border.
Motion: only the recording lamp pulse (1.6s, skip if reduced motion) and toggle thumb 120ms.

## Components
- Tab rail: 34px rows, 10px h-padding, 16px stroke icon (1.6) + 14/500 label, dim; hover raised; active = accent-soft bg, text color, 3px accent bar on the left edge. A 1px divider separates daily tabs (Live, Sessions, Notes) from setup tabs (Templates, Calendar, Models, Settings). Live tab shows a 8px rec dot while recording or paused (amber when paused).
- Record control (Live strip): 12px radius surface, 14/18 padding, row: lamp + state word (Recording / Paused / Not recording), mono clock, two level meters (You, Remote; 5px bars, ok green), controls. Idle: lamp faint, clock faint, one "Start recording" button (rec fill, white dot). Recording: strip rec-soft with rec-tinted border, buttons "Pause" (neutral) and "Stop" (rec fill, square glyph). Paused: amber lamp, "Resume" replaces "Pause". Pending: label "Starting…" / "Stopping…", buttons disabled at 40%.
- Transcript line: grid 48 (mono 11 faint timestamp) / 86 (speaker tag: 3x14 colored bar + mono 12/600 name in speaker color) / 1fr (17px text). Consecutive lines by the same speaker hide the timestamp and tag (still occupy the column). Live partial line: surface bg, dim text, accent caret. Auto-scroll to bottom unless the user scrolled up; then show a "Jump to live" button.
- List row: 60px, hairline bottom, name 14/600 over a 12 dim one-line snippet (ellipsis), tags, mono right-aligned duration, hover-only icon buttons (rename, delete). Selected: accent-soft + 3px inset accent bar. Group by day with 12/600 faint headers.
- Dropdown: 30px control, bg color fill, line2 border, chevron right. Open menu: raised bg, line2 border, 9px radius, 4px padding, 7x10 items, hover accent-soft, check on current.
- Toggle: 38x22 pill, thumb 16px white; off = line2, on = accent.
- Banner: 9px radius, 4px left bar. Warning (warn-soft, warn bar) and error (rec-soft, err bar). Bold sentence saying what happened, then what to do, plus one action button on the right. No apologies.
- Tags: 20px pills, 11/500; accent variant for the user tags that match the search or the default model.
- Empty state: centered 18/600 heading plus one dim sentence that says what to do next.
- Buttons: 32px, 7px radius, 1px line2 border, surface fill. `pri` = accent fill. `dng` = err text and err-tinted border. `sm` = 26px.

## GPUI implementation notes
Define tokens as `const` u32 and pick a palette by theme struct (`struct Theme { bg: u32, ... }`, `Theme::dark()`, `Theme::light()`) stored in a `Global`, so `rgb(theme.bg)` replaces the current BG/PANEL/FG/DIM/ACCENT constants in `view.rs`. Alpha tokens: `rgba(0x5C9DF512)` (0xRRGGBBAA; 0.07 alpha is 0x12).

Fonts: bundle the OTFs and register with `cx.text_system().add_fonts(vec![Cow::Borrowed(include_bytes!(...))])`. Then `.font_family("IBM Plex Sans")` on the root div and `.font_family("IBM Plex Mono")` on clock, timestamps, speaker ids, sizes. Set weights with `.font_weight(FontWeight::MEDIUM | SEMIBOLD)`. Sizes: `.text_size(px(17.))`; line height `.line_height(relative(1.5))`.

- Window: `div().size_full().flex().bg(rgb(t.bg)).text_color(rgb(t.text)).font_family("IBM Plex Sans")`. Recording edge: wrap content with `.border_t(px(3.)).border_color(rgb(rec))` (use `transparent` border when idle so layout does not jump). Window default size `size(px(1100.), px(720.))`, min 860x560. The blueprint grid is optional polish: paint with a `canvas` element drawing 1px lines every 24px at the grid alpha; skip if it costs frames.
- Rail: `div().w(px(176.)).flex_none().flex().flex_col().gap(px(2.)).p(px(10.)).bg(rgb(side)).border_r_1().border_color(rgb(line))`. Tab: `div().h(px(34.)).px(px(10.)).rounded(px(7.)).flex().items_center().gap(px(10.)).cursor_pointer().hover(|s| s.bg(rgb(raised)))`; active adds `.bg(rgb(accent_soft))` and an absolutely positioned child `div().absolute().left(px(-10.)).top(px(7.)).bottom(px(7.)).w(px(3.)).rounded_r(px(3.)).bg(rgb(accent))` (parent `.relative()`). Icons: embed SVGs (`svg().path("icons/mic.svg").size(px(16.)).text_color(...)` with an AssetSource).
- Record strip: `div().flex().items_center().gap(px(20.)).mx(px(28.)).px(px(18.)).py(px(14.)).rounded(px(12.)).border_1()` with bg/border switched on `Rec` (the existing `Controls::of` and `badge_text` remain the logic). Clock: `.font_family("IBM Plex Mono").text_size(px(38.)).font_weight(FontWeight::MEDIUM).min_w(px(170.))`. Lamp: 14px `rounded_full`; pulse with `with_animation("pulse", Animation::new(Duration::from_millis(1600)).repeat(), |el, d| el.opacity(...))` (animate opacity of a ring div, since GPUI has no box-shadow spread). Skip animation if reduced motion is on.
- Meter: track `div().h(px(5.)).flex_1().rounded_full().bg(rgb(line)).overflow_hidden()` with child `div().h_full().w(relative(level))`. Feed level from the capture RMS, redraw at about 15 Hz.
- Transcript: reuse `lines(...)` in `view.rs`: replace `gap_1` with rows of `px(12.) py(px(7.)) rounded(px(8.))`; columns `w(px(48.))`, `w(px(86.))` flex_none, then `flex_1().min_w_0().whitespace_normal()` text. Speaker color from a function `speaker_color(&str) -> u32` (`you` fixed, `remote_n` index mod 3 onto the three remote hues). Tag bar: `div().w(px(3.)).h(px(14.)).rounded(px(2.)).bg(rgb(c))` beside the name in a `flex().gap(px(7.))` container. Scroll: `div().id("tx").overflow_y_scroll().track_scroll(&handle)`; call `handle.scroll_to_bottom()` on new lines when pinned. Cap the visible column with `.max_w(px(760.))` for line length.
- List rows: `div().h(px(60.)).px(px(16.)).flex().items_center().gap(px(14.)).border_b_1().border_color(rgb(line)).hover(|s| s.bg(rgb(raised)))`; selected: `.bg(rgb(accent_soft)).border_l(px(3.)).border_color(rgb(accent))`. Ellipsis: `.overflow_hidden().text_ellipsis().whitespace_nowrap()`. Hover-only actions: use `.group("row")` on the row and `.invisible().group_hover("row", |s| s.visible())` on the actions.
- Dropdown: trigger div as in the Settings mock; menu via `deferred(anchored().child(div()...))` so it floats over siblings; shadow `.shadow(vec![BoxShadow { color: hsla(0.,0.,0.,0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.) }])`. Close on outside click (`on_mouse_down_out`).
- Toggle: `div().w(px(38.)).h(px(22.)).rounded_full().bg(...).relative()` with a child `div().absolute().top(px(3.)).left(px(if on {19.} else {3.})).size(px(16.)).rounded_full().bg(white)`.
- Banner: `div().flex().items_center().gap(px(12.)).mx(px(28.)).px(px(14.)).py(px(10.)).rounded(px(9.)).border_l(px(4.)).border_color(rgb(warn)).bg(rgb(warn_soft))`; message `div().flex_1()` (bold lead via a nested `div().font_weight(FontWeight::SEMIBOLD)` in a flex-wrap row). Reuses `stt_problem`.
- Buttons: replace `button()` in `view.rs` with a `kind` enum (Primary, Secondary, Danger, Rec); disabled = `.opacity(0.4)` and no `on_click` (as now).
- Models table: fixed-width columns (`w(px(84.))` size, `w(px(70.))` WER, `w(px(80.))` speed, `w(px(150.))` action), numbers in Plex Mono right-aligned (`.text_right()`); split `ModelRow.facts` into separate fields (size, wer, rtfx, languages, runtime) so columns align rather than one comma string.
- Contrast: all text pairs above meet WCAG AA at 13px+, except `faint` which is only for 11-12px meta (4.0:1 dark, 3.4:1 light); if accessibility matters more than airiness, darken light `faint` to #6B7C93.

## Unresolved
- Fonts: bundling Plex adds about 600 KB per weight; confirm licensing (SIL OFL, fine).
- Pulse animation and grid are optional polish; ship without them first.
- Settings screen is a representative subset; the real config keys were not mapped into groups.
- Trash tab exists in code (`Tab::Trash`) but not in the mock; treat it as a Sessions sub-view ("Recently deleted").
- I did not screenshot the current app; the current look was taken from `view.rs` constants (Catppuccin Mocha: #1e1e2e / #313244 / #89b4fa) and `oats ctl ui-state`.
