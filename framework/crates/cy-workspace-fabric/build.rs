//! Rebuild embedded SQLx migrations whenever the migration directory changes.

fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
