//! The menu a Linux window shows for [`crate::WindowControls::show_context_menu`]:
//! a small Florui tree painted by the same code as a window, plus the state a
//! menu keeps (which row the pointer or the keyboard is on) and what each input
//! does. A host presents its frames in a surface of its own and feeds it input;
//! nothing here knows about X11 or Wayland.

use std::cell::RefCell;
use std::rc::Rc;

use florui::prelude::*;
use florui_reactive::{Signal, use_signal};
use florui_style::Rgba;

use crate::headless::{HeadlessOptions, HeadlessWindow};
use crate::menu::{ContextMenuOutcome, MenuCommandId, MenuEntry};

const CSS: &str = include_str!("menu_popup.css");

/// The sizes the stylesheet gives a row and a separator, and the padding plus
/// border around the rows; a test checks them against the painted tree.
const ITEM_HEIGHT: f32 = 28.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const EDGE: f32 = 5.0;

#[derive(Clone, Debug, PartialEq)]
enum Row {
    Item {
        id: MenuCommandId,
        label: String,
        enabled: bool,
        checked: bool,
        shortcut: Option<String>,
    },
    Separator,
}

impl Row {
    fn height(&self) -> f32 {
        match self {
            Row::Item { .. } => ITEM_HEIGHT,
            Row::Separator => SEPARATOR_HEIGHT,
        }
    }

    fn enabled_item(&self) -> Option<MenuCommandId> {
        match self {
            Row::Item {
                id, enabled: true, ..
            } => Some(*id),
            _ => None,
        }
    }
}

/// What a key does to a menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PopupKey {
    Up,
    Down,
    Home,
    End,
    Activate,
    Dismiss,
}

/// A painted frame, premultiplied BGRA (a little-endian `ARGB8888`), rows top
/// to bottom.
pub(crate) struct PopupFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub(crate) struct MenuPopup {
    window: HeadlessWindow,
    rows: Vec<Row>,
    hover: Signal<Option<usize>>,
    hovered: Option<usize>,
    scale: f64,
    /// Logical size of the menu.
    size: (f32, f32),
}

fn rows_of(entries: &[MenuEntry]) -> Vec<Row> {
    entries
        .iter()
        .map(|entry| match entry {
            MenuEntry::Action {
                id,
                label,
                enabled,
                checked,
                shortcut,
            } => Row::Item {
                id: *id,
                label: label.clone(),
                enabled: *enabled,
                checked: *checked,
                shortcut: shortcut.clone(),
            },
            MenuEntry::Separator => Row::Separator,
        })
        .collect()
}

fn render_row(row: &Row, hovered: bool) -> Element {
    match row {
        Row::Separator => view! { <div class="separator" /> },
        Row::Item {
            label,
            enabled,
            checked,
            shortcut,
            ..
        } => {
            let class = format!(
                "item{}{}",
                if hovered { " hover" } else { "" },
                if *enabled { "" } else { " disabled" }
            );
            let mark = if *checked { "\u{2713}" } else { "" };
            view! {
                <div class={class} role="menuitem">
                    <span class="check">{mark}</span>
                    <span class="label">{label.clone()}</span>
                    {shortcut
                        .iter()
                        .map(|text| view! { <span class="shortcut">{text.clone()}</span> })
                        .collect::<Vec<_>>()}
                </div>
            }
        }
    }
}

