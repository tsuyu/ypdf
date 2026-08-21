//! What a reader is permitted to do with a document (spec §8).
//!
//! **These flags are advisory.** They are stored inside the encrypted document
//! and every conforming reader honours them, but nothing enforces them: a
//! reader that ignores them is not breaking the file, only the convention. What
//! actually protects a document is the encryption — a user password means the
//! bytes cannot be read at all without it.
//!
//! Saying so plainly matters more than it looks. Someone who believes "cannot
//! copy" is enforced will put a secret in a document and send it to a stranger.

use lopdf::Permissions as Bits;

/// The permission flags, as booleans rather than bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions {
    /// Print the document.
    pub print: bool,
    /// Print at full quality rather than a degraded raster.
    pub print_high_quality: bool,
    /// Change the contents.
    pub modify: bool,
    /// Copy text and graphics out.
    pub copy: bool,
    /// Add or change annotations.
    pub annotate: bool,
    /// Fill in existing form fields.
    pub fill_forms: bool,
    /// Insert, rotate, or delete pages and edit the outline.
    pub assemble: bool,
}

impl Default for Permissions {
    /// Everything allowed. Encrypting a document should not quietly take
    /// abilities away that nobody asked to remove.
    fn default() -> Self {
        Self::all()
    }
}

impl Permissions {
    /// Everything allowed.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            print: true,
            print_high_quality: true,
            modify: true,
            copy: true,
            annotate: true,
            fill_forms: true,
            assemble: true,
        }
    }

    /// Nothing allowed beyond reading and accessibility extraction.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            print: false,
            print_high_quality: false,
            modify: false,
            copy: false,
            annotate: false,
            fill_forms: false,
            assemble: false,
        }
    }

    /// The common case: read and print, nothing else.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            print: true,
            print_high_quality: true,
            ..Self::none()
        }
    }

    /// Are all of them set?
    #[must_use]
    pub fn is_unrestricted(self) -> bool {
        self == Self::all()
    }

    /// The ones that are switched off, for a report.
    #[must_use]
    pub fn restrictions(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.print {
            out.push("printing");
        } else if !self.print_high_quality {
            out.push("high-quality printing");
        }
        if !self.modify {
            out.push("editing");
        }
        if !self.copy {
            out.push("copying text");
        }
        if !self.annotate {
            out.push("annotating");
        }
        if !self.fill_forms {
            out.push("filling forms");
        }
        if !self.assemble {
            out.push("reordering pages");
        }
        out
    }

    /// Convert to the bit flags a PDF stores.
    ///
    /// Extraction for accessibility is always allowed. PDF 2.0 requires the bit
    /// to be set for backward compatibility, and a document a screen reader
    /// cannot read is not a document that has been protected — it is one that
    /// has been made unusable for some of its readers.
    #[must_use]
    pub fn to_bits(self) -> Bits {
        let mut bits = Bits::COPYABLE_FOR_ACCESSIBILITY;
        if self.print {
            bits |= Bits::PRINTABLE;
        }
        if self.print_high_quality {
            bits |= Bits::PRINTABLE_IN_HIGH_QUALITY;
        }
        if self.modify {
            bits |= Bits::MODIFIABLE;
        }
        if self.copy {
            bits |= Bits::COPYABLE;
        }
        if self.annotate {
            bits |= Bits::ANNOTABLE;
        }
        if self.fill_forms {
            bits |= Bits::FILLABLE;
        }
        if self.assemble {
            bits |= Bits::ASSEMBLABLE;
        }
        bits
    }

    /// Read the flags out of a document's `/P` value.
    #[must_use]
    pub fn from_bits(bits: Bits) -> Self {
        Self {
            print: bits.contains(Bits::PRINTABLE),
            print_high_quality: bits.contains(Bits::PRINTABLE_IN_HIGH_QUALITY),
            modify: bits.contains(Bits::MODIFIABLE),
            copy: bits.contains(Bits::COPYABLE),
            annotate: bits.contains(Bits::ANNOTABLE),
            fill_forms: bits.contains(Bits::FILLABLE),
            assemble: bits.contains(Bits::ASSEMBLABLE),
        }
    }

    /// Read the flags out of a raw `/P` integer.
    #[must_use]
    pub fn from_raw(value: i64) -> Self {
        #[expect(
            clippy::cast_sign_loss,
            reason = "/P is a signed integer holding a bit field"
        )]
        let bits = Bits::from_bits_retain(value as u64);
        Self::from_bits(bits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_through_the_bits_keeps_every_flag() {
        for permissions in [
            Permissions::all(),
            Permissions::none(),
            Permissions::read_only(),
            Permissions {
                copy: false,
                ..Permissions::all()
            },
        ] {
            assert_eq!(
                Permissions::from_bits(permissions.to_bits()),
                permissions,
                "{permissions:?} did not survive the round trip"
            );
        }
    }

    #[test]
    fn accessibility_extraction_is_always_allowed() {
        // Even with everything else switched off: a document a screen reader
        // cannot read has not been protected, it has been broken.
        let bits = Permissions::none().to_bits();
        assert!(bits.contains(Bits::COPYABLE_FOR_ACCESSIBILITY));
    }

    #[test]
    fn restrictions_are_listed_in_words_not_bits() {
        let permissions = Permissions {
            copy: false,
            modify: false,
            ..Permissions::all()
        };
        let restrictions = permissions.restrictions();
        assert!(restrictions.contains(&"copying text"));
        assert!(restrictions.contains(&"editing"));
        assert_eq!(restrictions.len(), 2);
    }

    #[test]
    fn no_print_at_all_is_reported_once_rather_than_twice() {
        // "printing, high-quality printing" reads as two separate losses.
        let permissions = Permissions {
            print: false,
            print_high_quality: false,
            ..Permissions::all()
        };
        assert_eq!(permissions.restrictions(), vec!["printing"]);
    }

    #[test]
    fn the_default_takes_nothing_away() {
        assert!(Permissions::default().is_unrestricted());
        assert!(Permissions::default().restrictions().is_empty());
    }
}
