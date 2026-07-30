//! Build script — guarantee the SPA embed folder exists so `cargo build`
//! succeeds even when the frontend hasn't been built yet.
//!
//! `rust-embed`'s derive (`#[folder = "../web/dist"]`, see `src/spa.rs`) reads
//! the folder at compile time and errors if it is missing. The real contents
//! come from `cd src/web && npm run build`, but the backend must build
//! independently (CI, first checkout, tests). So here we ensure `../web/dist`
//! exists and holds at least a minimal `index.html` placeholder.
//!
//! If the real Vite build has already produced `index.html`, we leave it
//! untouched — we only write the placeholder when it is absent.

use std::fs;
use std::path::Path;

fn main() {
    // Relative to this crate root (src/server) → sibling src/web/dist.
    let dist = Path::new("../web/dist");
    let index = dist.join("index.html");

    if let Err(e) = fs::create_dir_all(dist) {
        // Don't hard-fail the build over a placeholder; surface a warning.
        println!("cargo:warning=could not create {}: {e}", dist.display());
        return;
    }

    if !index.exists() {
        let placeholder = "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head><meta charset=\"UTF-8\"><title>Spec ADE</title></head>\n\
<body>\n\
  <p>Spec ADE frontend not built. Run <code>cd src/web &amp;&amp; npm install &amp;&amp; npm run build</code>.</p>\n\
</body>\n\
</html>\n";
        if let Err(e) = fs::write(&index, placeholder) {
            println!(
                "cargo:warning=could not write placeholder {}: {e}",
                index.display()
            );
        }
    }

    // Rebuild if the embedded dist changes.
    println!("cargo:rerun-if-changed=../web/dist");
}
