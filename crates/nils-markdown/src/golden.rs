//! Byte-equality assertion harness for `.md.tera` fixtures.
//!
//! Each Tier A migration captures pre-migration output to a
//! fixture file before changing the function body, then the same
//! PR adds an [`assert_render`] call that re-renders the
//! migrated path and asserts byte-for-byte equality against that
//! fixture using [`pretty_assertions`]. The fixture is the
//! source of truth; any drift surfaces immediately as a diff.

use std::path::Path;

use serde::Serialize;

use crate::Engine;

/// Render `template_name` against `view` through `engine` and
/// assert the result is byte-equal to the contents of
/// `fixture_path`. Panics with a `pretty_assertions` diff on
/// mismatch.
///
/// The template must already be registered on `engine` before
/// calling this helper; consumers typically do that in a
/// per-template setup helper and reuse it across positive cases.
pub fn assert_render<T, P>(fixture_path: P, engine: &mut Engine, template_name: &str, view: &T)
where
    T: Serialize,
    P: AsRef<Path>,
{
    let path = fixture_path.as_ref();
    let expected = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read golden fixture {}: {e}", path.display()));
    let actual = engine
        .render(template_name, view)
        .unwrap_or_else(|e| panic!("render template `{template_name}`: {e}"));
    pretty_assertions::assert_eq!(expected, actual, "golden mismatch at {}", path.display());
}
