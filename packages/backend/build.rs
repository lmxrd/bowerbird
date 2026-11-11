// sqlx::migrate! embeds the migrations folder; without this a new
// migration file would not trigger a rebuild.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
