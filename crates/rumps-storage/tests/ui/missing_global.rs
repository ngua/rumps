use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
struct MissingGlobal {
    #[rumps(key)]
    id: u64,
    name: String,
}

fn main() {}
