# Control behavior matrix

What each built-in control does with the pointer and keyboard, how it takes part
in a form and what it exposes to assistive technology. This is the published
coverage for the initial release: it does not claim arbitrary HTML control
equivalence, and a row says "not supported" where that is the case. Behavior
marked *measured* was compared against Edge with real input; accessibility
coverage was checked through the UI Automation tree and, for the controls listed
under Verification, by ear with Windows Narrator.

| Control | Pointer | Keyboard | Disabled | In a form | Accessibility role |
|---|---|---|---|---|---|
| `<button>` | Click | Enter and Space activate | Not focusable, no click | `type` submit (default), reset or button; a named submitter adds its `name`/`value` | Button |
| `<a href>` | Click opens the href or route | Enter activates (Space does not) | Not focusable | none | Link |
| `<input>` text, password, email, url, tel, search | Click sets the caret, double-click selects a word, drag selects | Caret movement, selection, clipboard, undo/redo, Tab selects all, Enter submits the form, IME composition (not in a password) | Not focusable | Entry by `name`; `required`, `minlength`, `maxlength`, `pattern`; email and url type checks; `maxlength` enforced while typing | TextInput, PasswordInput |
| `<input type=number>` | As text, plus spinner arrows (press, hold to repeat) | Up/Down step, typing filtered to digits, sign, point and `e` | As text | `min`, `max`, `step`, bad input; `:in-range`/`:out-of-range` | SpinButton with its limits |
| `<input type=hidden>` | none (no box) | Not focusable | n/a | Always submitted, never validated | Hidden |
| `<input type=checkbox>`, `radio` | Click toggles or selects | Space toggles; arrows move and select within a radio group (one tab stop per group); `role=switch` on a checkbox | Not focusable | Submitted when checked; `required` (a radio group needs one checked) | CheckBox, RadioButton, Switch |
| `<input type=range>` | Drag the thumb, click the track | Arrows, Page Up/Down, Home/End | Not focusable | Entry by `name` | Slider |
| `<select>` | Click opens; click an option | Up/Down move, Enter commits, Escape closes; `multiple` supported | Not focusable | Selected option(s) by `name`; `required` | ComboBox, ListBox, ListBoxOption |
| `<textarea>` | As text, plus triple-click for a paragraph, wheel, scrollbar (buttons, thumb, track, press-and-hold) and a resize corner | As text; Enter inserts a line, Up/Down and Page keys move between rows | Not focusable | As text; Enter never submits | MultilineTextInput |
| `<label for>` | Click hands focus and activation to its control | none | Follows its control | none | Label, associated to its control |
| `<form>`, `<fieldset disabled>` | none | Enter in a field clicks the default submit button | A disabled fieldset disables its controls (the first legend is exempt) | Validation on submit with the message bubble, `novalidate`, `formnovalidate`, the `form` attribute | Form landmark when named |
| `Dialog` | Clicks outside the content are the app's to handle | Focus moves in and stays in (Tab wraps), Escape closes, focus returns to the opener; dialogs stack | n/a | none | Dialog, modal |
| `Popover` with `role="menu"` | Click outside dismisses | Focus enters the first item; Up/Down wrap, Home/End, Right opens a submenu, Left closes it, Escape and Tab close it and focus returns to the trigger | Not focusable items | none | Menu, MenuItem, trigger reports expanded |
| Virtualized list | Wheel and scrollbar | With focus on a row or a control in it: Up/Down move one row, Page Up/Down a viewport of rows, Home/End to the ends, clamped, repeating with a held key, scrolling only as far as needed; a select or radio in a row keeps the arrows; the focused row stays mounted; `focus_item` brings any item into view | n/a | none | List, ListItem with position in the whole collection |

## Not supported

`<input type=submit|reset|button>`, the date and time types, `color`, `file`,
`<datalist>`, styling a placeholder with `::placeholder` (use
`--florui-placeholder-color`) and `resize` or `appearance` as real CSS
properties (use `--florui-resize` and `--florui-appearance`).

## Verification

Headless tests drive a `UiRuntime` directly; behavior that needs real windows,
mouse or keyboard input was checked on Windows through UI Automation, and the
scripts in `scripts/real-session/` repeat part of it.

Narrator (Windows 11) was run against the `controlled_form`, `popover` and
`virtualized_list` examples:

- **Announced as expected**: text, password and email fields with their name and
  "required"; the spin button with its value and maximum; the combo box
  expanding and collapsing and reading its chosen option; the checkbox and
  radio states changing; the form landmark on entering it; the validation
  message after a failed submit; buttons; menu items, a submenu opening and
  closing, and focus returning to the trigger on Escape; list rows while
  moving with the arrows, Home, End and Page Down.
- **Not confirmed**: a row's position in the whole list ("n of N"), the menu
  container being announced on open, and whether the submenu item says it is
  collapsed.

NVDA and JAWS were not tried.
