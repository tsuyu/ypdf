//! The bookmarks panel (spec §17).
//!
//! Reading a long document is mostly navigation, so this is a panel rather than
//! a dialog: it stays open beside the page and clicking a bookmark jumps there.
//!
//! Editing is the second job and is kept out of the way of the first. A
//! bookmark is added for the page being read, which is the only place someone
//! ever wants one, and renaming happens in place.

use egui::{Context, Ui};
use ypdf_outline::Bookmark;

/// State of the panel.
#[derive(Debug, Default)]
pub struct OutlinePanel {
    /// Is it showing?
    pub open: bool,
    /// The tree as it currently stands.
    pub tree: Vec<Bookmark>,
    /// True once the tree has been read from the document.
    pub loaded: bool,
    /// Title being typed for a new bookmark.
    pub new_title: String,
    /// The bookmark being renamed, as its position in the flattened tree.
    pub renaming: Option<usize>,
    /// The text being typed into a rename.
    pub rename_text: String,
    /// True when the tree has been changed and not written.
    pub dirty: bool,
    /// Why the last write failed.
    pub error: Option<String>,
}

/// What the panel is asking for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Jump to a page, 1-based.
    Go(u32),
    /// Add a bookmark for the page being read.
    Add,
    /// Remove the bookmark at this flattened position.
    Remove(usize),
    /// Rename the bookmark at this flattened position.
    Rename(usize, String),
    /// Write the tree into the document.
    Save,
}

impl OutlinePanel {
    /// Take a tree read from the document.
    pub fn load(&mut self, tree: Vec<Bookmark>) {
        self.tree = tree;
        self.loaded = true;
        self.dirty = false;
    }

    /// Forget the tree, so it is read again.
    pub fn invalidate(&mut self) {
        self.loaded = false;
        self.dirty = false;
        self.renaming = None;
    }

