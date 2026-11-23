//! The package rule: the Daml packages a transaction may use.

use std::collections::BTreeSet;

use super::Rules;
use crate::canon::values::PackageId;

/// Ok when `rules` allow every package in `used`; else the first one they do not, ascending.
pub fn check_packages(rules: &Rules, used: &BTreeSet<PackageId>) -> Result<(), PackageId> {
    match used.iter().find(|id| !rules.packages.allows(id)) {
        Some(refused) => Err(*refused),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Allowlist, DEFAULT_LINKED_RULES};

    fn id(byte: u8) -> PackageId {
        PackageId([byte; 32])
    }

    fn used(bytes: &[u8]) -> BTreeSet<PackageId> {
        bytes.iter().map(|b| id(*b)).collect()
    }

    #[test]
    fn any_allows_every_package() {
        let rules = DEFAULT_LINKED_RULES;
        assert!(rules.packages.any);
        assert_eq!(check_packages(&rules, &used(&[1, 2, 0xff])), Ok(()));
        assert_eq!(check_packages(&rules, &BTreeSet::new()), Ok(()));
    }

    #[test]
    fn selected_allows_only_listed_ids_and_names_the_first_refused() {
        let mut rules = DEFAULT_LINKED_RULES;
        rules.packages = Allowlist {
            any: false,
            items: vec![id(2), id(4)],
        };
        assert_eq!(check_packages(&rules, &used(&[2])), Ok(()));
        assert_eq!(check_packages(&rules, &used(&[2, 4])), Ok(()));
        assert_eq!(check_packages(&rules, &BTreeSet::new()), Ok(()));
        assert_eq!(check_packages(&rules, &used(&[2, 3, 4])), Err(id(3)));
        assert_eq!(check_packages(&rules, &used(&[9, 1, 4])), Err(id(1)));
        rules.packages.items.clear();
        assert_eq!(check_packages(&rules, &used(&[2])), Err(id(2)));
    }
}
