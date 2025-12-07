use rumps_storage::orm::ToRumps;

#[derive(ToRumps)]
#[rumps(global = "test")]
struct SkipAndSubtree {
    #[rumps(key)]
    id: u64,
    #[rumps(skip, subtree)]
    data: String,
}

fn main() {}