impl MenuPopup {
    /// `None` when there is nothing to show.
    pub(crate) fn new(entries: &[MenuEntry], scale: f64, dark: bool) -> Option<Self> {
        let rows = rows_of(entries);
        if rows.is_empty() {
            return None;
        }
        let tall: f32 = rows.iter().map(Row::height).sum::<f32>() + 2.0 * EDGE;
        let shared: Rc<RefCell<Option<Signal<Option<usize>>>>> = Rc::default();
        let tree = Rc::new(rows.clone());
        let root = {
            let shared = Rc::clone(&shared);
            let tree = Rc::clone(&tree);
            move || {
                let hover = use_signal(|| None::<usize>);
                *shared.borrow_mut() = Some(hover.clone());
                let current = hover.get();
                view! {
                    <div class="page">
                        <div class="menu" role="menu">
                            {tree
                                .iter()
                                .enumerate()
                                .map(|(index, row)| render_row(row, current == Some(index)))
                                .collect::<Vec<_>>()}
                        </div>
                    </div>
                }
            }
        };
        let mut window = HeadlessWindow::new(
            CSS,
            root,
            HeadlessOptions {
                // Wide enough for any label: the menu takes the width of its
                // longest row, which is measured below.
                width: 1200.0,
                height: tall,
                scale_factor: scale,
                canvas_color: Rgba::TRANSPARENT,
                prefers_dark: dark,
                assistive_technology: true,
                ..HeadlessOptions::default()
            },
        )
        .ok()?;
        let hover = shared.borrow().clone()?;
        let measured = window.frame();
        let menu = measured
            .accessibility
            .iter()
            .find(|node| node.role == "Menu")?;
        let (_, _, width, height) = menu.bounds?;
        let size = (
            (width / scale as f32).ceil(),
            (height / scale as f32).ceil(),
        );
        window.resize(size.0, size.1);
        Some(Self {
            window,
            rows,
            hover,
            hovered: None,
            scale,
            size,
        })
    }

    /// Logical size of the menu.
    pub(crate) fn size(&self) -> (f32, f32) {
        self.size
    }

