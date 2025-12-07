use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct KeyAndSkip {
    #[rumps(key, skip)]
    id: u64,
    name: String,
}

fn main() {}
