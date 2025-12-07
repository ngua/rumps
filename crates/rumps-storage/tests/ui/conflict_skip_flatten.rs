use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct SkipAndFlatten {
    #[rumps(key)]
    id: u64,
    #[rumps(skip, flatten)]
    data: String,
}

fn main() {}
