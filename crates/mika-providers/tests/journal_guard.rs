//! The guard of the journal for this crate: no record writes a text of the
//! user or of the model as it is, exposes a secret or dumps a whole request
//! (`mika_core::journal::lint`).

use std::path::Path;

#[test]
fn the_records_of_this_crate_carry_no_content() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = mika_core::journal::lint::check_crate(&src);
    assert!(found.is_empty(), "\n{}", found.join("\n"));
}
