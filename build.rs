// Bez tohohle cargo po přidání nového souboru do `migrations/` nic nepřekompiluje
// (proc-makro `sqlx::migrate!` sleduje jen soubory, které už při buildu existovaly),
// a aplikace by novou migraci při startu nepustila.
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
