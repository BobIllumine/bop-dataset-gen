//! The committed `OpenAPI` document matches the routes the server serves.
//!
//! Regenerate it with `BOP_BLESS=1 cargo test -p bop-server --test openapi`.

use std::path::Path;

#[test]
fn committed_openapi_document_matches_the_router() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/api.openapi.json");
    let generated = format!("{}\n", bop_server::api::openapi().to_pretty_json().unwrap());
    if std::env::var_os("BOP_BLESS").is_some() {
        std::fs::write(&committed, &generated).unwrap();
        return;
    }
    let current = std::fs::read_to_string(&committed).unwrap_or_default();
    assert!(
        current == generated,
        "{} is out of date; run `BOP_BLESS=1 cargo test -p bop-server --test openapi`",
        committed.display()
    );
}