    /// The row a point (logical, in the menu) is on, if it is on an item.
    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < EDGE || x >= self.size.0 - EDGE {
            return None;
        }
        let mut top = EDGE;
        for (index, row) in self.rows.iter().enumerate() {
            let bottom = top + row.height();
            if y >= top && y < bottom {
                return matches!(row, Row::Item { .. }).then_some(index);
            }
            top = bottom;
        }
        None
    }

    fn inside(&self, x: f32, y: f32) -> bool {
        x >= 0.0 && y >= 0.0 && x < self.size.0 && y < self.size.1
    }

    fn highlight(&mut self, row: Option<usize>) {
        if self.hovered != row {
            self.hovered = row;
            self.hover.set(row);
            self.window.update();
        }
    }

    /// The pointer is at `(x, y)` (logical, in the menu). A disabled item is
    /// not highlighted.
    pub(crate) fn pointer_moved(&mut self, x: f32, y: f32) {
        let row = self
            .row_at(x, y)
            .filter(|&index| self.rows[index].enabled_item().is_some());
        self.highlight(row);
    }

    /// The pointer left the menu.
    pub(crate) fn pointer_left(&mut self) {
        self.highlight(None);
    }

    /// A press at `(x, y)` (logical, in the menu); `None` is a press anywhere
    /// outside it. Outside dismisses, an enabled item chooses, anything else
    /// in the menu does nothing.
    pub(crate) fn pointer_pressed(&mut self, at: Option<(f32, f32)>) -> Option<ContextMenuOutcome> {
        let Some((x, y)) = at.filter(|&(x, y)| self.inside(x, y)) else {
            return Some(ContextMenuOutcome::Dismissed);
        };
        self.row_at(x, y)
            .and_then(|index| self.rows[index].enabled_item())
            .map(ContextMenuOutcome::Selected)
    }

    fn enabled_rows(&self) -> Vec<usize> {
        (0..self.rows.len())
            .filter(|&index| self.rows[index].enabled_item().is_some())
            .collect()
    }

    pub(crate) fn key_pressed(&mut self, key: PopupKey) -> Option<ContextMenuOutcome> {
        let enabled = self.enabled_rows();
        let position = self
            .hovered
            .and_then(|row| enabled.iter().position(|&index| index == row));
        let target = match key {
            PopupKey::Dismiss => return Some(ContextMenuOutcome::Dismissed),
            PopupKey::Activate => {
                return self
                    .hovered
                    .and_then(|row| self.rows[row].enabled_item())
                    .map(ContextMenuOutcome::Selected);
            }
            PopupKey::Home => enabled.first().copied(),
            PopupKey::End => enabled.last().copied(),
            PopupKey::Down => match position {
                Some(at) => enabled.get((at + 1) % enabled.len()).copied(),
                None => enabled.first().copied(),
            },
            PopupKey::Up => match position {
                Some(at) => enabled
                    .get((at + enabled.len() - 1) % enabled.len())
                    .copied(),
                None => enabled.last().copied(),
            },
        };
        self.highlight(target);
        None
    }

    /// The row highlighted now, as an index into the entries given.
    #[cfg(test)]
    pub(crate) fn highlighted(&self) -> Option<usize> {
        self.hovered
    }

    /// Paints the menu as it is now.
    pub(crate) fn frame(&mut self) -> PopupFrame {
        let frame = self.window.frame();
        let mut bgra = frame.rgba;
        for pixel in bgra.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        PopupFrame {
            width: frame.width,
            height: frame.height,
            bgra,
        }
    }

    /// Device pixels per logical pixel.
    pub(crate) fn scale(&self) -> f64 {
        self.scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(id: u32, label: &str, enabled: bool) -> MenuEntry {
        MenuEntry::Action {
            id: MenuCommandId(id),
            label: label.to_owned(),
            enabled,
            checked: false,
            shortcut: None,
        }
    }

    fn menu() -> MenuPopup {
        MenuPopup::new(
            &[
                action(1, "Save", true),
                MenuEntry::Separator,
                action(2, "Save as", false),
                action(3, "Close", true),
            ],
            1.0,
            false,
        )
        .expect("a menu")
    }

    #[test]
    fn nothing_to_show_is_no_menu() {
        assert!(MenuPopup::new(&[], 1.0, false).is_none());
    }

    #[test]
    fn the_rows_sit_where_the_stylesheet_puts_them() {
        let mut popup = menu();
        let frame = popup.window.frame();
        let items: Vec<_> = frame
            .accessibility
            .iter()
            .filter(|node| node.role == "MenuItem")
            .collect();
        assert_eq!(items.len(), 3);
        // Row 0 is the first item, row 2 the disabled one, row 3 the last.
        let tops = [EDGE, EDGE + ITEM_HEIGHT + SEPARATOR_HEIGHT, {
            EDGE + 2.0 * ITEM_HEIGHT + SEPARATOR_HEIGHT
        }];
        for (node, top) in items.iter().zip(tops) {
            let (_, y, _, height) = node.bounds.expect("bounds");
            assert_eq!(
                (y, height),
                (top, ITEM_HEIGHT),
                "{}",
                node.name.clone().unwrap_or_default()
            );
        }
        let (width, height) = popup.size();
        assert_eq!(height, 2.0 * EDGE + 3.0 * ITEM_HEIGHT + SEPARATOR_HEIGHT);
        assert!(width >= 168.0, "the minimum width holds: {width}");
    }

    #[test]
    fn the_pointer_highlights_enabled_items_only() {
        let mut popup = menu();
        let (w, _) = popup.size();
        popup.pointer_moved(w / 2.0, EDGE + 5.0);
        assert_eq!(popup.highlighted(), Some(0));
        popup.pointer_moved(w / 2.0, EDGE + ITEM_HEIGHT + 2.0);
        assert_eq!(popup.highlighted(), None, "a separator");
        popup.pointer_moved(w / 2.0, EDGE + ITEM_HEIGHT + SEPARATOR_HEIGHT + 3.0);
        assert_eq!(popup.highlighted(), None, "a disabled item");
        popup.pointer_moved(w / 2.0, EDGE + 2.0 * ITEM_HEIGHT + SEPARATOR_HEIGHT + 3.0);
        assert_eq!(popup.highlighted(), Some(3));
        popup.pointer_left();
        assert_eq!(popup.highlighted(), None);
        popup.pointer_moved(2.0, EDGE + 5.0);
        assert_eq!(popup.highlighted(), None, "the padding is not a row");
        popup.pointer_moved(w - 2.0, EDGE + 5.0);
        assert_eq!(popup.highlighted(), None, "nor is the other side");
    }

    #[test]
    fn a_press_chooses_dismisses_or_does_nothing() {
        let mut popup = menu();
        let (w, _) = popup.size();
        assert_eq!(
            popup.pointer_pressed(Some((w / 2.0, EDGE + 5.0))),
            Some(ContextMenuOutcome::Selected(MenuCommandId(1)))
        );
        assert_eq!(
            popup.pointer_pressed(Some((w / 2.0, EDGE + ITEM_HEIGHT + SEPARATOR_HEIGHT + 3.0))),
            None,
            "a disabled item"
        );
        assert_eq!(
            popup.pointer_pressed(Some((w / 2.0, EDGE + ITEM_HEIGHT + 2.0))),
            None
        );
        assert_eq!(
            popup.pointer_pressed(Some((w + 10.0, 5.0))),
            Some(ContextMenuOutcome::Dismissed)
        );
        assert_eq!(
            popup.pointer_pressed(None),
            Some(ContextMenuOutcome::Dismissed)
        );
    }

    #[test]
    fn keys_move_over_the_enabled_items_and_wrap() {
        let mut popup = menu();
        assert_eq!(popup.key_pressed(PopupKey::Down), None);
        assert_eq!(popup.highlighted(), Some(0));
        popup.key_pressed(PopupKey::Down);
        assert_eq!(popup.highlighted(), Some(3), "the disabled item is skipped");
        popup.key_pressed(PopupKey::Down);
        assert_eq!(popup.highlighted(), Some(0), "wraps");
        popup.key_pressed(PopupKey::Up);
        assert_eq!(popup.highlighted(), Some(3), "wraps back");
        popup.key_pressed(PopupKey::Home);
        assert_eq!(popup.highlighted(), Some(0));
        popup.key_pressed(PopupKey::End);
        assert_eq!(popup.highlighted(), Some(3));
        assert_eq!(
            popup.key_pressed(PopupKey::Activate),
            Some(ContextMenuOutcome::Selected(MenuCommandId(3)))
        );
        assert_eq!(
            popup.key_pressed(PopupKey::Dismiss),
            Some(ContextMenuOutcome::Dismissed)
        );
    }

    #[test]
    fn a_frame_is_as_many_pixels_as_the_scale_says() {
        let entries = [action(1, "Save", true)];
        let mut plain = MenuPopup::new(&entries, 1.0, false).expect("menu");
        let mut sharp = MenuPopup::new(&entries, 2.0, false).expect("menu");
        assert_eq!(sharp.scale(), 2.0);
        let (width, height) = sharp.size();
        let frame = sharp.frame();
        assert_eq!(
            (frame.width, frame.height),
            ((width * 2.0) as u32, (height * 2.0) as u32)
        );
        let frame = plain.frame();
        assert_eq!((frame.width, frame.height), (width as u32, height as u32));
    }

    #[test]
    fn activating_with_nothing_highlighted_does_nothing() {
        let mut popup = menu();
        assert_eq!(popup.key_pressed(PopupKey::Activate), None);
        let mut only_disabled =
            MenuPopup::new(&[action(9, "No", false)], 1.0, false).expect("menu");
        assert_eq!(only_disabled.key_pressed(PopupKey::Down), None);
        assert_eq!(only_disabled.highlighted(), None);
    }

    #[test]
    fn the_highlight_is_painted_and_the_frame_is_premultiplied_bgra() {
        let mut popup = menu();
        let plain = popup.frame();
        popup.key_pressed(PopupKey::Down);
        let lit = popup.frame();
        assert_eq!((lit.width, lit.height), (plain.width, plain.height));
        assert_ne!(lit.bgra, plain.bgra, "the first row is highlighted");
        // Inside the first row, away from its text, the highlight is blue:
        // the accent color #3584e4 in B, G, R order.
        let at = ((EDGE as u32 + 14) * lit.width + (lit.width - 12)) as usize * 4;
        assert_eq!(&lit.bgra[at..at + 4], &[0xe4, 0x84, 0x35, 0xff]);
        // The window corner is transparent.
        assert_eq!(lit.bgra[3], 0);
    }
}
