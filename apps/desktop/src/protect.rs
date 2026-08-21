//! Password prompts and the Protect dialog (spec §8).
//!
//! Two dialogs that pull in opposite directions and share one honesty
//! requirement.
//!
//! The **password prompt** asks for a password to open a file. It says which of
//! the two situations it is in — no password given, or the wrong one — because
//! "could not open" leaves someone retyping a password that was never going to
//! work.
//!
//! The **Protect dialog** writes a protected copy. It says out loud that
//! permission flags are advisory, since the whole value of that sentence is
//! being read before someone relies on the flags rather than after.

use egui::{Context, RichText, Ui};
use ypdf_crypt::{Algorithm, EncryptSettings, Permissions};

/// A prompt for the password of one document.
#[derive(Debug, Default)]
pub struct PasswordPrompt {
    /// Is it showing?
    pub open: bool,
    /// What has been typed.
    pub password: String,
    /// Set when a previous attempt was made with the wrong password.
    pub was_wrong: bool,
}

impl PasswordPrompt {
    /// Ask for a password because the file needs one.
    pub fn ask(&mut self, was_wrong: bool) {
        self.open = true;
        self.was_wrong = was_wrong;
        if was_wrong {
            // Leaving the rejected text in the box invites retyping the same
            // thing; clearing it says plainly that this one did not work.
            self.password.clear();
        }
    }

    /// Draw it. Returns the password when the user submits one.
    pub fn show(&mut self, ctx: &Context, name: &str) -> Option<String> {
        if !self.open {
            return None;
        }

        let mut submitted = None;
        let mut open = self.open;

        egui::Window::new("Password required")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.label(format!("{name} is protected."));
                if self.was_wrong {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        "That password was not accepted.",
                    );
                }
                ui.add_space(4.0);

                let field = ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .hint_text("Open or owner password")
                        .desired_width(f32::INFINITY),
                );
                field.request_focus();

                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() || entered {
                        submitted = Some(self.password.clone());
                    }
                    ui.weak("Nothing is sent anywhere.");
                });
            });

        self.open = open;
        if submitted.is_some() {
            self.open = false;
        }
        submitted
    }
}

/// The Protect dialog.
#[derive(Debug)]
pub struct ProtectDialog {
    /// Is it showing?
    pub open: bool,
    /// Password needed to open the document.
    pub user_password: String,
    /// Password needed to change the protection.
    pub owner_password: String,
    /// Which cipher.
    pub algorithm: Algorithm,
    /// What readers may do.
    pub permissions: Permissions,
    /// Why the last attempt failed.
    pub error: Option<String>,
}

impl Default for ProtectDialog {
    fn default() -> Self {
        Self {
            open: false,
            user_password: String::new(),
            owner_password: String::new(),
            algorithm: Algorithm::Aes256,
            permissions: Permissions::all(),
            error: None,
        }
    }
}

impl ProtectDialog {
    /// The settings as they stand.
    #[must_use]
    pub fn settings(&self) -> EncryptSettings {
        EncryptSettings {
            algorithm: self.algorithm,
            user_password: self.user_password.clone(),
            owner_password: self.owner_password.clone(),
            permissions: self.permissions,
        }
    }

    /// Is there enough here to protect anything?
    #[must_use]
    pub fn is_usable(&self) -> bool {
        !self.user_password.is_empty() || !self.owner_password.is_empty()
    }