    /// The bookmark at a flattened position, mutably.
    ///
    /// The panel lists a tree as a flat sequence, so the position someone
    /// clicked has to be turned back into a place in the tree.
    pub fn at_mut(&mut self, index: usize) -> Option<&mut Bookmark> {
        fn walk<'a>(
            level: &'a mut [Bookmark],
            index: usize,
            seen: &mut usize,
        ) -> Option<&'a mut Bookmark> {
            for bookmark in level {
                if *seen == index {
                    return Some(bookmark);
                }
                *seen += 1;
                // `children` has to be reborrowed here, which is why this is a
                // function rather than an iterator chain.
                if let Some(found) = walk(&mut bookmark.children, index, seen) {
                    return Some(found);
                }
            }
            None
        }

        let mut seen = 0;
        walk(&mut self.tree, index, &mut seen)
    }

    /// Remove the bookmark at a flattened position, keeping its children.
    ///
    /// Deleting a heading should not silently delete everything under it: the
    /// children move up to take its place.
    pub fn remove_at(&mut self, index: usize) -> bool {
        fn walk(level: &mut Vec<Bookmark>, index: usize, seen: &mut usize) -> bool {
            for position in 0..level.len() {
                if *seen == index {
                    let removed = level.remove(position);
                    for (offset, child) in removed.children.into_iter().enumerate() {
                        level.insert(position + offset, child);
                    }
                    return true;
                }
                *seen += 1;
                if walk(&mut level[position].children, index, seen) {
                    return true;
                }
            }
            false
        }

        let mut seen = 0;
        let removed = walk(&mut self.tree, index, &mut seen);
        if removed {
            self.dirty = true;
        }
        removed
    }

    /// Draw the panel.
    pub fn show(&mut self, ctx: &Context, current_page: u32) -> Option<Action> {
        if !self.open {
            return None;
        }

        let mut action = None;
        let mut open = self.open;

        egui::Window::new("Bookmarks")
            .open(&mut open)
            .default_width(300.0)
            .show(ctx, |ui| {
                action = self.body(ui, current_page);
            });

        self.open = open;
        action
    }

    fn body(&mut self, ui: &mut Ui, current_page: u32) -> Option<Action> {
        let mut action = None;

        if !self.loaded {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Reading…");
            });
            return None;
        }

        egui::ScrollArea::vertical()
            .max_height(320.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                if self.tree.is_empty() {
                    ui.weak("This document has no bookmarks.");
                    return;
                }
                action = self.list(ui);
            });

        ui.separator();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_title)
                    .desired_width(150.0)
                    .hint_text("New bookmark"),
            );
            if ui
                .add_enabled(
                    !self.new_title.trim().is_empty(),
                    egui::Button::new(format!("Add for page {current_page}")),
                )
                .clicked()
            {
                action = Some(Action::Add);
            }
        });

        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }

        if self.dirty {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button("Save into the document").clicked() {
                    action = Some(Action::Save);
                }
                ui.colored_label(ui.visuals().warn_fg_color, "Changed — not written");
            });
        }

        action
    }

    fn list(&mut self, ui: &mut Ui) -> Option<Action> {
        // Collected first so the tree is not borrowed while the rows draw.
        let rows: Vec<(usize, usize, String, Option<u32>)> =
            ypdf_outline::bookmarks::flatten(&self.tree)
                .into_iter()
                .enumerate()
                .map(|(index, (depth, bookmark))| {
                    (index, depth, bookmark.title.clone(), bookmark.page)
                })
                .collect();

        let mut action = None;

        for (index, depth, title, page) in rows {
            ui.horizontal(|ui| {
                ui.add_space((depth as f32) * 12.0);

                if self.renaming == Some(index) {
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut self.rename_text).desired_width(160.0),
                    );
                    field.request_focus();
                    let done = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if done || ui.button("Done").clicked() {
                        action = Some(Action::Rename(index, self.rename_text.clone()));
                    }
                    return;
                }

                let label = match page {
                    Some(page) => format!("{title}  ·  p{page}"),
                    None => format!("{title}  ·  —"),
                };
                if ui
                    .add(egui::Label::new(label).sense(egui::Sense::click()))
                    .on_hover_text("Click to go there")
                    .clicked()
                    && let Some(page) = page
                {
                    action = Some(Action::Go(page));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("✕").on_hover_text("Remove").clicked() {
                        action = Some(Action::Remove(index));
                    }
                    if ui.small_button("✎").on_hover_text("Rename").clicked() {
                        self.renaming = Some(index);
                        self.rename_text = title.clone();
                    }
                });
            });
        }

        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> Vec<Bookmark> {
        vec![
            Bookmark {
                title: "One".into(),
                children: vec![Bookmark::new("One.a", 2), Bookmark::new("One.b", 3)],
                ..Bookmark::new("One", 1)
            },
            Bookmark::new("Two", 4),
        ]
    }

    #[test]
    fn a_flattened_position_finds_the_right_bookmark() {
        let mut panel = OutlinePanel::default();
        panel.load(tree());

        assert_eq!(
            panel.at_mut(0).map(|b| b.title.clone()).as_deref(),
            Some("One")
        );
        assert_eq!(
            panel.at_mut(1).map(|b| b.title.clone()).as_deref(),
            Some("One.a")
        );
        assert_eq!(
            panel.at_mut(3).map(|b| b.title.clone()).as_deref(),
            Some("Two")
        );
        assert!(panel.at_mut(9).is_none());
    }

    #[test]
    fn removing_a_heading_keeps_what_was_under_it() {
        // Otherwise deleting one line of the outline silently takes a chapter
        // of navigation with it.
        let mut panel = OutlinePanel::default();
        panel.load(tree());

        assert!(panel.remove_at(0));

        let titles: Vec<String> = ypdf_outline::bookmarks::flatten(&panel.tree)
            .into_iter()
            .map(|(_, bookmark)| bookmark.title.clone())
            .collect();
        assert_eq!(titles, vec!["One.a", "One.b", "Two"]);
        assert!(panel.dirty);
    }

    #[test]
    fn removing_a_leaf_leaves_the_rest_alone() {
        let mut panel = OutlinePanel::default();
        panel.load(tree());

        assert!(panel.remove_at(2));
        let titles: Vec<String> = ypdf_outline::bookmarks::flatten(&panel.tree)
            .into_iter()
            .map(|(_, bookmark)| bookmark.title.clone())
            .collect();
        assert_eq!(titles, vec!["One", "One.a", "Two"]);
    }

    #[test]
    fn removing_something_that_is_not_there_changes_nothing() {
        let mut panel = OutlinePanel::default();
        panel.load(tree());
        assert!(!panel.remove_at(99));
        assert!(!panel.dirty);
    }

    #[test]
    fn invalidating_forgets_the_tree_so_it_is_read_again() {
        let mut panel = OutlinePanel::default();
        panel.load(tree());
        panel.dirty = true;

        panel.invalidate();
        assert!(!panel.loaded);
        assert!(!panel.dirty);
    }
}
