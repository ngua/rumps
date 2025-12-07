use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct KeyAndFlatten {
    #[rumps(key, flatten)]
    id: u64,
    name: String,
}

fn main() {}