    /// Draw it. Returns true when the user asks to write the protected copy.
    pub fn show(&mut self, ctx: &Context) -> bool {
        if !self.open {
            return false;
        }

        let mut protect = false;
        let mut open = self.open;

        egui::Window::new("Protect")
            .open(&mut open)
            .resizable(false)
            .default_width(420.0)
            .show(ctx, |ui| {
                self.passwords(ui);
                ui.separator();
                self.algorithm(ui);
                ui.separator();
                self.flags(ui);
                ui.separator();

                if let Some(error) = &self.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    let button = ui.add_enabled(self.is_usable(), egui::Button::new("Save as…"));
                    if button.clicked() {
                        protect = true;
                    }
                    if !self.is_usable() {
                        ui.weak("A password is needed.");
                    } else {
                        ui.weak("The original file is not touched.");
                    }
                });
            });

        self.open = open;
        protect
    }

    fn passwords(&mut self, ui: &mut Ui) {
        egui::Grid::new("protect-passwords")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Open password");
                ui.add(
                    egui::TextEdit::singleline(&mut self.user_password)
                        .password(true)
                        .desired_width(240.0),
                )
                .on_hover_text("Needed to read the document at all");
                ui.end_row();

                ui.label("Owner password");
                ui.add(
                    egui::TextEdit::singleline(&mut self.owner_password)
                        .password(true)
                        .desired_width(240.0),
                )
                .on_hover_text("Needed to change the protection. Defaults to the open password");
                ui.end_row();
            });

        if self.user_password.is_empty() && !self.owner_password.is_empty() {
            ui.add_space(4.0);
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "With no open password anyone can read the file; only the flags below apply.",
            );
        }
    }

    fn algorithm(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label("Encryption");
            ui.selectable_value(&mut self.algorithm, Algorithm::Aes256, "AES-256");
            ui.selectable_value(&mut self.algorithm, Algorithm::Aes128, "AES-128");
        });
        ui.weak(match self.algorithm {
            Algorithm::Aes256 => "Strongest. Needs a reader from about 2017 or later.",
            Algorithm::Aes128 => "Readable by essentially every PDF reader.",
        });
    }

    fn flags(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Allow readers to").strong());

        ui.checkbox(&mut self.permissions.print, "Print");
        ui.add_enabled_ui(self.permissions.print, |ui| {
            ui.checkbox(
                &mut self.permissions.print_high_quality,
                "Print at full quality",
            );
        });
        ui.checkbox(&mut self.permissions.copy, "Copy text and graphics");
        ui.checkbox(&mut self.permissions.modify, "Edit the contents");
        ui.checkbox(&mut self.permissions.annotate, "Annotate");
        ui.checkbox(&mut self.permissions.fill_forms, "Fill in forms");
        ui.checkbox(&mut self.permissions.assemble, "Reorder or delete pages");

        if !self.permissions.is_unrestricted() {
            ui.add_space(4.0);
            // The single most important sentence in this dialog.
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "These flags are advisory. Conforming readers honour them; nothing enforces them.\n\
                 The open password is what actually protects the file.",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protecting_is_refused_until_there_is_a_password() {
        let mut dialog = ProtectDialog::default();
        assert!(!dialog.is_usable());

        dialog.owner_password = "owner".to_string();
        assert!(dialog.is_usable(), "an owner password alone is a choice");
    }

    #[test]
    fn the_default_takes_no_permission_away() {
        assert!(ProtectDialog::default().permissions.is_unrestricted());
    }

    #[test]
    fn the_settings_carry_what_was_typed() {
        let mut dialog = ProtectDialog {
            user_password: "open-me".into(),
            algorithm: Algorithm::Aes128,
            ..ProtectDialog::default()
        };
        dialog.permissions.copy = false;

        let settings = dialog.settings();
        assert_eq!(settings.user_password, "open-me");
        assert!(!settings.permissions.copy);
        assert_eq!(settings.algorithm, Algorithm::Aes128);
    }

    #[test]
    fn a_rejected_password_is_cleared_rather_than_left_to_be_retyped() {
        let mut prompt = PasswordPrompt {
            open: false,
            password: "wrong".into(),
            was_wrong: false,
        };
        prompt.ask(true);

        assert!(prompt.open);
        assert!(prompt.was_wrong);
        assert!(prompt.password.is_empty());
    }

    #[test]
    fn a_first_prompt_keeps_whatever_was_typed() {
        let mut prompt = PasswordPrompt {
            open: false,
            password: "half-typed".into(),
            was_wrong: false,
        };
        prompt.ask(false);
        assert_eq!(prompt.password, "half-typed");
    }
}
