use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct FlattenAndSubtree {
    #[rumps(key)]
    id: u64,
    #[rumps(flatten, subtree)]
    data: String,
}

fn main() {}
