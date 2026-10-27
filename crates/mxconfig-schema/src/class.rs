//! Namespace classes, derived from a reserved label in the id.

use crate::error::{Error, Kind};

/// The class of a namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Class {
    /// Private to one app.
    App,
    /// Shared by the desktop's apps that declare it.
    Desktop,
    /// Shared, but only behind a barrier.
    Privileged,
}

/// The reserved label marking a desktop namespace.
const DESKTOP: &str = "desktop";
/// The reserved label marking a privileged namespace.
const PRIVILEGED: &str = "privileged";

impl Class {
    /// The class of `namespace`. A malformed id is an error.
    pub fn of(namespace: &str) -> Result<Class, Error> {
        let labels: Vec<&str> = namespace.split('.').collect();
        let bad = || Error::from(Kind::Namespace(namespace.to_string()));
        if labels.len() < 2 || labels.iter().any(|label| !identifier(label, &['-', '_'])) {
            return Err(bad());
        }

        let mut class = Class::App;
        for (i, label) in labels.iter().enumerate() {
            let kind = match *label {
                DESKTOP => Class::Desktop,
                PRIVILEGED => Class::Privileged,
                _ => continue,
            };
            // A reserved label must not be first, last, or repeated.
            if i == 0 || i + 1 == labels.len() || class != Class::App {
                return Err(bad());
            }
            class = kind;
        }
        Ok(class)
    }

    /// Whether `namespace` is a valid app namespace.
    pub fn is_app(namespace: &str) -> bool {
        Class::of(namespace).is_ok_and(|class| class == Class::App)
    }
}

impl std::fmt::Display for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Class::App => "app",
            Class::Desktop => DESKTOP,
            Class::Privileged => PRIVILEGED,
        })
    }
}

/// Whether `name` is ASCII alphanumerics and `extra`, not starting with a digit.
pub(crate) fn identifier(name: &str, extra: &[char]) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
}
