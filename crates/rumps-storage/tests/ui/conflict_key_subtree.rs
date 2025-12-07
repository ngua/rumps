use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct KeyAndSubtree {
    #[rumps(key, subtree)]
    id: u64,
    name: String,
}

fn main() {}
